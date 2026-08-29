use libtmux::plan::{OperationValue as CoreOperationValue, Plan};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use libtmux::TmuxText;

/// The shared text budget for all evidence in one plan response.
const PLAN_EVIDENCE_BYTES: usize = 64 * 1024;

/// Closed vocabularies used only to describe string fields in JSON Schema.
#[allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]
#[derive(Clone, Debug, Eq, PartialEq, schemars::JsonSchema)]
#[schemars(rename_all = "snake_case")]
enum PlanGroupingSchema {
    Sequential,
    Folding,
    Marked,
}

#[allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]
#[derive(Clone, Debug, Eq, PartialEq, schemars::JsonSchema)]
#[schemars(rename_all = "snake_case")]
enum SplitDirectionSchema {
    Above,
    Below,
    Left,
    Right,
}

#[allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]
#[derive(Clone, Debug, Eq, PartialEq, schemars::JsonSchema)]
#[schemars(rename_all = "snake_case")]
enum ResizeDirectionSchema {
    Up,
    Down,
    Left,
    Right,
}

#[allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]
#[derive(Clone, Debug, Eq, PartialEq, schemars::JsonSchema)]
#[schemars(rename_all = "snake_case")]
enum SelectPaneDirectionSchema {
    Up,
    Down,
    Left,
    Right,
    Last,
    Next,
    Previous,
}

#[allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]
#[derive(Clone, Debug, Eq, PartialEq, schemars::JsonSchema)]
#[schemars(rename_all = "snake_case")]
enum SelectWindowDirectionSchema {
    Next,
    Previous,
    Last,
}

#[allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]
#[derive(Clone, Debug, Eq, PartialEq, schemars::JsonSchema)]
#[schemars(rename_all = "kebab-case")]
enum OptionScopeSchema {
    Server,
    GlobalSession,
    GlobalWindow,
    Session,
    Window,
    Pane,
}

/// Arguments naming one session.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionArgs {
    /// The session name, as `list_sessions` reports it.
    pub session: String,
}

/// Arguments for creating a session.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateSessionArgs {
    /// The name for the new session. It must not already exist.
    pub name: String,
    /// An optional working directory for the session's first window.
    pub start_directory: Option<String>,
}

/// Arguments naming one pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PaneArgs {
    /// The `%`-prefixed pane id, as `list_panes` reports it.
    pub pane: String,
}

/// Arguments naming one window.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WindowArgs {
    /// The `@`-prefixed window id, as `list_windows` reports it.
    pub window: String,
}

/// Arguments for creating a window in a session.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NewWindowArgs {
    /// The session name to create the window in.
    pub session: String,
    /// An optional name for the new window.
    pub name: Option<String>,
}

/// Arguments for running a recorded plan.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunPlanArgs {
    /// The plan, as the JSON a `libtmux` plan serializes to.
    pub plan: Plan,
    /// How to group the plan: `sequential`, `folding`, or `marked`.
    ///
    /// Defaults to `sequential`, which is the only grouping that can say
    /// which operation failed.
    #[schemars(with = "Option<PlanGroupingSchema>")]
    pub grouping: Option<String>,
}

/// What running a plan produced.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PlanRun {
    /// One report per operation, in the order the plan records them.
    pub operations: Vec<PlanOperationReport>,
    /// One report per refused invocation.
    pub failures: Vec<PlanFailure>,
    /// How many tmux invocations it cost.
    pub dispatches: usize,
    /// Whether every operation is known to have succeeded.
    pub complete: bool,
}

/// One operation's outcome and typed answer.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PlanOperationReport {
    /// The operation's zero-based index in the plan.
    pub index: usize,
    /// The tmux command the operation runs.
    pub kind: String,
    /// `complete`, `failed`, `skipped`, or `unknown`.
    ///
    /// `unknown` means the operation shared a tmux invocation with a failure
    /// and nothing distinguishes them. Re-run with `grouping: "sequential"`
    /// to find out which one failed.
    pub outcome: String,
    /// `per_command` or `merged`, or `null` when nothing was dispatched.
    pub attribution: Option<String>,
    /// The operation's typed answer, or `null` when it produced none.
    pub value: Option<PlanValue>,
}

/// One refused invocation, kept separate from operation return values.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PlanFailure {
    /// The operation indices that shared the refused invocation.
    pub operations: Vec<usize>,
    /// `per_command` or `merged`.
    pub attribution: String,
    /// The stable failure category.
    pub kind: PlanFailureKind,
    /// Bounded stderr, unless the invocation carried sensitive input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<PlanEvidence>,
    /// How many stderr bytes tmux returned.
    pub stderr_bytes: usize,
    /// Whether stderr was withheld because an argument was sensitive.
    pub stderr_withheld: bool,
}

/// A payload-free plan failure category.
#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanFailureKind {
    /// A referenced tmux object disappeared.
    ObjectGone,
    /// tmux rejected the invocation.
    Refused,
    /// No tmux server answered.
    ServerGone,
    /// The invocation timed out.
    Timeout,
    /// tmux could not be reached or started.
    Unreachable,
    /// The tmux release lacks a required capability.
    UnsupportedVersion,
    /// The invocation contained invalid input.
    InvalidInput,
    /// The transport failed.
    Transport,
    /// tmux output could not be decoded.
    Decode,
    /// A newer libtmux failure category this server does not yet name.
    Other,
}

impl From<libtmux::ErrorKind> for PlanFailureKind {
    fn from(kind: libtmux::ErrorKind) -> Self {
        match kind {
            libtmux::ErrorKind::ObjectGone => Self::ObjectGone,
            libtmux::ErrorKind::Refused => Self::Refused,
            libtmux::ErrorKind::ServerGone => Self::ServerGone,
            libtmux::ErrorKind::Timeout => Self::Timeout,
            libtmux::ErrorKind::Unreachable => Self::Unreachable,
            libtmux::ErrorKind::UnsupportedVersion => Self::UnsupportedVersion,
            libtmux::ErrorKind::InvalidInput => Self::InvalidInput,
            libtmux::ErrorKind::Transport => Self::Transport,
            libtmux::ErrorKind::Decode => Self::Decode,
            _ => Self::Other,
        }
    }
}

/// Bounded tmux output carried by a plan response.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PlanEvidence {
    /// The retained tail, with invalid UTF-8 replaced.
    pub text: String,
    /// How many source bytes tmux returned.
    pub bytes: usize,
    /// How many UTF-8 bytes the rendered text occupies.
    pub rendered_bytes: usize,
    /// Whether invalid UTF-8 required replacement characters.
    pub lossy: bool,
    /// Whether older bytes or expanded replacement text were omitted.
    pub truncated: bool,
}

/// A JSON-safe projection of one operation's typed value.
#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanValue {
    /// tmux accepted an operation with no other answer.
    Acknowledged,
    /// A session and the first window and pane it created.
    CreatedSession {
        /// The created session ID.
        session: String,
        /// The first window ID.
        window: String,
        /// The first pane ID.
        pane: String,
    },
    /// A window and its first pane.
    CreatedWindow {
        /// The created window ID.
        window: String,
        /// The first pane ID.
        pane: String,
    },
    /// A pane created by splitting another pane.
    CreatedPane {
        /// The created pane ID.
        pane: String,
    },
    /// Pane bytes rendered for JSON.
    CapturedPane {
        /// The bounded pane contents.
        output: PlanEvidence,
    },
}

/// Divide the response budget fairly between every returned text stream.
pub(super) fn plan_evidence_limit(streams: usize) -> usize {
    PLAN_EVIDENCE_BYTES.checked_div(streams).unwrap_or(0)
}

/// Keep the newest part of tmux output within one stream's rendered budget.
pub(super) fn plan_evidence(bytes: &[u8], limit: usize) -> PlanEvidence {
    let raw_truncated = bytes.len() > limit;
    let retained = if raw_truncated {
        &bytes[bytes.len() - limit..]
    } else {
        bytes
    };
    let lossy = std::str::from_utf8(retained).is_err();
    let rendered = String::from_utf8_lossy(retained);
    let mut start = rendered.len().saturating_sub(limit);
    while !rendered.is_char_boundary(start) {
        start += 1;
    }
    let text = rendered[start..].to_owned();
    PlanEvidence {
        rendered_bytes: text.len(),
        text,
        bytes: bytes.len(),
        lossy,
        truncated: raw_truncated || start > 0,
    }
}

pub(super) fn project_plan_value(
    value: &CoreOperationValue,
    evidence_limit: usize,
) -> Option<PlanValue> {
    match value {
        CoreOperationValue::Acknowledged => Some(PlanValue::Acknowledged),
        CoreOperationValue::CreatedSession {
            session,
            window,
            pane,
        } => Some(PlanValue::CreatedSession {
            session: session.to_string(),
            window: window.to_string(),
            pane: pane.to_string(),
        }),
        CoreOperationValue::CreatedWindow { window, pane } => Some(PlanValue::CreatedWindow {
            window: window.to_string(),
            pane: pane.to_string(),
        }),
        CoreOperationValue::CreatedPane { pane } => Some(PlanValue::CreatedPane {
            pane: pane.to_string(),
        }),
        CoreOperationValue::CapturedPane(text) => Some(PlanValue::CapturedPane {
            output: plan_evidence(text.as_bytes(), evidence_limit),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod plan_projection_tests {
    use super::*;

    #[test]
    fn capture_projection_reports_loss_and_truncation() {
        let mut bytes = vec![b'x'; PLAN_EVIDENCE_BYTES + 1];
        bytes[PLAN_EVIDENCE_BYTES] = 0xff;
        let value = libtmux::plan::OperationValue::CapturedPane(TmuxText::from(bytes));

        let Some(PlanValue::CapturedPane { output }) =
            project_plan_value(&value, PLAN_EVIDENCE_BYTES)
        else {
            panic!("a pane capture projects as pane text");
        };

        assert_eq!(output.bytes, PLAN_EVIDENCE_BYTES + 1);
        assert!(output.rendered_bytes <= PLAN_EVIDENCE_BYTES);
        assert!(output.lossy);
        assert!(output.truncated);
    }

    #[test]
    fn plan_evidence_retains_a_bounded_tail() {
        let mut bytes = vec![b'a'; PLAN_EVIDENCE_BYTES + 4];
        bytes[PLAN_EVIDENCE_BYTES..].copy_from_slice(b"tail");

        let evidence = plan_evidence(&bytes, PLAN_EVIDENCE_BYTES);

        assert_eq!(evidence.bytes, PLAN_EVIDENCE_BYTES + 4);
        assert!(evidence.truncated);
        assert_eq!(evidence.rendered_bytes, PLAN_EVIDENCE_BYTES);
        assert!(evidence.text.ends_with("tail"));
    }

    #[test]
    fn plan_evidence_shares_one_response_budget() {
        let limit = plan_evidence_limit(3);
        let evidence = plan_evidence(&vec![0xff; PLAN_EVIDENCE_BYTES], limit);

        assert!(evidence.rendered_bytes * 3 <= PLAN_EVIDENCE_BYTES);
        assert!(evidence.lossy);
        assert!(evidence.truncated);
    }
}

/// Arguments for renaming an object.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RenameArgs {
    /// The `$`-prefixed session id or `@`-prefixed window id to rename.
    pub target: String,
    /// The new name.
    pub name: String,
}

/// Arguments carrying a portable filter expression, and what to apply it to.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FilterArgs {
    /// A libtmux filter expression envelope.
    ///
    /// `{"version": 1, "target": "pane", "expr": {...}}`. The same grammar the
    /// TypeScript port speaks, so an agent can build one expression and use it
    /// against either.
    pub filter: libtmux::query::FilterExpr<libtmux::Pane>,
    /// Only consider panes in this session, by name.
    ///
    /// An expression names one object's own fields, so a pane cannot ask
    /// about its session. Narrowing first is how that question is asked, and
    /// it is what tmux itself does with a target.
    pub session: Option<String>,
    /// Only consider panes in this window, by `@`-prefixed id.
    pub window: Option<String>,
}

/// Arguments carrying a portable filter over the whole hierarchy.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TreeFilterArgs {
    /// A libtmux filter expression envelope targeting `session_tree`.
    ///
    /// `{"version": 1, "target": "session_tree", "expr": {...}}`. A session's
    /// own fields are named directly; `windows` is a relation taking `any`,
    /// `all`, or `none`, and a window's `panes` is another.
    pub filter: libtmux::query::FilterExpr<libtmux::SessionTree>,
}

/// Arguments for moving focus between panes.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SelectPaneArgs {
    /// The `%`-prefixed pane to select, or to move relative to.
    pub pane: String,
    /// Move relative to that pane instead of selecting it.
    ///
    /// `up`, `down`, `left`, and `right` follow the layout, so `up` selects
    /// whatever pane is drawn above. `last` returns to the previously active
    /// pane, and `next` and `previous` step through the window's panes in
    /// order. Omit to select the named pane itself.
    #[schemars(with = "Option<SelectPaneDirectionSchema>")]
    pub direction: Option<String>,
}

/// Arguments for running a command and waiting for it.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunCommandArgs {
    /// The `%`-prefixed pane to run in.
    pub pane: String,
    /// The shell command to run.
    ///
    /// It runs inside a subshell, so several lines are fine and a bare `exit`
    /// does not end the pane's own shell. An unbalanced quote or bracket does
    /// leave that shell waiting for the rest, which shows up as a run that
    /// reaches its deadline having produced nothing.
    pub command: String,
    /// How long to allow, in seconds. Defaults to 30, capped at 600.
    pub seconds: Option<u64>,
    /// Whether to keep the command out of the shell's history.
    #[serde(default)]
    pub suppress_history: bool,
}

/// Arguments for starting a command that outlives this call.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StartCommandArgs {
    /// The `%`-prefixed pane to run in.
    pub pane: String,
    /// The shell command to run.
    ///
    /// It runs inside a subshell, so several lines are fine and a bare `exit`
    /// does not end the pane's own shell.
    pub command: String,
    /// Whether to keep the command out of the shell's history.
    #[serde(default)]
    pub suppress_history: bool,
}

/// Arguments for asking how a background command is getting on.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JobStatusArgs {
    /// The job id `start_command` returned.
    pub job: String,
    /// The cursor from the previous call, to read only what is new.
    ///
    /// Omit it to read the command's output from the beginning.
    pub cursor: Option<u64>,
    /// Seconds to wait for the job to finish before answering.
    ///
    /// Omitted or zero answers immediately, which is the cheap poll. A value
    /// here returns as soon as the job ends rather than at the deadline, so
    /// waiting costs nothing when the job was already over.
    pub seconds: Option<u64>,
}

/// Arguments for forgetting a background command.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ForgetJobArgs {
    /// The job id `start_command` returned.
    pub job: String,
}

/// Arguments for asking which windows have written lately.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WhatChangedArgs {
    /// Report only windows that wrote after this time.
    ///
    /// Pass back the `now` from the previous call. Omit it to see every
    /// window ordered by how recently it wrote.
    pub since: Option<i64>,
}

/// Arguments for expanding a tmux format.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FormatArgs {
    /// The tmux format to expand, such as `#{pane_unseen_changes}`.
    pub format: String,
    /// The `%`-prefixed pane to expand it against.
    ///
    /// Pane, window and session formats all need one, because tmux resolves
    /// the window and session from the pane.
    pub pane: Option<String>,
}

/// Arguments for reading a tmux environment.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ShowEnvironmentArgs {
    /// The session whose environment to read. Omit for the server's own.
    pub session: Option<String>,
}

/// Arguments for writing a tmux environment.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetEnvironmentArgs {
    /// The variable name.
    pub name: String,
    /// The value to store. Omit to mark the variable for removal.
    pub value: Option<String>,
    /// The session to write it for. Omit for the server's own.
    pub session: Option<String>,
}

/// Arguments for reading the hooks set at a scope.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ShowHooksArgs {
    /// The session whose hooks to read. Omit for the server's own.
    pub session: Option<String>,
}

/// Arguments for piping a pane somewhere.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PipePaneArgs {
    /// The `%`-prefixed pane to pipe.
    pub pane: String,
    /// The shell command to feed the pane's output to.
    ///
    /// Omit to stop piping. tmux runs this itself, so it outlives this
    /// server: a pipe left on keeps writing after the agent has gone.
    pub command: Option<String>,
}

/// Arguments for arranging a window's panes.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SelectLayoutArgs {
    /// The `@`-prefixed window to arrange.
    pub window: String,
    /// A named layout, or a layout string tmux produced earlier.
    ///
    /// The names are `even-horizontal`, `even-vertical`, `main-horizontal`,
    /// `main-vertical` and `tiled`.
    pub layout: String,
}

/// Arguments for restarting what a pane runs.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RespawnPaneArgs {
    /// The `%`-prefixed pane to restart.
    pub pane: String,
    /// The command to run in it. Omit to rerun the one it started with.
    pub command: Option<String>,
    /// Restart even when the pane's process is still alive.
    ///
    /// Without this a live pane is left alone, which is what keeps a
    /// mistyped id from destroying work.
    #[serde(default)]
    pub kill_first: bool,
}

/// Arguments for putting text into a pane without typing it.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PasteTextArgs {
    /// The `%`-prefixed pane to paste into.
    pub pane: String,
    /// The text to deliver.
    pub text: String,
}

/// Arguments for waiting until a pane goes quiet.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitForIdleArgs {
    /// The `%`-prefixed pane to watch.
    pub pane: String,
    /// How many seconds of silence count as quiet. Defaults to 2.
    pub quiet_seconds: Option<u64>,
    /// How long to allow in total, in seconds. Defaults to 30, capped at 600.
    pub seconds: Option<u64>,
}

/// Arguments for waiting until a pane says something.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitForTextArgs {
    /// The `%`-prefixed pane to watch.
    pub pane: String,
    /// Text that ends the wait successfully. Omit to wait for any output.
    pub patterns: Option<Vec<String>>,
    /// Text that ends the wait as a failure, reported as `stopped`.
    ///
    /// Give the failure markers you already know — `error:`, `Traceback` — and
    /// a failed run returns at once instead of at the deadline.
    pub stop: Option<Vec<String>>,
    /// Read both lists as regular expressions rather than literal text.
    #[serde(default)]
    pub regex: bool,
    /// Match case. Off by default.
    #[serde(default)]
    pub match_case: bool,
    /// How long to wait, in seconds. Defaults to 30, capped at 600.
    pub seconds: Option<u64>,
}

/// Arguments for reading what a pane wrote since last time.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CaptureSinceArgs {
    /// The `%`-prefixed pane to read.
    pub pane: String,
    /// The cursor from the previous call. Omit to start watching.
    pub cursor: Option<String>,
}

/// Arguments for moving focus between windows.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SelectWindowArgs {
    /// The `@`-prefixed window to select, or to move relative to.
    pub window: String,
    /// Move relative to that window instead of selecting it.
    ///
    /// `next` and `previous` step through the session in index order, and
    /// `last` returns to the previously active window. Omit to select the
    /// named window itself.
    #[schemars(with = "Option<SelectWindowDirectionSchema>")]
    pub direction: Option<String>,
}

/// Arguments for searching what panes are showing.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchPanesArgs {
    /// The text to look for.
    pub pattern: String,
    /// Read the pattern as a regular expression rather than literal text.
    #[serde(default)]
    pub regex: bool,
    /// Match case. Off by default.
    #[serde(default)]
    pub match_case: bool,
    /// Search scrollback as well as the visible screen.
    #[serde(default)]
    pub history: bool,
    /// Only search panes in this session, by name.
    pub session: Option<String>,
    /// Only search panes in this window, by `@`-prefixed id.
    pub window: Option<String>,
}

/// Arguments for reading or writing a tmux option.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OptionArgs {
    /// The option name, such as `history-limit` or a user option like `@theme`.
    pub name: String,
    /// Which tmux object the option belongs to.
    ///
    /// One of `server`, `global-session`, `global-window`, `session`,
    /// `window`, or `pane`. Defaults to `global-session`, which is what
    /// setting an option without a target means in tmux.
    #[schemars(with = "Option<OptionScopeSchema>")]
    pub scope: Option<String>,
    /// The `$`, `@` or `%`-prefixed id, for the scopes that need one.
    pub target: Option<String>,
    /// The value to set. Omit to read the option instead.
    ///
    /// A number or boolean is accepted as well as a string, because tmux
    /// stores every option as text and an agent setting `history-limit`
    /// naturally writes `5000` rather than `"5000"`. Refusing that is a
    /// deserialization error with no tmux in it, which is the least useful
    /// kind of failure to hand back.
    pub value: Option<OptionValueArg>,
}

/// A tmux option value as an agent is likely to write it.
///
/// tmux has one type on the wire -- text -- so a number or a boolean here is
/// not a different kind of value, only a different spelling of one.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum OptionValueArg {
    /// Written as a string, which is what tmux ultimately stores.
    Text(String),
    /// Written as a whole number, which every tmux size and limit is.
    ///
    /// Tried before the fractional form, so `5000` keeps its exact spelling
    /// instead of arriving as a float and being rendered back.
    Integer(i64),
    /// Written with a fraction. tmux has no such option, so this exists to
    /// carry the value through to tmux and let tmux reject it, rather than
    /// failing here with an error that never reached a terminal.
    Number(f64),
    /// Written as a boolean, for the on/off options.
    Flag(bool),
}

impl std::fmt::Display for OptionValueArg {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => formatter.write_str(text),
            Self::Integer(number) => write!(formatter, "{number}"),
            Self::Number(number) => write!(formatter, "{number}"),
            Self::Flag(true) => formatter.write_str("on"),
            Self::Flag(false) => formatter.write_str("off"),
        }
    }
}

/// Arguments for reading a pane's whole state.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SnapshotArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// The most content lines to return, oldest dropped first.
    ///
    /// Defaults to the whole visible screen. The end of a pane is what says
    /// what just happened, so a limit keeps the end.
    pub max_lines: Option<usize>,
    /// Include scrollback rather than only the visible screen.
    #[serde(default)]
    pub history: bool,
}

/// Arguments naming a `wait-for` channel.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ChannelArgs {
    /// The channel name, which is any string both sides agree on.
    pub channel: String,
    /// How long to wait, in seconds. Defaults to 30, capped at 600.
    pub seconds: Option<u64>,
}

/// Arguments for splitting a pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SplitPaneArgs {
    /// The `%`-prefixed pane id to divide.
    pub pane: String,
    /// Where the new pane goes: `above`, `below`, `left`, or `right`.
    ///
    /// Defaults to `below`.
    #[schemars(with = "Option<SplitDirectionSchema>")]
    pub direction: Option<String>,
    /// How much of the divided space the new pane takes, as a percentage.
    pub percent: Option<u32>,
    /// A command to run instead of the default shell.
    pub command: Option<String>,
}

/// Arguments for resizing a pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ResizePaneArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// Which edge to move: `up`, `down`, `left`, or `right`.
    #[schemars(with = "ResizeDirectionSchema")]
    pub direction: String,
    /// How many rows or columns to move it by.
    pub cells: u32,
}

/// Arguments for reading a pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CapturePaneArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// Read the whole history rather than the visible screen.
    #[serde(default)]
    pub history: bool,
    /// Return only the last command's output, when the shell marks its
    /// prompts.
    ///
    /// Answers far less than the history, because it starts where the last
    /// command's output began. When the pane's shell does not mark its
    /// prompts -- fish does, bash and zsh do not -- this reports
    /// `marks: "absent"` and returns the visible screen instead.
    #[serde(default)]
    pub last_command: bool,
    /// Start at this line. Zero is the top of the screen, negative is
    /// scrollback.
    pub start: Option<i32>,
    /// End at this line.
    pub end: Option<i32>,
}

/// Arguments for watching a pane produce output.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WatchPaneArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// How long to watch, in seconds. Capped at one minute.
    pub seconds: u64,
    /// Stop early once this many bytes have arrived. Capped at 64 KiB.
    pub max_bytes: Option<usize>,
}

/// What a pane produced while it was watched.
#[derive(Debug, Serialize)]
pub struct WatchView {
    /// The pane that was watched.
    pub pane: String,
    /// What the pane wrote, with terminal escapes left in place.
    pub output: String,
    /// How many bytes arrived.
    pub bytes: usize,
    /// Why watching stopped.
    pub stopped: &'static str,
}

/// Arguments for sending input to a pane.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendKeysArgs {
    /// The `%`-prefixed pane id.
    pub pane: String,
    /// Text typed literally into the pane. Key names are not interpreted.
    pub text: Option<String>,
    /// tmux key names to press, in order, after any text.
    ///
    /// These are interpreted rather than typed, which is the only way to send
    /// a key that has no character: `C-c` to interrupt, `Escape`, `Up`,
    /// `C-d`. Sending `C-c` as `text` would type those three characters.
    pub keys: Option<Vec<String>>,
    /// Whether to press Enter afterwards.
    #[serde(default)]
    pub enter: bool,
}
