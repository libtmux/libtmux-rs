//! Describe tmux work before doing any of it.
//!
//! A [`Plan`] records operations without touching tmux, so it can be
//! inspected, explained, and only then run. That separation is what lets the
//! same recorded work dispatch as one command per operation or as a folded
//! `tmux a \; b`: the [`Planner`] decides how many processes it costs, and the
//! answer does not change.
//!
//! An operation may target an object an *earlier* operation will create. That
//! reference is a [`Slot`], and it carries the scope of what it points at, so
//! sending keys to the window a split is about to make is a compile error
//! rather than a target tmux rejects at run time:
//!
//! ```compile_fail
//! use libtmux::plan::{Plan, SplitWindow, SendKeys};
//! use libtmux::WindowId;
//!
//! let mut plan = Plan::new();
//! let pane = plan.add(SplitWindow::new("@1".parse::<WindowId>().unwrap()));
//! // `pane` is a Slot<Pane>; SelectWindow wants a window.
//! plan.add(libtmux::plan::SelectWindow::new(pane));
//! ```
//!
//! With the `serde` feature a plan is also a document: it serializes to JSON
//! and back without changing what it renders, so one can be written by hand,
//! stored, or sent somewhere else to run. Arguments stay exact -- text where
//! tmux was given text, bytes where it was given bytes no text format could
//! carry.
//!
//! # Examples
//!
//! ```
//! use libtmux::plan::{Plan, Planner, SendKeys, SplitWindow};
//! use libtmux::WindowId;
//!
//! let window = "@1".parse::<WindowId>()?;
//! let mut plan = Plan::new();
//! // A focused split can share its invocation with what decorates it; a
//! // detached one cannot, because the fold marks whichever pane is active.
//! let pane = plan.add(SplitWindow::new(window).focus());
//! plan.add(SendKeys::new(pane).text("cargo test").enter());
//!
//! // Two operations, but not necessarily two tmux processes.
//! assert_eq!(plan.len(), 2);
//! assert_eq!(Planner::Sequential.steps(&plan).len(), 2);
//! assert_eq!(Planner::Marked.steps(&plan).len(), 1);
//! # Ok::<(), libtmux::IdParseError>(())
//! ```

use std::ffi::OsString;
use std::fmt;
use std::marker::PhantomData;

use crate::{Command, PaneId, SessionId, WindowId};

mod ops;
mod planner;
mod run;
#[cfg(feature = "serde")]
mod wire;

pub use ops::{
    CapturePane, KillPane, KillWindow, NewSession, NewWindow, RenameWindow, SelectLayout,
    SelectPane, SelectWindow, SendKeys, SetEnvironment, SetOption, SplitWindow,
};
pub use planner::{Planner, Step, StepReason};
pub use run::{Attribution, OperationReport, OperationValue, Outcome, PlanResult, StepOutcome};

/// What an operation does to tmux state.
///
/// Carried as data rather than left implicit so a planner, a safety policy,
/// and a documentation table all read the same answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a descriptor whose named fields are the API a caller reads"
)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Effects {
    /// The operation does not change tmux state.
    pub read_only: bool,
    /// The operation removes an object.
    pub destructive: bool,
    /// Running it twice leaves what running it once leaves.
    pub idempotent: bool,
    /// The scope of the object it creates, if it creates one.
    pub creates: Option<Scope>,
    /// It captures pane output, so its stdout is the answer.
    pub reads_output: bool,
}

impl Effects {
    /// An operation that changes state, creates nothing, and reads nothing.
    const MUTATING: Self = Self {
        read_only: false,
        destructive: false,
        idempotent: false,
        creates: None,
        reads_output: false,
    };
}

/// The tmux object an operation addresses or makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Scope {
    /// A tmux session.
    Session,
    /// A window within a session.
    Window,
    /// A pane within a window.
    Pane,
}

/// Why a plan dependency is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PlanValidationErrorKind {
    /// The referenced source index is outside the plan.
    SourceMissing,
    /// The source is the same step or a later one.
    SourceNotEarlier,
    /// The source does not produce the referenced output.
    SourceOutputMissing,
    /// The source output is a different tmux object kind.
    SourceScopeMismatch,
}

/// An invalid dependency between two plan steps.
///
/// No tmux command is dispatched when validation fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanValidationError {
    step: usize,
    source_step: usize,
    kind: PlanValidationErrorKind,
    expected_scope: Scope,
    source_scope: Option<Scope>,
}

impl PlanValidationError {
    const fn new(
        step: usize,
        source_step: usize,
        kind: PlanValidationErrorKind,
        expected_scope: Scope,
        source_scope: Option<Scope>,
    ) -> Self {
        Self {
            step,
            source_step,
            kind,
            expected_scope,
            source_scope,
        }
    }

    /// The step carrying the invalid target.
    #[must_use]
    pub const fn step(&self) -> usize {
        self.step
    }

    /// The step the invalid target references.
    #[must_use]
    pub const fn source_step(&self) -> usize {
        self.source_step
    }

    /// The validation failure category.
    #[must_use]
    pub const fn kind(&self) -> PlanValidationErrorKind {
        self.kind
    }

    /// The tmux object kind the target requires.
    #[must_use]
    pub const fn expected_scope(&self) -> Scope {
        self.expected_scope
    }

    /// The source output's object kind, when that output exists.
    #[must_use]
    pub const fn source_scope(&self) -> Option<Scope> {
        self.source_scope
    }
}

impl fmt::Display for PlanValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plan step {} has an invalid dependency on step {}: ",
            self.step, self.source_step
        )?;
        match self.kind {
            PlanValidationErrorKind::SourceMissing => formatter.write_str("the source is absent"),
            PlanValidationErrorKind::SourceNotEarlier => {
                formatter.write_str("the source is not earlier")
            }
            PlanValidationErrorKind::SourceOutputMissing => write!(
                formatter,
                "the source does not produce the requested {:?} output",
                self.expected_scope
            ),
            PlanValidationErrorKind::SourceScopeMismatch => write!(
                formatter,
                "the source output is {:?}, not {:?}",
                self.source_scope, self.expected_scope
            ),
        }
    }
}

impl std::error::Error for PlanValidationError {}

#[derive(Clone, Copy)]
pub(in crate::plan) struct SlotUse {
    pub(in crate::plan) source_step: usize,
    pub(in crate::plan) part: Part,
    expected_scope: Scope,
}

/// How much damage an operation can do, for callers that gate on it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Safety {
    /// Reads state and changes nothing.
    ReadOnly,
    /// Changes state reversibly.
    Mutating,
    /// Removes an object.
    Destructive,
}

/// A typed reference to the object a recorded operation will create.
///
/// Returned by [`Plan::add`]. The type parameter is the scope it points at, so
/// a slot for a pane cannot be handed to an operation that wants a window.
/// A slot is only meaningful inside the plan that produced it.
///
/// # Examples
///
/// ```
/// use libtmux::plan::{NewWindow, Plan, SendKeys};
/// use libtmux::SessionId;
///
/// let mut plan = Plan::new();
/// let window = plan.add(NewWindow::new("$0".parse::<SessionId>()?).name("build"));
/// // A new window owns a first pane, reachable without listing anything.
/// plan.add(SendKeys::new(window.pane()).text("just check").enter());
/// # Ok::<(), libtmux::IdParseError>(())
/// ```
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(bound = ""))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(bound = ""))]
pub struct Slot<T> {
    index: usize,
    part: Part,
    #[cfg_attr(feature = "serde", serde(skip))]
    scope: PhantomData<fn() -> T>,
}

/// Which of a creating operation's objects a slot points at.
///
/// `new-session` prints a session, its first window, and that window's first
/// pane from one command, so all three are addressable without a second round
/// trip.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub(crate) enum Part {
    /// The object the operation is named for.
    Created,
    /// The first window of a created session.
    FirstWindow,
    /// The first pane of a created session or window.
    FirstPane,
}

impl<T> Slot<T> {
    pub(crate) const fn new(index: usize, part: Part) -> Self {
        Self {
            index,
            part,
            scope: PhantomData,
        }
    }

    /// The index of the operation that creates this object.
    #[must_use]
    pub const fn step(&self) -> usize {
        self.index
    }
}

// Derived impls would demand `T: Clone` and friends, which is wrong: a slot is
// an index and a tag, and the tag is never held.
impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Slot<T> {}

impl<T> fmt::Debug for Slot<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Slot")
            .field("step", &self.index)
            .field("part", &self.part)
            .finish()
    }
}

impl<T> PartialEq for Slot<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.part == other.part
    }
}

impl<T> Eq for Slot<T> {}

/// Marker for a slot that points at a session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SessionSlot;

/// Marker for a slot that points at a window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WindowSlot;

/// Marker for a slot that points at a pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PaneSlot;

impl Slot<SessionSlot> {
    /// The first window tmux made with this session.
    #[must_use]
    pub const fn window(self) -> Slot<WindowSlot> {
        Slot::new(self.index, Part::FirstWindow)
    }

    /// The first pane of this session's first window.
    #[must_use]
    pub const fn pane(self) -> Slot<PaneSlot> {
        Slot::new(self.index, Part::FirstPane)
    }
}

impl Slot<WindowSlot> {
    /// The first pane tmux made with this window.
    #[must_use]
    pub const fn pane(self) -> Slot<PaneSlot> {
        Slot::new(self.index, Part::FirstPane)
    }
}

/// A `-t` target naming a pane, resolved when the plan runs.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PaneTarget {
    /// A pane that already exists.
    Id(
        #[cfg_attr(
            feature = "serde",
            serde(serialize_with = "wire::id", deserialize_with = "wire::parse_id")
        )]
        #[cfg_attr(feature = "schema", schemars(schema_with = "wire::pane_id_schema"))]
        PaneId,
    ),
    /// The pane an earlier step creates.
    Slot(Slot<PaneSlot>),
}

/// A `-t` target naming a window, resolved when the plan runs.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WindowTarget {
    /// A window that already exists.
    Id(
        #[cfg_attr(
            feature = "serde",
            serde(serialize_with = "wire::id", deserialize_with = "wire::parse_id")
        )]
        #[cfg_attr(feature = "schema", schemars(schema_with = "wire::window_id_schema"))]
        WindowId,
    ),
    /// The window an earlier step creates.
    Slot(Slot<WindowSlot>),
}

/// A `-t` target naming a session, resolved when the plan runs.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SessionTarget {
    /// A session that already exists.
    Id(
        #[cfg_attr(
            feature = "serde",
            serde(serialize_with = "wire::id", deserialize_with = "wire::parse_id")
        )]
        #[cfg_attr(feature = "schema", schemars(schema_with = "wire::session_id_schema"))]
        SessionId,
    ),
    /// The session an earlier step creates.
    Slot(Slot<SessionSlot>),
}

macro_rules! target_conversions {
    ($target:ty, $id:ty, $marker:ty, $scope:expr) => {
        impl From<$id> for $target {
            fn from(id: $id) -> Self {
                Self::Id(id)
            }
        }

        impl From<Slot<$marker>> for $target {
            fn from(slot: Slot<$marker>) -> Self {
                Self::Slot(slot)
            }
        }

        impl $target {
            pub(in crate::plan) const fn slot(&self) -> Option<SlotUse> {
                match self {
                    Self::Id(_) => None,
                    Self::Slot(slot) => Some(SlotUse {
                        source_step: slot.index,
                        part: slot.part,
                        expected_scope: $scope,
                    }),
                }
            }

            /// The tmux `-t` token, once any slot has been resolved.
            pub(crate) fn token(
                &self,
                resolve: &dyn Fn(usize, Part) -> Option<OsString>,
            ) -> Option<OsString> {
                match self {
                    Self::Id(id) => Some(OsString::from(id.to_string())),
                    Self::Slot(slot) => resolve(slot.index, slot.part),
                }
            }
        }
    };
}

target_conversions!(PaneTarget, PaneId, PaneSlot, Scope::Pane);
target_conversions!(WindowTarget, WindowId, WindowSlot, Scope::Window);
target_conversions!(SessionTarget, SessionId, SessionSlot, Scope::Session);

/// An operation that can be recorded in a [`Plan`].
///
/// Implemented by this crate's operation types, not by callers: a plan folds
/// operations into shared tmux invocations, and that is only sound for the
/// commands whose rendering and results it knows.
pub trait Operation: Into<Op> {
    /// What [`Plan::add`] hands back, which is a [`Slot`] when this creates
    /// something and `()` when it does not.
    type Creates: FromStep;

    /// What the operation does to tmux state.
    const EFFECTS: Effects;

    /// How much damage it can do.
    const SAFETY: Safety;

    /// The lowest tmux release that accepts it, when it is not universal.
    const MIN_VERSION: Option<(u32, u32)> = None;
}

/// An operation that may share one tmux invocation with its neighbours.
///
/// A folded run of commands returns one merged stdout with no per-command
/// boundary, so an operation whose output *is* its answer cannot be folded:
/// its lines would be indistinguishable from its neighbours'. This trait is
/// implemented only for the operations where that cannot happen, which makes
/// [`Plan::chain`] reject the others at compile time.
///
/// ```compile_fail
/// use libtmux::plan::{CapturePane, Plan, SendKeys};
/// use libtmux::PaneId;
///
/// let pane = "%1".parse::<PaneId>().unwrap();
/// let mut plan = Plan::new();
/// // CapturePane is not Chainable: its stdout is the answer.
/// plan.chain(CapturePane::new(pane));
/// ```
pub trait Chainable: Operation {}

/// Turns a recorded step index into whatever [`Plan::add`] returns for it.
pub trait FromStep {
    /// Build the handle for the operation recorded at `index`.
    fn from_step(index: usize) -> Self;
}

impl FromStep for () {
    fn from_step(_: usize) {}
}

impl<T> FromStep for Slot<T> {
    fn from_step(index: usize) -> Self {
        Self::new(index, Part::Created)
    }
}

/// One recorded operation.
///
/// A plan holds these rather than boxed trait objects: the set of commands is
/// closed, so an enum keeps a plan allocation-free per step, `Clone`, and
/// inspectable without downcasting.
#[derive(Clone, Debug)]
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Op {
    /// Create a session.
    NewSession(NewSession),
    /// Create a window.
    NewWindow(NewWindow),
    /// Split a window into a new pane.
    SplitWindow(SplitWindow),
    /// Send text or keys to a pane.
    SendKeys(SendKeys),
    /// Make a pane active.
    SelectPane(SelectPane),
    /// Make a window active.
    SelectWindow(SelectWindow),
    /// Rename a window.
    RenameWindow(RenameWindow),
    /// Set a tmux option.
    SetOption(SetOption),
    /// Set a variable in a session's environment.
    SetEnvironment(SetEnvironment),
    /// Rearrange a window's panes.
    SelectLayout(SelectLayout),
    /// Capture a pane's contents.
    CapturePane(CapturePane),
    /// Destroy a pane.
    KillPane(KillPane),
    /// Destroy a window.
    KillWindow(KillWindow),
}

/// Which operation a result describes, without retaining its arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OperationKind {
    /// Create a session.
    NewSession,
    /// Create a window.
    NewWindow,
    /// Split a window into a new pane.
    SplitWindow,
    /// Send text or keys to a pane.
    SendKeys,
    /// Make a pane active.
    SelectPane,
    /// Make a window active.
    SelectWindow,
    /// Rename a window.
    RenameWindow,
    /// Set a tmux option.
    SetOption,
    /// Set a variable in a session's environment.
    SetEnvironment,
    /// Rearrange a window's panes.
    SelectLayout,
    /// Capture a pane's contents.
    CapturePane,
    /// Destroy a pane.
    KillPane,
    /// Destroy a window.
    KillWindow,
}

impl OperationKind {
    /// The tmux subcommand this operation runs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NewSession => "new-session",
            Self::NewWindow => "new-window",
            Self::SplitWindow => "split-window",
            Self::SendKeys => "send-keys",
            Self::SelectPane => "select-pane",
            Self::SelectWindow => "select-window",
            Self::RenameWindow => "rename-window",
            Self::SetOption => "set-option",
            Self::SetEnvironment => "set-environment",
            Self::SelectLayout => "select-layout",
            Self::CapturePane => "capture-pane",
            Self::KillPane => "kill-pane",
            Self::KillWindow => "kill-window",
        }
    }
}

impl Op {
    /// Which operation this is, without exposing its arguments.
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        match self {
            Self::NewSession(_) => OperationKind::NewSession,
            Self::NewWindow(_) => OperationKind::NewWindow,
            Self::SplitWindow(_) => OperationKind::SplitWindow,
            Self::SendKeys(_) => OperationKind::SendKeys,
            Self::SelectPane(_) => OperationKind::SelectPane,
            Self::SelectWindow(_) => OperationKind::SelectWindow,
            Self::RenameWindow(_) => OperationKind::RenameWindow,
            Self::SetOption(_) => OperationKind::SetOption,
            Self::SetEnvironment(_) => OperationKind::SetEnvironment,
            Self::SelectLayout(_) => OperationKind::SelectLayout,
            Self::CapturePane(_) => OperationKind::CapturePane,
            Self::KillPane(_) => OperationKind::KillPane,
            Self::KillWindow(_) => OperationKind::KillWindow,
        }
    }

    /// What this operation does to tmux state.
    #[must_use]
    pub const fn effects(&self) -> Effects {
        match self {
            Self::NewSession(_) => NewSession::EFFECTS,
            Self::NewWindow(_) => NewWindow::EFFECTS,
            Self::SplitWindow(_) => SplitWindow::EFFECTS,
            Self::SendKeys(_) => SendKeys::EFFECTS,
            Self::SelectPane(_) => SelectPane::EFFECTS,
            Self::SelectWindow(_) => SelectWindow::EFFECTS,
            Self::RenameWindow(_) => RenameWindow::EFFECTS,
            Self::SetOption(_) => SetOption::EFFECTS,
            Self::SetEnvironment(_) => SetEnvironment::EFFECTS,
            Self::SelectLayout(_) => SelectLayout::EFFECTS,
            Self::CapturePane(_) => CapturePane::EFFECTS,
            Self::KillPane(_) => KillPane::EFFECTS,
            Self::KillWindow(_) => KillWindow::EFFECTS,
        }
    }

    /// How much damage this operation can do.
    #[must_use]
    pub const fn safety(&self) -> Safety {
        match self {
            Self::NewSession(_) => NewSession::SAFETY,
            Self::NewWindow(_) => NewWindow::SAFETY,
            Self::SplitWindow(_) => SplitWindow::SAFETY,
            Self::SendKeys(_) => SendKeys::SAFETY,
            Self::SelectPane(_) => SelectPane::SAFETY,
            Self::SelectWindow(_) => SelectWindow::SAFETY,
            Self::RenameWindow(_) => RenameWindow::SAFETY,
            Self::SetOption(_) => SetOption::SAFETY,
            Self::SetEnvironment(_) => SetEnvironment::SAFETY,
            Self::SelectLayout(_) => SelectLayout::SAFETY,
            Self::CapturePane(_) => CapturePane::SAFETY,
            Self::KillPane(_) => KillPane::SAFETY,
            Self::KillWindow(_) => KillWindow::SAFETY,
        }
    }

    /// The tmux subcommand this operation runs.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.kind().name()
    }

    /// Whether this operation may share a tmux invocation with its neighbours.
    ///
    /// The compile-time answer is [`Chainable`]; this is the same answer for a
    /// planner, which works over recorded operations rather than typed ones.
    #[must_use]
    pub const fn is_chainable(&self) -> bool {
        // An operation that reads output cannot fold: its lines would be
        // indistinguishable from a neighbour's in one merged stdout. Nor can
        // one that creates an object, because the id it prints is read back by
        // position.
        !self.effects().reads_output && self.effects().creates.is_none()
    }

    /// Which of this operation's created objects is the pane it left active.
    ///
    /// `None` when it creates nothing, or creates without taking focus. The
    /// `{marked}` fold marks the *active* pane, so an operation that does not
    /// leave its own pane active cannot be folded with what decorates it: the
    /// mark would land on whichever pane was already there.
    pub(crate) const fn focused_pane(&self) -> Option<Part> {
        match self {
            Self::SplitWindow(op) if op.focuses() => Some(Part::Created),
            Self::NewWindow(op) if op.focuses() => Some(Part::FirstPane),
            _ => None,
        }
    }

    /// The targets this operation resolves before it can render.
    fn slots(&self) -> [Option<SlotUse>; 2] {
        match self {
            Self::NewSession(_) => [None, None],
            Self::NewWindow(op) => [op.target.slot(), None],
            Self::SplitWindow(op) => [op.target.slot(), None],
            Self::SendKeys(op) => [op.target.slot(), None],
            Self::SelectPane(op) => [op.target.slot(), None],
            Self::SelectWindow(op) => [op.target.slot(), None],
            Self::RenameWindow(op) => [op.target.slot(), None],
            Self::SetOption(op) => [op.target(), None],
            Self::SetEnvironment(op) => [op.target.slot(), None],
            Self::SelectLayout(op) => [op.target.slot(), None],
            Self::CapturePane(op) => [op.target.slot(), None],
            Self::KillPane(op) => [op.target.slot(), None],
            Self::KillWindow(op) => [op.target.slot(), None],
        }
    }

    fn output_scope(&self, part: Part) -> Option<Scope> {
        let created = self.effects().creates?;
        match (part, created) {
            (Part::Created, scope) => Some(scope),
            (Part::FirstWindow, Scope::Session) => Some(Scope::Window),
            (Part::FirstPane, Scope::Session | Scope::Window) => Some(Scope::Pane),
            _ => None,
        }
    }

    /// Lower this operation into the tmux command that performs it.
    ///
    /// `resolve` supplies the concrete id for a target that names an earlier
    /// step, and `capture` asks a creating operation to print what it made.
    pub(crate) fn render(
        &self,
        resolve: &dyn Fn(usize, Part) -> Option<OsString>,
        _reserved: (),
    ) -> Option<Command> {
        match self {
            Self::NewSession(op) => Some(op.render()),
            Self::NewWindow(op) => op.render(resolve),
            Self::SplitWindow(op) => op.render(resolve),
            Self::SendKeys(op) => op.render(resolve),
            Self::SelectPane(op) => op.render(resolve),
            Self::SelectWindow(op) => op.render(resolve),
            Self::RenameWindow(op) => op.render(resolve),
            Self::SetOption(op) => op.render(resolve),
            Self::SetEnvironment(op) => op.render(resolve),
            Self::SelectLayout(op) => op.render(resolve),
            Self::CapturePane(op) => op.render(resolve),
            Self::KillPane(op) => op.render(resolve),
            Self::KillWindow(op) => op.render(resolve),
        }
    }
}

/// Operations recorded but not yet run.
///
/// # Examples
///
/// ```
/// use libtmux::plan::{Plan, RenameWindow, SendKeys};
/// use libtmux::{PaneId, WindowId};
///
/// let mut plan = Plan::new();
/// plan.add(SendKeys::new("%1".parse::<PaneId>()?).text("clear").enter());
/// plan.add(RenameWindow::new("@1".parse::<WindowId>()?, "ready"));
///
/// // Nothing has run: the plan can be read first.
/// let rendered = plan.preview();
/// assert_eq!(rendered.len(), 2);
/// assert!(rendered[0].as_ref().is_some_and(|c| c.summary().to_string().contains("send-keys")));
/// # Ok::<(), libtmux::IdParseError>(())
/// ```
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
// A plan is its steps, so it reads as a list rather than an object wrapping
// one. That is the shape a plan written by hand wants.
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Plan {
    steps: Vec<Op>,
}

impl Plan {
    /// Start an empty plan.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Record one operation and return a handle to what it creates.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::plan::{Plan, SplitWindow};
    /// use libtmux::WindowId;
    ///
    /// let mut plan = Plan::new();
    /// let pane = plan.add(SplitWindow::new("@1".parse::<WindowId>()?));
    /// assert_eq!(pane.step(), 0);
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    pub fn add<O: Operation>(&mut self, operation: O) -> O::Creates {
        let index = self.steps.len();
        self.steps.push(operation.into());
        O::Creates::from_step(index)
    }

    /// Record an operation that is allowed to share a tmux invocation.
    ///
    /// Identical to [`Plan::add`] except that the compiler rejects an
    /// operation whose output could not be told apart from its neighbours'.
    /// Use it to say that intent at the call site; a folding [`Planner`]
    /// reaches the same grouping on its own.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::plan::{Plan, SendKeys};
    /// use libtmux::PaneId;
    ///
    /// let mut plan = Plan::new();
    /// plan.chain(SendKeys::new("%1".parse::<PaneId>()?).text("make").enter());
    /// # Ok::<(), libtmux::IdParseError>(())
    /// ```
    pub fn chain<O: Chainable>(&mut self, operation: O) -> O::Creates {
        self.add(operation)
    }

    /// The recorded operations, in order.
    #[must_use]
    pub fn steps(&self) -> &[Op] {
        &self.steps
    }

    /// How many operations are recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Whether nothing is recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Check every slot before a run can change tmux state.
    ///
    /// # Errors
    ///
    /// Returns an error when a slot names an absent or non-earlier step, or
    /// when that step does not produce the required tmux object.
    pub fn validate(&self) -> Result<(), PlanValidationError> {
        for (step, operation) in self.steps.iter().enumerate() {
            for slot_use in operation.slots().into_iter().flatten() {
                let Some(source) = self.steps.get(slot_use.source_step) else {
                    return Err(PlanValidationError::new(
                        step,
                        slot_use.source_step,
                        PlanValidationErrorKind::SourceMissing,
                        slot_use.expected_scope,
                        None,
                    ));
                };
                if slot_use.source_step >= step {
                    return Err(PlanValidationError::new(
                        step,
                        slot_use.source_step,
                        PlanValidationErrorKind::SourceNotEarlier,
                        slot_use.expected_scope,
                        source.output_scope(slot_use.part),
                    ));
                }

                let Some(source_scope) = source.output_scope(slot_use.part) else {
                    return Err(PlanValidationError::new(
                        step,
                        slot_use.source_step,
                        PlanValidationErrorKind::SourceOutputMissing,
                        slot_use.expected_scope,
                        None,
                    ));
                };
                if source_scope != slot_use.expected_scope {
                    return Err(PlanValidationError::new(
                        step,
                        slot_use.source_step,
                        PlanValidationErrorKind::SourceScopeMismatch,
                        slot_use.expected_scope,
                        Some(source_scope),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Render every operation without running any of them.
    ///
    /// An operation whose target is an object no earlier step has created yet
    /// renders as `None`: it needs an id that only exists once the plan runs.
    #[must_use]
    pub fn preview(&self) -> Vec<Option<Command>> {
        self.steps
            .iter()
            .map(|op| op.render(&|_, _| None, ()))
            .collect()
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Plan {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;

        let plan = Self {
            steps: Vec::<Op>::deserialize(deserializer)?,
        };
        plan.validate().map_err(D::Error::custom)?;
        Ok(plan)
    }
}
