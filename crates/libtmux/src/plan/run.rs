//! Running a plan, and saying honestly how each operation ended.
//!
//! What a run can claim depends on how it dispatched. One invocation per
//! operation gives one exit status per operation. A shared invocation gives
//! one exit status for the group, and tmux runs a shared group up to the first
//! failure and drops the rest -- so a failed group says *that* something
//! failed and never *which*. This module reports that difference rather than
//! guessing past it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::str::FromStr as _;

use super::planner::Planner;
use super::{Op, OperationKind, Part, Plan, Scope, Step};
use crate::error::ListingDecodeError;
use crate::formats::FormatCodecError;
use crate::{
    Command, CommandChain, Error, IdParseError, PaneId, Server, SessionId, TmuxText, WindowId,
};

/// How an operation ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// tmux ran it and accepted it.
    Complete,
    /// tmux ran it and refused it.
    Failed,
    /// tmux never ran it, because something before it in the same invocation
    /// failed and tmux dropped the rest.
    Skipped,
    /// It shared an invocation with a failure and nothing distinguishes it.
    ///
    /// This is not a soft failure, it is the absence of evidence: the merged
    /// result carries one exit status and one stderr whichever member failed.
    /// Re-run with [`Planner::Sequential`] to get an answer per operation.
    Unknown,
}

impl Outcome {
    /// Whether tmux is known to have accepted this operation.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// How much a run could tell about each operation's outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Attribution {
    /// Each operation had its own exit status.
    PerCommand,
    /// Operations shared an exit status, so a failure is not attributable.
    Merged,
}

/// The typed value one operation produced.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OperationValue {
    /// tmux accepted an operation that has no other return value.
    Acknowledged,
    /// A session and the first window and pane it created.
    CreatedSession {
        /// The created session.
        session: SessionId,
        /// The session's first window.
        window: WindowId,
        /// The first window's pane.
        pane: PaneId,
    },
    /// A window and its first pane.
    CreatedWindow {
        /// The created window.
        window: WindowId,
        /// The window's first pane.
        pane: PaneId,
    },
    /// A pane created by splitting another pane.
    CreatedPane {
        /// The created pane.
        pane: PaneId,
    },
    /// Pane contents, exactly as tmux wrote them.
    CapturedPane(TmuxText),
}

/// What one recorded operation produced, in plan order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationReport {
    index: usize,
    kind: OperationKind,
    outcome: Outcome,
    attribution: Option<Attribution>,
    value: Option<OperationValue>,
}

impl OperationReport {
    /// The operation's zero-based index in the plan.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Which recorded operation this report describes.
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.kind
    }

    /// How the operation ended.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// How directly the dispatch evidence applies to this operation.
    ///
    /// `None` means the plan stopped before this operation's invocation.
    #[must_use]
    pub const fn attribution(&self) -> Option<Attribution> {
        self.attribution
    }

    /// The operation's typed value, when the run established one.
    #[must_use]
    pub const fn value(&self) -> Option<&OperationValue> {
        self.value.as_ref()
    }
}

/// What one dispatched invocation produced.
#[derive(Clone)]
pub struct StepOutcome {
    step: Step,
    outcomes: Vec<Outcome>,
    attribution: Attribution,
    command: &'static str,
    sensitive_input: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl fmt::Debug for StepOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StepOutcome")
            .field("step", &self.step)
            .field("outcomes", &self.outcomes)
            .field("attribution", &self.attribution)
            .field("command", &self.command)
            .field("sensitive_input", &self.sensitive_input)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .finish()
    }
}

impl StepOutcome {
    /// The invocation this describes.
    #[must_use]
    pub const fn step(&self) -> &Step {
        &self.step
    }

    /// One outcome per operation in the invocation, in order.
    #[must_use]
    pub fn outcomes(&self) -> &[Outcome] {
        &self.outcomes
    }

    /// Whether the outcomes are per-operation evidence or a shared verdict.
    #[must_use]
    pub const fn attribution(&self) -> Attribution {
        self.attribution
    }

    /// The invocation's stdout, exactly as tmux wrote it.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// The invocation's stderr, exactly as tmux wrote it.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Whether any command in this invocation carried a sensitive argument.
    #[must_use]
    pub const fn has_sensitive_input(&self) -> bool {
        self.sensitive_input
    }

    /// Why tmux refused this invocation, in the crate's error vocabulary.
    ///
    /// A refusal is data rather than an error here, because a plan may expect
    /// one. Without sensitive input, this classifies it the same way a direct
    /// call would, so a caller can match on [`Error::SessionExists`] rather
    /// than racing another session creation. A sensitive invocation reports
    /// [`Error::CommandFailed`] and withholds tmux output.
    ///
    /// `None` when the invocation succeeded.
    #[must_use]
    pub fn refusal(&self) -> Option<Error> {
        if self.outcomes.iter().copied().all(Outcome::is_complete) {
            return None;
        }

        Some(if self.sensitive_input {
            Error::refused_withheld(self.command, None)
        } else {
            Error::refused(
                self.command,
                None,
                String::from_utf8_lossy(&self.stderr).into_owned(),
                None,
            )
        })
    }
}

/// What running a plan produced.
#[derive(Clone)]
pub struct PlanResult {
    operations: Vec<OperationReport>,
    steps: Vec<StepOutcome>,
    bound: HashMap<(usize, Part), OsString>,
    dispatches: usize,
}

impl fmt::Debug for PlanResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlanResult")
            .field("operations", &self.operations)
            .field("steps", &self.steps)
            .field("binding_count", &self.bound.len())
            .field("dispatches", &self.dispatches)
            .finish()
    }
}

impl PlanResult {
    /// One report per recorded operation, in plan order.
    #[must_use]
    pub fn operations(&self) -> &[OperationReport] {
        &self.operations
    }

    /// What each dispatched invocation produced.
    #[must_use]
    pub fn steps(&self) -> &[StepOutcome] {
        &self.steps
    }

    /// How many tmux invocations the run cost.
    ///
    /// This is the number the planner changes, and the reason to change it.
    #[must_use]
    pub const fn dispatches(&self) -> usize {
        self.dispatches
    }

    /// Whether every operation is known to have succeeded.
    ///
    /// An [`Outcome::Unknown`] is not success: it is the absence of evidence,
    /// so this is false while any remains.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.operations
            .iter()
            .all(|report| report.outcome().is_complete())
    }

    /// The concrete id a creating operation produced, if it bound one.
    #[must_use]
    pub fn created(&self, step: usize) -> Option<&OsString> {
        self.bound.get(&(step, Part::Created))
    }
}

impl Plan {
    /// Run this plan, grouping it with `planner`.
    ///
    /// The result does not depend on the planner; the number of tmux
    /// invocations, and how precisely a failure can be attributed, do.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached, a process cannot be
    /// captured, a slot dependency is invalid, or a creating operation does
    /// not return valid IDs. Validation happens before the first command. A
    /// command tmux *refuses* is reported through the returned [`PlanResult`],
    /// not as an error, because a plan may expect one.
    pub async fn run(&self, server: &Server, planner: Planner) -> Result<PlanResult, Error> {
        self.validate()
            .map_err(|source| Error::InvalidPlan { source })?;
        let steps = planner.steps(self);
        let mut bound: HashMap<(usize, Part), OsString> = HashMap::new();
        let mut outcomes = vec![Outcome::Skipped; self.len()];
        let mut reported = Vec::with_capacity(steps.len());
        let mut dispatches = 0;
        let mut effect_seen = false;

        for step in steps {
            let (result, marked_creation) = self
                .dispatch(server, &step, &bound)
                .await
                .map_err(|error| after_plan_effect(error, effect_seen))?;
            dispatches += 1;

            let succeeded = result.success();
            let step_committed = (succeeded
                && step.indices().iter().any(|index| {
                    self.steps()
                        .get(*index)
                        .is_some_and(|op| !op.effects().read_only)
                }))
                || (marked_creation && !result.stdout().is_empty());
            let effect_through_step = effect_seen || step_committed;
            let step_outcomes = attribute(step.indices().len(), succeeded);
            for (position, index) in step.indices().iter().enumerate() {
                outcomes[*index] = step_outcomes[position];
            }
            if succeeded || (marked_creation && !result.stdout().is_empty()) {
                bind(&mut bound, self.steps(), &step, result.stdout())
                    .map_err(|error| after_plan_effect(error, effect_through_step))?;
            }

            reported.push(StepOutcome {
                attribution: if step.indices().len() == 1 {
                    Attribution::PerCommand
                } else {
                    Attribution::Merged
                },
                command: step
                    .indices()
                    .first()
                    .and_then(|index| self.steps().get(*index))
                    .map_or("plan", Op::name),
                sensitive_input: result.command().sensitive_argument_count() > 0,
                step,
                outcomes: step_outcomes,
                stdout: result.stdout().to_vec(),
                stderr: result.stderr().to_vec(),
            });

            if !succeeded {
                break;
            }
            effect_seen = effect_through_step;
        }

        let operations = operation_reports(self.steps(), &outcomes, &reported, &bound);
        Ok(PlanResult {
            operations,
            steps: reported,
            bound,
            dispatches,
        })
    }

    /// Send one invocation, sharing it when the step carries several.
    async fn dispatch(
        &self,
        server: &Server,
        step: &Step,
        bound: &HashMap<(usize, Part), OsString>,
    ) -> Result<(crate::CommandResult, bool), Error> {
        let commands = self.render_step(step, bound)?;
        let marked = step.is_marked();
        let mut commands = commands.into_iter();
        let Some(first) = commands.next() else {
            return Err(Error::CommandFailed {
                command: "plan",
                exit_code: None,
                stderr: String::from("a plan step carried no commands"),
            });
        };

        let result = match commands.next() {
            None => server.cmd(first).await?,
            Some(second) => {
                let mut chain = CommandChain::new(first).then(second);
                for command in commands {
                    chain = chain.then(command);
                }
                server.chain(chain).await?
            }
        };
        Ok((result, marked))
    }

    /// Lower one invocation's operations into commands.
    fn render_step(
        &self,
        step: &Step,
        bound: &HashMap<(usize, Part), OsString>,
    ) -> Result<Vec<Command>, Error> {
        // In a marked fold the decorations address a pane that has no id yet,
        // so they resolve to tmux's `{marked}` register instead of to a bound
        // value. Which slot part names that pane is the creating operation's
        // answer, and it is the same one the planner folded on: `Created` for
        // a split, the created window's `FirstPane` for a new window.
        let marked = step
            .is_marked()
            .then(|| step.indices()[0])
            .and_then(|index| {
                self.steps()
                    .get(index)
                    .and_then(Op::focused_pane)
                    .map(|part| (index, part))
            });
        let resolve = |slot: usize, part: Part| -> Option<OsString> {
            if marked == Some((slot, part)) {
                return Some(OsString::from("{marked}"));
            }
            bound.get(&(slot, part)).cloned()
        };

        let mut commands = Vec::with_capacity(step.len() + 2);
        for (position, index) in step.indices().iter().enumerate() {
            let op = &self.steps()[*index];
            let command = op
                .render(&resolve, ())
                .ok_or_else(|| Error::CommandFailed {
                    command: op.name(),
                    exit_code: None,
                    stderr: format!(
                        "step {index} targets an object no earlier step created; \
                     a plan cannot address what it has not made"
                    ),
                })?;
            commands.push(command);
            // Mark the new pane straight after creating it, so the
            // decorations that follow have a register to address.
            if step.is_marked() && position == 0 {
                commands.push(Command::new("select-pane").arg("-m"));
            }
        }
        if step.is_marked() {
            commands.push(Command::new("select-pane").arg("-M"));
        }
        Ok(commands)
    }
}

/// Decide each operation's outcome from one invocation's exit status.
///
/// A shared invocation that succeeded proves every member ran, so every member
/// is `Complete`. A shared invocation that failed proves only that one member
/// did: tmux reports the same status and stderr whichever it was, so blaming
/// the first would be a guess that is wrong whenever the failure was later.
fn attribute(members: usize, succeeded: bool) -> Vec<Outcome> {
    if succeeded {
        return vec![Outcome::Complete; members];
    }
    if members == 1 {
        return vec![Outcome::Failed];
    }
    vec![Outcome::Unknown; members]
}

const SESSION_BINDINGS: &[(Part, Scope)] = &[
    (Part::Created, Scope::Session),
    (Part::FirstWindow, Scope::Window),
    (Part::FirstPane, Scope::Pane),
];
const WINDOW_BINDINGS: &[(Part, Scope)] = &[
    (Part::Created, Scope::Window),
    (Part::FirstPane, Scope::Pane),
];
const PANE_BINDINGS: &[(Part, Scope)] = &[(Part::Created, Scope::Pane)];

/// Record the ids a creating operation printed.
fn bind(
    bound: &mut HashMap<(usize, Part), OsString>,
    ops: &[Op],
    step: &Step,
    stdout: &[u8],
) -> Result<(), Error> {
    let Some(index) = step.indices().first().copied() else {
        return Ok(());
    };
    let Some(op) = ops.get(index) else {
        return Ok(());
    };
    let expected = match op.effects().creates {
        Some(Scope::Session) => SESSION_BINDINGS,
        Some(Scope::Window) => WINDOW_BINDINGS,
        Some(Scope::Pane) => PANE_BINDINGS,
        None => return Ok(()),
    };

    // A creating operation prints its ids on the first line, most specific
    // last: `$1 @2 %3`. Reading them positionally is what makes a session's
    // first window and pane addressable without a second round trip.
    let Some(line_end) = stdout.iter().position(|byte| *byte == b'\n') else {
        let field = expected.len() - 1;
        return Err(binding_shape_error(
            op,
            field,
            expected.get(field).map(|(_, scope)| binding_format(*scope)),
            Some(stdout.len()),
        ));
    };
    let ids: Vec<&[u8]> = stdout[..line_end].split(|byte| *byte == b' ').collect();
    if ids.len() != expected.len() {
        let field = ids.len().min(expected.len());
        return Err(binding_shape_error(
            op,
            field,
            expected.get(field).map(|(_, scope)| binding_format(*scope)),
            None,
        ));
    }

    let mut decoded = Vec::with_capacity(expected.len());
    for (id, (part, scope)) in ids.into_iter().zip(expected) {
        decoded.push(((index, *part), decode_binding(id, *scope)?));
    }
    bound.extend(decoded);
    Ok(())
}

fn binding_shape_error(
    op: &Op,
    field: usize,
    field_name: Option<&'static str>,
    offset: Option<usize>,
) -> Error {
    Error::DecodeListing {
        list_command: op.name(),
        detail: ListingDecodeError::new(FormatCodecError::row_mismatch(
            0,
            Some(field),
            field_name,
            offset,
        )),
    }
}

fn binding_format(scope: Scope) -> &'static str {
    match scope {
        Scope::Session => "session_id",
        Scope::Window => "window_id",
        Scope::Pane => "pane_id",
    }
}

fn decode_binding(bytes: &[u8], scope: Scope) -> Result<OsString, Error> {
    let (format, sigil) = match scope {
        Scope::Session => ("#{session_id}", '$'),
        Scope::Window => ("#{window_id}", '@'),
        Scope::Pane => ("#{pane_id}", '%'),
    };
    let text = std::str::from_utf8(bytes).map_err(|_| Error::UnreadableFormatValue {
        format,
        detail: IdParseError::new(sigil),
    })?;
    let invalid = |detail| Error::UnreadableFormatValue { format, detail };
    let rendered = match scope {
        Scope::Session => SessionId::from_str(text).map_err(invalid)?.to_string(),
        Scope::Window => WindowId::from_str(text).map_err(invalid)?.to_string(),
        Scope::Pane => PaneId::from_str(text).map_err(invalid)?.to_string(),
    };
    Ok(OsString::from(rendered))
}

/// Correlate dispatch evidence back to the operations that produced it.
fn operation_reports(
    ops: &[Op],
    outcomes: &[Outcome],
    steps: &[StepOutcome],
    bound: &HashMap<(usize, Part), OsString>,
) -> Vec<OperationReport> {
    let mut attributions = vec![None; ops.len()];
    let mut stdout = vec![None; ops.len()];
    for step in steps {
        for index in step.step().indices() {
            attributions[*index] = Some(step.attribution());
            if step.step().indices().len() == 1 {
                stdout[*index] = Some(step.stdout());
            }
        }
    }

    ops.iter()
        .enumerate()
        .map(|(index, op)| {
            let outcome = outcomes[index];
            OperationReport {
                index,
                kind: op.kind(),
                outcome,
                attribution: attributions[index],
                value: operation_value(index, op, outcome, stdout[index], bound),
            }
        })
        .collect()
}

fn operation_value(
    index: usize,
    op: &Op,
    outcome: Outcome,
    stdout: Option<&[u8]>,
    bound: &HashMap<(usize, Part), OsString>,
) -> Option<OperationValue> {
    match op {
        Op::NewSession(_) => Some(OperationValue::CreatedSession {
            session: bound_id(bound, index, Part::Created)?,
            window: bound_id(bound, index, Part::FirstWindow)?,
            pane: bound_id(bound, index, Part::FirstPane)?,
        }),
        Op::NewWindow(_) => Some(OperationValue::CreatedWindow {
            window: bound_id(bound, index, Part::Created)?,
            pane: bound_id(bound, index, Part::FirstPane)?,
        }),
        Op::SplitWindow(_) => Some(OperationValue::CreatedPane {
            pane: bound_id(bound, index, Part::Created)?,
        }),
        Op::CapturePane(_) if outcome.is_complete() => Some(OperationValue::CapturedPane(
            TmuxText::from_bytes(stdout?.to_vec()),
        )),
        _ if outcome.is_complete() => Some(OperationValue::Acknowledged),
        _ => None,
    }
}

fn bound_id<T: std::str::FromStr>(
    bound: &HashMap<(usize, Part), OsString>,
    index: usize,
    part: Part,
) -> Option<T> {
    bound.get(&(index, part))?.to_str()?.parse().ok()
}

#[cfg(feature = "control-mode")]
impl Plan {
    /// Run this plan over an open control-mode connection.
    ///
    /// Control mode is the one transport that separates *how many commands*
    /// from *how many processes*: every operation is its own protocol block
    /// over one connection, so a plan costs one process however long it is and
    /// every operation still reports its own outcome. That is the combination
    /// a subprocess cannot offer -- there, sharing an invocation is what buys
    /// the process back, and it is exactly what costs the attribution.
    ///
    /// There is no planner argument because there is nothing to trade: blocks
    /// are per command already.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection is closed, a command cannot be
    /// written, a slot dependency is invalid, or a creating operation does not
    /// return valid IDs. Validation happens before the first command. A command
    /// tmux refuses is reported in the [`PlanResult`].
    pub async fn run_over_control_mode(
        &self,
        sender: &crate::control::ControlSender,
    ) -> Result<PlanResult, Error> {
        self.validate()
            .map_err(|source| Error::InvalidPlan { source })?;
        let mut bound: HashMap<(usize, Part), OsString> = HashMap::new();
        let mut outcomes = vec![Outcome::Skipped; self.len()];
        let mut reported = Vec::with_capacity(self.len());
        let mut dispatches = 0;
        let mut effect_seen = false;

        for (index, op) in self.steps().iter().enumerate() {
            let resolve = |slot: usize, part: Part| bound.get(&(slot, part)).cloned();
            let command = op
                .render(&resolve, ())
                .ok_or_else(|| Error::CommandFailed {
                    command: op.name(),
                    exit_code: None,
                    stderr: format!(
                        "step {index} targets an object no earlier step created; \
                     a plan cannot address what it has not made"
                    ),
                })
                .map_err(|error| after_plan_effect(error, effect_seen))?;

            let sensitive_input = command.summary().sensitive_argument_count() > 0;
            let block = sender
                .send(command)
                .await
                .map_err(|error| after_plan_effect(error, effect_seen))?;
            dispatches += 1;

            let succeeded = block.succeeded();
            let outcome = if succeeded {
                Outcome::Complete
            } else {
                Outcome::Failed
            };
            outcomes[index] = outcome;
            let effect_through_step = effect_seen || (succeeded && !op.effects().read_only);

            let output = block
                .output()
                .iter()
                .flat_map(|line| {
                    let mut bytes = line.as_bytes().to_vec();
                    bytes.push(b'\n');
                    bytes
                })
                .collect::<Vec<u8>>();
            let (stdout, stderr) = if succeeded {
                (output, Vec::new())
            } else {
                (Vec::new(), output)
            };
            if succeeded {
                let step = Step::single(index);
                bind(&mut bound, self.steps(), &step, &stdout)
                    .map_err(|error| after_plan_effect(error, effect_through_step))?;
            }

            reported.push(StepOutcome {
                step: Step::single(index),
                command: op.name(),
                sensitive_input,
                outcomes: vec![outcome],
                // One block per command, so tmux says which one failed.
                attribution: Attribution::PerCommand,
                stdout,
                stderr,
            });

            if !succeeded {
                break;
            }
            effect_seen = effect_through_step;
        }

        let operations = operation_reports(self.steps(), &outcomes, &reported, &bound);
        Ok(PlanResult {
            operations,
            steps: reported,
            bound,
            dispatches,
        })
    }
}

fn after_plan_effect(error: Error, effect_seen: bool) -> Error {
    if effect_seen {
        error.after_effect("plan")
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::ExitStatus;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::PaneId;
    use crate::command::{CommandRequest, CommandResult, ProcessStatus};
    use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};
    use crate::plan::{CapturePane, NewSession, SendKeys};

    #[derive(Clone, Copy)]
    enum SecondOutcome {
        DispatchError,
        Refused,
    }

    struct SequentialExecutor {
        calls: AtomicUsize,
        second: SecondOutcome,
    }

    impl Executor for SequentialExecutor {
        fn execute(&self, request: CommandRequest) -> DispatchFuture {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let second = self.second;
            DispatchFuture::new(async move {
                if call == 0 {
                    return Ok(CommandResult::new(
                        request.request_id(),
                        request.summary().clone(),
                        ProcessStatus::from_exit_status(ExitStatus::from_raw(0)),
                        Vec::new(),
                        Vec::new(),
                    ));
                }

                match second {
                    SecondOutcome::DispatchError => Err(Error::Overloaded {
                        request_id: request.request_id().get(),
                        command: request.summary().clone(),
                        in_flight: 1,
                    }),
                    SecondOutcome::Refused => Ok(CommandResult::new(
                        request.request_id(),
                        request.summary().clone(),
                        ProcessStatus::from_exit_status(ExitStatus::from_raw(256)),
                        Vec::new(),
                        b"second operation refused".to_vec(),
                    )),
                }
            })
        }

        fn shutdown(&self) -> ShutdownFuture {
            ShutdownFuture::new(async { Ok(()) })
        }
    }

    fn two_step_plan(first_read_only: bool) -> Plan {
        let pane: PaneId = "%1".parse().expect("a pane id");
        let mut plan = Plan::new();
        if first_read_only {
            plan.add(CapturePane::new(pane.clone()));
        } else {
            plan.add(SendKeys::new(pane.clone()).text("first"));
        }
        plan.add(SendKeys::new(pane).text("second"));
        plan
    }

    struct CreationOutputExecutor {
        calls: AtomicUsize,
        stdout: &'static [u8],
    }

    impl Executor for CreationOutputExecutor {
        fn execute(&self, request: CommandRequest) -> DispatchFuture {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let stdout = if call == 0 {
                self.stdout.to_vec()
            } else {
                Vec::new()
            };
            DispatchFuture::new(async move {
                Ok(CommandResult::new(
                    request.request_id(),
                    request.summary().clone(),
                    ProcessStatus::from_exit_status(ExitStatus::from_raw(0)),
                    stdout,
                    Vec::new(),
                ))
            })
        }

        fn shutdown(&self) -> ShutdownFuture {
            ShutdownFuture::new(async { Ok(()) })
        }
    }

    #[test]
    fn creation_binding_rejects_a_wrong_scope_without_partial_state() {
        let mut plan = Plan::new();
        plan.add(NewSession::new("work"));
        let step = Planner::Sequential
            .steps(&plan)
            .into_iter()
            .next()
            .expect("one plan step");
        let mut bound = HashMap::new();

        let error = bind(
            &mut bound,
            plan.steps(),
            &step,
            b"$1 %2 sentinel-binding-output\n",
        )
        .expect_err("the second and third ids have the wrong scopes");

        assert_eq!(error.kind(), crate::ErrorKind::Decode);
        assert!(bound.is_empty(), "a failed decode binds nothing");
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains("sentinel-binding-output"));
    }

    #[tokio::test]
    async fn malformed_creation_output_stops_before_a_dependent_operation() {
        for stdout in [
            b"$1 %2 @3\n".as_slice(),
            b"$1 @2 sentinel-binding-output\n".as_slice(),
            b"$1 @2 %3".as_slice(),
        ] {
            let executor = Arc::new(CreationOutputExecutor {
                calls: AtomicUsize::new(0),
                stdout,
            });
            let server = Server::from_executor_for_test(executor.clone());
            let mut plan = Plan::new();
            let session = plan.add(NewSession::new("work"));
            plan.add(SendKeys::new(session.pane()).text("do-not-send"));

            let outcome = plan.run(&server, Planner::Sequential).await;

            assert_eq!(
                executor.calls.load(Ordering::SeqCst),
                1,
                "the dependent operation must not be dispatched",
            );
            let error = outcome.expect_err("malformed creation output fails the run");
            assert_eq!(error.kind(), crate::ErrorKind::PartialEffect);
            assert!(!error.is_transient());
            let diagnostic = format!("{error:?} {error}");
            assert!(!diagnostic.contains("sentinel-binding-output"));
        }
    }

    #[tokio::test]
    async fn a_later_dispatch_error_follows_the_first_mutating_plan_effect() {
        let executor = Arc::new(SequentialExecutor {
            calls: AtomicUsize::new(0),
            second: SecondOutcome::DispatchError,
        });
        let server = Server::from_executor_for_test(executor.clone());

        let error = two_step_plan(false)
            .run(&server, Planner::Sequential)
            .await
            .expect_err("the second dispatch fails");

        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
        assert!(
            matches!(
                &error,
                Error::AfterEffect { operation: "plan", source }
                    if source.kind() == crate::ErrorKind::Refused && source.is_transient()
            ),
            "{error:?}",
        );
        assert!(!error.is_transient());
    }

    #[tokio::test]
    async fn a_dispatch_error_after_only_reads_keeps_its_retryability() {
        let executor = Arc::new(SequentialExecutor {
            calls: AtomicUsize::new(0),
            second: SecondOutcome::DispatchError,
        });
        let server = Server::from_executor_for_test(executor.clone());

        let error = two_step_plan(true)
            .run(&server, Planner::Sequential)
            .await
            .expect_err("the second dispatch fails");

        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
        assert_eq!(error.kind(), crate::ErrorKind::Refused);
        assert!(error.is_transient());
        assert!(!matches!(error, Error::AfterEffect { .. }));
    }

    #[tokio::test]
    async fn a_later_refusal_remains_plan_result_data_after_a_mutation() {
        let executor = Arc::new(SequentialExecutor {
            calls: AtomicUsize::new(0),
            second: SecondOutcome::Refused,
        });
        let server = Server::from_executor_for_test(executor.clone());

        let result = two_step_plan(false)
            .run(&server, Planner::Sequential)
            .await
            .expect("tmux refusals remain inspectable plan data");

        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
        assert!(!result.is_complete());
        assert_eq!(result.operations()[0].outcome(), Outcome::Complete);
        assert_eq!(result.operations()[1].outcome(), Outcome::Failed);
        assert!(result.steps()[1].refusal().is_some());
    }

    #[tokio::test]
    async fn a_refusal_after_only_reads_remains_plan_result_data() {
        let executor = Arc::new(SequentialExecutor {
            calls: AtomicUsize::new(0),
            second: SecondOutcome::Refused,
        });
        let server = Server::from_executor_for_test(executor.clone());

        let result = two_step_plan(true)
            .run(&server, Planner::Sequential)
            .await
            .expect("nothing changed before the refusal");

        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
        assert!(!result.is_complete());
        assert!(result.steps()[1].refusal().is_some());
    }

    #[test]
    fn raw_plan_output_is_absent_from_debug() {
        let secret = b"sentinel-plan-output";
        let pane: PaneId = "%1".parse().expect("a pane id");
        let mut plan = Plan::new();
        plan.add(CapturePane::new(pane));
        let step = Planner::Sequential
            .steps(&plan)
            .into_iter()
            .next()
            .expect("one plan step");
        let reported = StepOutcome {
            step,
            outcomes: vec![Outcome::Failed],
            attribution: Attribution::PerCommand,
            command: "capture-pane",
            sensitive_input: true,
            stdout: secret.to_vec(),
            stderr: secret.to_vec(),
        };
        let result = PlanResult {
            operations: vec![OperationReport {
                index: 0,
                kind: OperationKind::CapturePane,
                outcome: Outcome::Failed,
                attribution: Some(Attribution::PerCommand),
                value: None,
            }],
            steps: vec![reported.clone()],
            bound: HashMap::new(),
            dispatches: 1,
        };
        let exposed = format!("{:?}", secret.to_vec());

        for diagnostic in [format!("{reported:?}"), format!("{result:?}")] {
            assert!(!diagnostic.contains(&exposed), "{diagnostic}");
        }
    }

    #[test]
    fn sensitive_refusal_withholds_tmux_output() {
        let secret = "sentinel-plan-secret";
        let pane: PaneId = "%1".parse().expect("a pane id");
        let mut plan = Plan::new();
        plan.add(CapturePane::new(pane));
        let step = Planner::Sequential
            .steps(&plan)
            .into_iter()
            .next()
            .expect("one plan step");
        let reported = StepOutcome {
            step,
            outcomes: vec![Outcome::Failed],
            attribution: Attribution::PerCommand,
            command: "set-option",
            sensitive_input: true,
            stdout: Vec::new(),
            stderr: format!("bad value: {secret}\n").into_bytes(),
        };

        assert!(String::from_utf8_lossy(reported.stderr()).contains(secret));
        let refusal = reported.refusal().expect("the invocation failed");
        assert!(matches!(&refusal, Error::CommandFailed { .. }));
        let diagnostic = format!("{refusal:?} {refusal}");
        assert!(!diagnostic.contains(secret), "{diagnostic}");
    }
}
