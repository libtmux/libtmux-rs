//! Deciding how many tmux invocations a plan costs.
//!
//! A planner is policy and nothing else: it reads recorded operations and
//! returns the groups they dispatch in. Changing the planner changes the
//! number of processes, never the result, which is what makes the choice
//! measurable rather than a matter of taste.

use std::collections::BTreeSet;

use super::{Op, Part, Plan};

/// One tmux invocation: the operations it carries, in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Step {
    indices: Vec<usize>,
    marked: bool,
}

impl Step {
    /// The plan indices this invocation runs, in order.
    #[must_use]
    pub fn indices(&self) -> &[usize] {
        &self.indices
    }

    /// Whether this invocation addresses a new pane through tmux's `{marked}`
    /// register, which is what lets a create share an invocation with the
    /// operations that decorate it.
    #[must_use]
    pub const fn is_marked(&self) -> bool {
        self.marked
    }

    /// How many operations share this invocation.
    #[must_use]
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    /// Whether this invocation carries no operations, which a planner never
    /// produces.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Why this invocation is separate from the one before it.
    #[must_use]
    pub fn reason(&self, plan: &Plan) -> StepReason {
        if self.marked {
            return StepReason::MarkedFold;
        }
        if self.indices.len() > 1 {
            return StepReason::Folded;
        }
        let Some(op) = self
            .indices
            .first()
            .and_then(|index| plan.steps().get(*index))
        else {
            return StepReason::Alone;
        };
        if op.effects().creates.is_some() {
            StepReason::CreatesId
        } else if op.effects().reads_output {
            StepReason::ReadsOutput
        } else {
            StepReason::Alone
        }
    }
}

/// Why a [`Step`] is its own tmux invocation.
///
/// The answer a caller wants from [`Planner::explain`] is usually "why is this
/// not cheaper", so the reasons name the obstacle rather than the outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StepReason {
    /// A pane creation sharing its invocation with what decorates it.
    MarkedFold,
    /// Several operations sharing one invocation.
    Folded,
    /// Alone because it prints an id a later operation needs.
    CreatesId,
    /// Alone because its output is its answer, and a shared invocation returns
    /// one merged stdout with no per-command boundary.
    ReadsOutput,
    /// Alone because it had nothing to share with.
    Alone,
}

/// How a plan is grouped into tmux invocations.
///
/// # Examples
///
/// ```
/// use libtmux::plan::{Plan, Planner, SendKeys, SplitWindow};
/// use libtmux::WindowId;
///
/// let window: WindowId = "@1".parse()?;
/// let mut plan = Plan::new();
/// let pane = plan.add(SplitWindow::new(window).focus());
/// plan.add(SendKeys::new(pane).text("vim").enter());
/// plan.add(SendKeys::new(pane).text(":e .").enter());
///
/// // The same three operations, grouped three ways.
/// assert_eq!(Planner::Sequential.steps(&plan).len(), 3);
/// assert_eq!(Planner::Folding.steps(&plan).len(), 2);
/// assert_eq!(Planner::Marked.steps(&plan).len(), 1);
/// # Ok::<(), libtmux::IdParseError>(())
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Planner {
    /// One tmux invocation per operation.
    ///
    /// The most processes and the most evidence: every operation has its own
    /// exit status, so a failure names itself.
    #[default]
    Sequential,
    /// Share one invocation between neighbouring operations that can.
    Folding,
    /// Also share a pane creation with the operations that decorate it.
    Marked,
}

impl Planner {
    /// Group a plan into the invocations it will dispatch as.
    #[must_use]
    pub fn steps(&self, plan: &Plan) -> Vec<Step> {
        self.steps_bounded(plan, &BTreeSet::new())
    }

    /// Group a plan, but never share an invocation across a boundary.
    ///
    /// A boundary is an index after which the caller must do something itself
    /// -- wait for a pane to be ready, run a hook -- so no group may span it.
    /// Splitting only ever breaks a group into contiguous runs, so it changes
    /// the number of invocations and not the result.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeSet;
    /// use libtmux::plan::{Plan, Planner, SendKeys};
    /// use libtmux::PaneId;
    ///
    /// let pane: PaneId = "%1".parse()?;
    /// let mut plan = Plan::new();
    /// plan.add(SendKeys::new(pane.clone()).text("one").enter());
    /// plan.add(SendKeys::new(pane).text("two").enter());
    ///
    /// assert_eq!(Planner::Folding.steps(&plan).len(), 1);
    /// let after_first = BTreeSet::from([0]);
    /// assert_eq!(Planner::Folding.steps_bounded(&plan, &after_first).len(), 2);
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    #[must_use]
    pub fn steps_bounded(&self, plan: &Plan, boundaries: &BTreeSet<usize>) -> Vec<Step> {
        let grouped = match self {
            Self::Sequential => (0..plan.len())
                .map(|index| Step {
                    indices: vec![index],
                    marked: false,
                })
                .collect(),
            Self::Folding => fold(plan.steps(), false),
            Self::Marked => fold(plan.steps(), true),
        };

        grouped
            .into_iter()
            .flat_map(|step| split_at_boundaries(step, boundaries))
            .collect()
    }

    /// Group a plan and say why each invocation is separate.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::plan::{CapturePane, Plan, Planner, StepReason};
    /// use libtmux::PaneId;
    ///
    /// let pane: PaneId = "%1".parse()?;
    /// let mut plan = Plan::new();
    /// plan.add(CapturePane::new(pane));
    ///
    /// let explained = Planner::Folding.explain(&plan);
    /// assert_eq!(explained[0].1, StepReason::ReadsOutput);
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    #[must_use]
    pub fn explain(&self, plan: &Plan) -> Vec<(Step, StepReason)> {
        self.steps(plan)
            .into_iter()
            .map(|step| {
                let reason = step.reason(plan);
                (step, reason)
            })
            .collect()
    }
}

/// Group neighbouring operations that may share an invocation.
fn fold(ops: &[Op], marked: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut index = 0;
    while index < ops.len() {
        if marked {
            let decorates = marked_decorates(ops, index);
            if !decorates.is_empty() {
                let end = decorates[decorates.len() - 1];
                steps.push(Step {
                    indices: (index..=end).collect(),
                    marked: true,
                });
                index = end + 1;
                continue;
            }
        }
        if ops[index].is_chainable() {
            let mut cursor = index;
            while cursor < ops.len() && ops[cursor].is_chainable() {
                cursor += 1;
            }
            steps.push(Step {
                indices: (index..cursor).collect(),
                marked: false,
            });
            index = cursor;
        } else {
            steps.push(Step {
                indices: vec![index],
                marked: false,
            });
            index += 1;
        }
    }
    steps
}

/// The operations that decorate a pane created at `index`, if it can fold.
///
/// Empty unless `index` creates a pane *and leaves it active*, and is followed
/// by operations that address only that pane. The focus requirement is not
/// cosmetic: the fold marks the active pane, so a detached creation would mark
/// whichever pane was already active and send the decorations there.
fn marked_decorates(ops: &[Op], index: usize) -> Vec<usize> {
    let Some(creation) = ops.get(index) else {
        return Vec::new();
    };
    let Some(focused) = creation.focused_pane() else {
        return Vec::new();
    };

    let mut decorates = Vec::new();
    for (offset, op) in ops.iter().enumerate().skip(index + 1) {
        if !op.is_chainable() {
            break;
        }
        // Only operations addressing the pane this creation left active may
        // fold: any other target would be sent to the marked pane instead.
        let named: Vec<(usize, Part)> = op.slots().iter().flatten().copied().collect();
        if named.is_empty()
            || !named
                .iter()
                .all(|(slot, part)| *slot == index && *part == focused)
        {
            break;
        }
        decorates.push(offset);
    }
    decorates
}

/// Break a group wherever a boundary falls inside it.
fn split_at_boundaries(step: Step, boundaries: &BTreeSet<usize>) -> Vec<Step> {
    if step.indices.len() < 2 {
        return vec![step];
    }
    let cuts: Vec<usize> = (0..step.indices.len() - 1)
        .filter(|position| boundaries.contains(&step.indices[*position]))
        .map(|position| position + 1)
        .collect();
    if cuts.is_empty() {
        return vec![step];
    }

    let mut runs = Vec::new();
    let mut start = 0;
    for cut in cuts.into_iter().chain(std::iter::once(step.indices.len())) {
        // A marked fold keeps its register only while the creation still has
        // something to decorate; a later run addresses the bound id instead.
        runs.push(Step {
            indices: step.indices[start..cut].to_vec(),
            marked: step.marked && start == 0 && cut - start > 1,
        });
        start = cut;
    }
    runs
}

impl Step {
    /// One invocation carrying one operation.
    pub(crate) fn single(index: usize) -> Self {
        Self {
            indices: vec![index],
            marked: false,
        }
    }
}
