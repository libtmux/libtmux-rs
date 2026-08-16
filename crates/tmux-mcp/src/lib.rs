//! A Model Context Protocol server exposing tmux through `libtmux`.
//!
//! This crate exists to exercise the `libtmux` public API from outside, the
//! way a real consumer would. It presents a small, deliberately read-biased
//! set of tools: an agent can inspect a tmux server freely, but every tool
//! that changes state names exactly what it changes.
//!
//! The dependency runs one way. `libtmux` knows nothing about MCP.
//!
//! # Knowing where you are
//!
//! When tmux starts this process, it says so through `TMUX` and `TMUX_PANE`.
//! Every pane a listing returns carries a `caller` field naming its relation
//! to this server — `self`, `other`, or `unknown` — and the tools that destroy
//! things refuse to destroy the pane the conversation runs through. There is
//! no `whoami` tool because there is no question left to ask.
//!
//! Both rest on comparing the socket as well as the pane id. `%1` names a
//! different pane on every server, so an id alone proves nothing.
//!
//! # Watching a pane
//!
//! Five tools read a pane, and they differ in what they can promise.
//!
//! | Tool | Reads | Use it for |
//! | --- | --- | --- |
//! | `capture_pane` | the rendered screen | what a pane looks like now |
//! | `snapshot_pane` | the screen and its state | where the pane is, not just what it says |
//! | `watch_pane` | the raw byte stream | bytes, escape sequences included |
//! | `wait_for_text` | the stream, filtered | "tell me when it says ready" |
//! | `capture_since` | a live tail | "what changed since I last looked" |
//!
//! The last three read what the pane wrote rather than what survived
//! rendering, so output that scrolled past between calls is still seen and no
//! scrollback anchor can be invalidated underneath them.
//!
//! `snapshot_pane` is the one that answers questions about the pane rather
//! than its text: the cursor position that distinguishes a shell waiting from
//! a command still running, and the copy mode that would swallow keys.
//!
//! To run something and learn whether it worked, use `run_command`: it reports
//! the exit status rather than leaving an agent to infer success from text.
//! Its deadline ends the waiting rather than the command, so a pane left busy
//! by one call is a pane the next call cannot type at.
//!
//! # Driving a pane
//!
//! `send_keys` both types and presses. Its `text` is sent literally, so `C-c`
//! there types three characters; its `keys` are tmux key names, which is the
//! only way to press something with no character of its own — `C-c` to
//! interrupt, `Escape`, `Up`. That is how a pane wedged by a timed-out run,
//! or sitting in copy mode, is recovered.
//!
//! # Asking about layout
//!
//! Position needs no tool of its own. `find_panes` takes a filter expression
//! over a pane's own format fields, which include `pane_at_top`,
//! `pane_at_bottom`, `pane_at_left` and `pane_at_right` as booleans, and
//! `pane_left`, `pane_right`, `pane_top`, `pane_bottom`, `pane_x` and `pane_y`
//! as coordinates. So "the bottom-right pane" is one expression:
//!
//! ```json
//! {"version": 1, "target": "pane", "expr": {"op": "and", "args": [
//!   {"op": "eq", "field": "pane_at_bottom", "value": true},
//!   {"op": "eq", "field": "pane_at_right", "value": true}
//! ]}}
//! ```
//!
//! Changing focus is an action rather than a question, so those are tools:
//! `select_pane` and `select_window`.
//!
//! To find *where* something is rather than what a known pane holds,
//! `search_panes` matches across every pane at once and reports the pane and
//! line of each hit. It is the difference between one call and one call per
//! pane when the question is "which pane printed the error".

#![forbid(unsafe_code)]

pub mod cli;
pub mod resources;

mod caller;
mod exec;
mod jobs;
mod tail;
mod text;
mod views;

pub use caller::{CallerIdentity, Relation};
pub use exec::{IdleOutcome, IdleView, RunOutcome, RunView, WaitOutcome, WaitView};
pub use tail::Cursor;
pub use views::*;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libtmux::plan::{Outcome as PlanOutcome, Plan, Planner, Safety as PlanSafety};
use libtmux::query::{FilterExpr, QueryIteratorExt as _};
use libtmux::{
    CaptureOptions, Command, NewSessionOptions, PaneSize, ResizeDirection, Server, SplitDirection,
    SplitOptions, TmuxText,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ErrorData, ServerCapabilities, ServerInfo};
use rmcp::model::{PromptMessage, Role};
use rmcp::{
    ServerHandler, prompt, prompt_handler, prompt_router, schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

use exec::Patterns;
use jobs::Jobs;
use tail::Tails;

/// A tmux server presented as MCP tools.
#[derive(Clone)]
pub struct TmuxTools {
    server: Arc<Server>,
    /// Where this process is running, when tmux started it.
    caller: Option<Arc<CallerIdentity>>,
    /// How much of the surface this server was told to offer.
    safety: Safety,
    /// The server's own socket path, resolved once and kept.
    ///
    /// `Server::socket_path` reports what this crate was configured with,
    /// which for a named socket is a reconstruction. tmux knows the real one,
    /// and every caller comparison rests on it.
    socket: Arc<OnceLock<Option<PathBuf>>>,
    /// Live per-pane output, for `capture_since`.
    tails: Arc<Tails>,
    /// Commands still running, for `job_status`.
    jobs: Arc<Jobs>,
    /// The recipes this server offers alongside its tools.
    prompt_router: rmcp::handler::server::router::prompt::PromptRouter<Self>,
    /// The tools this server offers, after the tier has taken its cut.
    ///
    /// Named in the `tool_handler` attribute below. Without that the macro
    /// defaults to `Self::tool_router()`, building a fresh router per request
    /// and silently discarding whatever this held.
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

// The resolved socket path stays out, as `ServerIdentity`'s own `Debug` keeps
// it out: this server's logs go wherever the agent's do.
impl std::fmt::Debug for TmuxTools {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TmuxTools")
            .field("server", &self.server)
            .field("caller", &self.caller)
            .field("safety", &self.safety)
            .finish_non_exhaustive()
    }
}

/// Render tmux bytes for a protocol that requires valid UTF-8.
///
/// tmux permits names and titles that are not UTF-8. JSON cannot carry those
/// bytes, so they are replaced rather than dropping the whole response.
fn lossy(value: &TmuxText) -> String {
    value.to_string_lossy().into_owned()
}

/// The same, for a field tmux may genuinely not report.
fn lossy_optional(value: Option<&TmuxText>) -> Option<String> {
    value.map(lossy)
}

/// The most a single `watch_pane` call will return.
///
/// A pane can produce output faster than any consumer reads it, so the ceiling
/// belongs here rather than in the caller's hands.
const WATCH_BYTES: usize = 64 * 1024;

/// What the server tells a client before its first call.
///
/// Composed from named pieces so that adding one is a decision rather than a
/// habit. Before adding a segment here, try the relevant tool's own
/// description first: an agent meets that at the moment it is choosing, while
/// this is read once and competes with everything else in the context. A
/// segment earns its place only when the thing it says is *server*-shaped —
/// true across tools, or about a tool that does not exist.
const INSTRUCTIONS: &str = concat!(
    // What this is, and how to name things in it.
    "Inspect and drive a tmux server. The hierarchy is Server > Session > \
     Window > Pane. Prefer ids for targeting: %1 is a pane, @1 a window, $1 a \
     session, and each is unique within one tmux server.",
    // When to reach for this at all. These words belong to other things too,
    // and picking wrong is worse than asking.
    "\n\nTRIGGERS: tmux objects — panes, windows, sessions, splits, scrollback, \
     copy mode, \"this terminal\", \"send keys\". The %, @ and $ id prefixes are \
     unambiguous. NOT for browser windows or tabs, editor splits (VS Code, \
     Neovim), desktop windows (i3, sway, Hyprland), Jupyter cells, or login \
     sessions. On a bare \"window\" or \"session\" with no other clue, ask one \
     clarifying question first.",
    // The confusion this shape of API reliably produces.
    "\n\nNAMES ARE NOT CONTENTS: list_sessions, list_windows and list_panes \
     report names, ids and the running command. They do not look at what a \
     terminal is showing. For what a pane displays, mentions or contains, use \
     search_panes across every pane, capture_pane for one, snapshot_pane to \
     also learn where its cursor is, or capture_since to follow one over \
     several turns.",
    // The habit worth breaking before it starts.
    "\n\nWAIT, DO NOT POLL: run_command runs something and reports its exit \
     status. start_command does the same without holding the call, for \
     anything slow or for several at once; poll it with job_status. \
     wait_for_text watches for output you did not author, and wait_for_idle \
     waits for a pane to go quiet when you cannot name what to look for. \
     capture_since returns only what is new since your last look. Reading \
     capture_pane in a loop is slower and misses whatever scrolled past \
     between reads.",
    // What to do about a failure, since the useful answer differs and reading
    // it off the message means guessing.
    "\n\nWHEN A CALL FAILS: every error carries kind, retryable and stale \
     alongside its message. stale means the target is gone — list again and \
     work from what you find, because repeating the call cannot work. \
     retryable means the same call could succeed on its own; nothing else is \
     worth repeating unchanged.",
    // Absences, so an agent stops hunting for them. These are server-shaped:
    // whole families that are missing rather than one tool's caveat.
    "\n\nNOT HERE: copy mode has no tools; leave it by sending the key q with \
     send_keys. Hooks are read-only, because one set from here would vanish \
     with this process. For any field tmux publishes that no tool carries, \
     use expand_format. Anything else tmux can do is reachable by running the \
     tmux command itself with run_command.",
);

/// The environment variable naming how much of the surface is offered.
pub const SAFETY_ENV: &str = "TMUX_MCP_SAFETY";

/// How much of the tool surface an operator has allowed.
///
/// A refusal an agent can see coming is better than one it discovers, so a
/// tier does not merely reject calls: the tools above it are not advertised at
/// all. An agent cannot choose what it cannot see, which is the difference
/// between a policy and a warning.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Safety {
    /// Only the tools that change nothing.
    ReadOnly,
    /// Everything except the tools that destroy work.
    ///
    /// The default. This server can end every session on a machine, and the
    /// caller guard only protects the pane the agent is talking through — it
    /// says nothing about anyone else's work. Reaching that far should be a
    /// decision an operator made rather than one they inherited.
    #[default]
    Mutating,
    /// Everything, including the four tools that destroy work.
    Destructive,
}

impl Safety {
    /// Read the tier from the environment, falling back to the default.
    ///
    /// An unreadable value is not worth refusing to start over, and widening
    /// the surface on a typo would be the wrong way to fail, so anything
    /// unrecognised leaves the default in place.
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var(SAFETY_ENV)
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .unwrap_or_default()
    }

    /// Read a tier by name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "readonly" | "read-only" => Some(Self::ReadOnly),
            "mutating" => Some(Self::Mutating),
            "destructive" => Some(Self::Destructive),
            _ => None,
        }
    }

    /// The name this tier is set by.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "readonly",
            Self::Mutating => "mutating",
            Self::Destructive => "destructive",
        }
    }

    /// Whether a tool carrying these annotations is offered at this tier.
    ///
    /// Decided from the annotations themselves rather than a list of names.
    /// A list is a second place to say what a tool does, and the two drift:
    /// the tool that gets mislabelled is the one nobody thought to add.
    fn admits(self, tool: &rmcp::model::Tool) -> bool {
        let hints = tool.annotations.as_ref();
        let reads = hints.and_then(|hints| hints.read_only_hint) == Some(true);
        let destroys = hints.and_then(|hints| hints.destructive_hint) == Some(true);
        match self {
            Self::ReadOnly => reads,
            Self::Mutating => !destroys,
            Self::Destructive => true,
        }
    }
}

impl Safety {
    /// Whether this tier admits one plan operation.
    ///
    /// The same rule the tool annotations get, read from the operation's own
    /// declared safety rather than from a second list that would drift.
    const fn admits_operation(self, safety: PlanSafety) -> bool {
        match self {
            Self::ReadOnly => matches!(safety, PlanSafety::ReadOnly),
            Self::Mutating => !matches!(safety, PlanSafety::Destructive),
            Self::Destructive => true,
        }
    }
}

/// Assembles a [`TmuxTools`] with the parts the environment usually supplies.
#[derive(Debug)]
pub struct Builder {
    server: Server,
    caller: Option<CallerIdentity>,
    safety: Safety,
}

impl Builder {
    /// Say where this process is running, rather than reading the environment.
    #[must_use]
    pub fn caller(mut self, caller: Option<CallerIdentity>) -> Self {
        self.caller = caller;
        self
    }

    /// Say how much of the surface to offer, rather than reading the
    /// environment.
    #[must_use]
    pub const fn safety(mut self, safety: Safety) -> Self {
        self.safety = safety;
        self
    }

    /// Build the server, offering only the tools the tier admits.
    #[must_use]
    pub fn build(self) -> TmuxTools {
        let mut router = TmuxTools::tool_router();
        // Taken from the router's own advertisement, so the tier reads exactly
        // what a client would.
        let withheld: Vec<String> = router
            .list_all()
            .iter()
            .filter(|tool| !self.safety.admits(tool))
            .map(|tool| tool.name.to_string())
            .collect();
        for name in withheld {
            router.remove_route(&name);
        }
        for route in router.map.values_mut() {
            strip_unknown_formats(Arc::make_mut(&mut route.attr.input_schema));
            if let Some(schema) = route.attr.output_schema.as_mut() {
                strip_unknown_formats(Arc::make_mut(schema));
            }
        }

        TmuxTools {
            server: Arc::new(self.server),
            caller: self.caller.map(Arc::new),
            safety: self.safety,
            socket: Arc::new(OnceLock::new()),
            tails: Arc::new(Tails::new()),
            jobs: Arc::new(Jobs::new()),
            tool_router: router,
            prompt_router: TmuxTools::prompt_router(),
        }
    }
}

/// Arguments naming one pane, for a prompt.
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PanePrompt {
    /// The `%`-prefixed pane id.
    pub pane: String,
}

/// Arguments for the run-and-wait recipe.
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RunPrompt {
    /// The `%`-prefixed pane id to run in.
    pub pane: String,
    /// The shell command to run.
    pub command: String,
}

/// Recipes for the tool combinations that are easy to get wrong.
///
/// Three, and no more without a reason. A prompt earns its place by teaching
/// a *composition* — something no single tool's description can say, because
/// it is about which tool to reach for and in what order. Anything that fits
/// in one tool's description belongs there instead, where an agent meets it
/// while choosing.
#[allow(
    missing_docs,
    reason = "the prompt macro generates a metadata function without a doc attribute, \
              unlike the tool macro, which does; the prompts themselves are documented"
)]
#[prompt_router]
impl TmuxTools {
    /// Run a command and act on its exit status.
    #[prompt(
        name = "run_and_wait",
        title = "Run A Command And Wait",
        description = "Run a shell command in a pane and act on how it finished, rather \
                       than typing it and reading the screen afterwards."
    )]
    pub async fn run_and_wait(
        &self,
        Parameters(RunPrompt { pane, command }): Parameters<RunPrompt>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "In tmux pane {pane}, run:\n\n    {command}\n\n\
                 Use run_command, not send_keys. It waits for the command to finish and \
                 comes back with an exit_status and the output the command itself wrote — \
                 no prompt, no echo, nothing that scrolled past. Decide from exit_status; \
                 read output only to explain it.\n\n\
                 Two answers are not failures and should not be retried blindly. \
                 outcome=deadline means the time ran out and the command is still running, \
                 so the pane is busy: either wait again or stop it with send_keys \
                 keys=[\"C-c\"]. outcome=no_shell means the pane was not at a prompt, so \
                 the text went into whatever is running there; look with snapshot_pane \
                 before trying again."
            ),
        )]
    }

    /// Get a wedged pane back to a prompt.
    #[prompt(
        name = "interrupt_gracefully",
        title = "Interrupt A Busy Pane",
        description = "Stop whatever a pane is running and get it back to a shell prompt, \
                       without killing the pane."
    )]
    pub async fn interrupt_gracefully(
        &self,
        Parameters(PanePrompt { pane }): Parameters<PanePrompt>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Get tmux pane {pane} back to a shell prompt without destroying it.\n\n\
                 Look first with snapshot_pane: it reports the cursor and whether the pane \
                 is in copy mode. A pane in copy mode is not busy at all — it is just not \
                 listening, and q leaves it.\n\n\
                 To stop a running command, send the key rather than the letters: \
                 send_keys with keys=[\"C-c\"]. Passing \"C-c\" as text types three \
                 characters at the command instead of interrupting it.\n\n\
                 Give it a moment and check again — a shell takes a beat to reclaim the \
                 terminal. If C-c does not take, C-\\\\ is stronger. Reach for kill_pane \
                 only when the pane itself is beyond saving, and remember that it destroys \
                 whatever was in it."
            ),
        )]
    }

    /// Work out what a pane is doing.
    #[prompt(
        name = "diagnose_pane",
        title = "Diagnose A Pane",
        description = "Work out what a pane is doing and why, using the tools in the order \
                       that answers it in fewest calls."
    )]
    pub async fn diagnose_pane(
        &self,
        Parameters(PanePrompt { pane }): Parameters<PanePrompt>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Work out what tmux pane {pane} is doing and report it.\n\n\
                 Start with snapshot_pane: one call gives what the pane shows, what is \
                 running in it, the cursor, and whether it is in copy mode or dead. A \
                 cursor at the start of a fresh line usually means a shell waiting; a \
                 pane whose command is not a shell is busy.\n\n\
                 If the visible screen is not enough, capture_pane with history=true \
                 reaches what has scrolled away. To follow it as it goes, take a cursor \
                 from capture_since and call again later with it — that returns only what \
                 is new, rather than the whole screen each time.\n\n\
                 If you are not sure this is even the right pane, search_panes finds which \
                 pane is showing a piece of text. The listing tools will not: they read \
                 names and commands, not what a terminal displays."
            ),
        )]
    }
}

/// Report a job id this server does not hold.
///
/// Classified `stale` rather than as bad input: a job is forgotten when it
/// ages out, so listing again is what helps, not a different argument.
fn unknown_job(job: &str) -> ErrorData {
    let mut data = serde_json::Map::new();
    data.insert("kind".into(), "object_gone".into());
    data.insert("retryable".into(), false.into());
    data.insert("stale".into(), true.into());

    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_PARAMS,
        format!("no job {job}; it finished long enough ago to be forgotten, or never existed"),
        Some(serde_json::Value::Object(data)),
    )
}

/// Marks a tool a client should keep loaded rather than defer.
///
/// Claude Code stops sending MCP tool schemas to the model once they crowd the
/// context, and this server's are around 19 KB. A deferred schema means a bare
/// "what's in my pane" never reaches these tools at all.
///
/// Applied to three anchors only. Each one a client honours costs a fixed
/// share of that budget, so widening the set makes the hint worth less to
/// every tool that has it. Best-effort by design: a client that does not read
/// the `anthropic` namespace simply ignores it.
fn always_load() -> rmcp::model::MetaObject {
    let mut meta = rmcp::model::MetaObject::new();
    meta.0.insert(
        "anthropic/alwaysLoad".to_owned(),
        serde_json::Value::Bool(true),
    );
    meta
}

/// Separates the fields of a `snapshot_pane` format query.
///
/// U+241E rather than an ASCII control byte because tmux copies valid UTF-8
/// through verbatim, while `vis()` would render a control byte as the literal
/// text `\036` on some builds.
const SEPARATOR: &str = "\u{241e}";

/// The most matches a single `search_panes` call will report.
///
/// A pattern like `.` matches every line of every pane, and an agent that
/// asked for that wants a signal, not a transcript of the server.
const SEARCH_MATCHES: usize = 200;

/// Which tmux object an option belongs to.
///
/// Boxed because a `Session`, `Window` and `Pane` each carry their own
/// snapshot, and the enum is a short-lived dispatch rather than something
/// worth sizing to its largest arm.
enum OptionScope {
    /// The server's own options.
    Server,
    /// The session options a new session inherits.
    GlobalSession,
    /// The window options a new window inherits.
    GlobalWindow,
    /// One session's options.
    Session(Box<libtmux::Session>),
    /// One window's options.
    Window(Box<libtmux::Window>),
    /// One pane's options.
    Pane(Box<libtmux::Pane>),
}

// Every error this server returns carries the same three fields on its `data`,
// so an agent decides what to do next by reading them rather than by matching
// on prose. A pane that has closed and a tmux that is not running both fail;
// the first wants the listing refreshed, the second wants the agent to stop.
//
// * `kind` — a short name for what went wrong.
// * `retryable` — whether making the same call again could succeed.
// * `stale` — whether the target is gone, so a listing taken now would say
//   something different.
//
// The JSON-RPC code answers a different question: whose move it is. A caller
// who named a dead pane gets `invalid_params`; a pane that died between two of
// this server's own calls gets `internal_error`. Both are classified `stale`,
// because in both cases looking again is what helps.

/// Convert a tmux failure into a protocol error an agent can act on.
///
/// libtmux already draws the distinctions above, so they are carried through
/// rather than flattened.
fn tmux_error(error: &libtmux::Error) -> ErrorData {
    use libtmux::ErrorKind;

    let kind = error.kind();
    let detail = serde_json::json!({
        "kind": match kind {
            ErrorKind::ObjectGone => "object_gone",
            ErrorKind::Refused => "refused",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Unreachable => "unreachable",
            ErrorKind::UnsupportedVersion => "unsupported_version",
            ErrorKind::InvalidInput => "invalid_input",
            ErrorKind::Transport => "transport",
            ErrorKind::Decode => "decode",
            // ErrorKind is #[non_exhaustive]; a kind added upstream is
            // reported rather than mistaken for one of these.
            _ => "other",
        },
        "retryable": error.is_transient(),
        "stale": error.is_object_gone(),
    });
    let message = error.to_string();

    match kind {
        // The caller named something. Whether it is gone or was refused, the
        // request is what needs to change, so it is the caller's error.
        ErrorKind::ObjectGone | ErrorKind::Refused | ErrorKind::InvalidInput => {
            ErrorData::invalid_params(message, Some(detail))
        }
        _ => ErrorData::internal_error(message, Some(detail)),
    }
}

/// The classification for a target that is not where it was said to be.
///
/// Shared by the two ways this server discovers that itself, so a change to
/// what it promises cannot apply to one and not the other.
fn stale_detail() -> serde_json::Value {
    serde_json::json!({
        "kind": "object_gone",
        // Nothing will change on its own to make this id resolve; the caller
        // has to look again and name something else.
        "retryable": false,
        "stale": true,
    })
}

/// Drop `format` keywords that are not part of JSON Schema.
///
/// Rust's unsigned integers are described by schemars as `format: "uint32"`
/// and friends, which no JSON Schema dialect defines. Clients that validate
/// schemas log a line per occurrence -- one real client emitted forty-four on
/// a single listing -- and a strict validator may reject outright.
///
/// Nothing is lost by removing them: schemars already writes `minimum: 0`
/// beside each one, so `type: integer` plus that bound says everything the
/// format was there to say.
fn strip_unknown_formats(schema: &mut serde_json::Map<String, serde_json::Value>) {
    // The formats JSON Schema itself defines, plus the two integer widths
    // OpenAPI added that tooling widely understands.
    const KNOWN: &[&str] = &[
        "date-time",
        "date",
        "time",
        "duration",
        "email",
        "idn-email",
        "hostname",
        "idn-hostname",
        "ipv4",
        "ipv6",
        "uri",
        "uri-reference",
        "iri",
        "iri-reference",
        "uuid",
        "uri-template",
        "json-pointer",
        "relative-json-pointer",
        "regex",
        "int32",
        "int64",
        "float",
        "double",
    ];

    if schema
        .get("format")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|format| !KNOWN.contains(&format))
    {
        schema.remove("format");
    }
    for value in schema.values_mut() {
        strip_value(value);
    }
}

/// Walk into whatever shape a schema keyword holds.
fn strip_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => strip_unknown_formats(map),
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_value),
        _ => {}
    }
}

/// Report a target that a listing named but tmux no longer has.
///
/// The `find_*` helpers notice this themselves rather than learning it from
/// libtmux, so they mint the classification directly. An agent should not have
/// to tell the two apart: a pane that vanished between the listing and the call
/// reads the same either way.
fn object_gone(what: &str, id: &str) -> ErrorData {
    ErrorData::invalid_params(format!("no {what} {id}"), Some(stale_detail()))
}

/// Report state that moved between two calls this server made.
///
/// Not the caller's mistake — the handle was good when it was taken — so the
/// code stays an internal error. The classification is the one for a target
/// that was already gone, because the useful response is the same: look again.
fn vanished(message: &str) -> ErrorData {
    ErrorData::internal_error(message.to_owned(), Some(stale_detail()))
}

/// Report an argument this server will not pass to tmux.
///
/// Nothing about the server needs to change for the next call to work, and
/// nothing has gone stale: the caller has to send something else.
fn bad_input(message: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(
        message.into(),
        Some(serde_json::json!({
            "kind": "invalid_input",
            "retryable": false,
            "stale": false,
        })),
    )
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
    pub plan: serde_json::Value,
    /// How to group the plan: `sequential`, `folding`, or `marked`.
    ///
    /// Defaults to `sequential`, which is the only grouping that can say
    /// which operation failed.
    pub grouping: Option<String>,
}

/// What running a plan produced.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PlanRun {
    /// One outcome per operation, in the order the plan records them.
    pub outcomes: Vec<PlanStepOutcome>,
    /// How many tmux invocations it cost.
    pub dispatches: usize,
    /// Whether every operation is known to have succeeded.
    pub complete: bool,
}

/// One operation's outcome.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PlanStepOutcome {
    /// The tmux command the operation ran.
    pub command: String,
    /// `complete`, `failed`, `skipped`, or `unknown`.
    ///
    /// `unknown` means the operation shared a tmux invocation with a failure
    /// and nothing distinguishes them. Re-run with `grouping: "sequential"`
    /// to find out which one failed.
    pub outcome: String,
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
    pub filter: serde_json::Value,
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
    pub filter: serde_json::Value,
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

/// Arguments for stopping a background command.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CancelJobArgs {
    /// The job id `start_command` returned.
    pub job: String,
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
    /// Answers far less than the screen or the history, because it starts
    /// where the last command's output began. Reports `marks: "absent"` and
    /// falls back when the pane's shell does not mark its prompts, which is
    /// the common case outside fish.
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

#[tool_router]
impl TmuxTools {
    /// Expose one tmux server, locating this process within it.
    #[must_use]
    pub fn new(server: Server) -> Self {
        Self::builder(server).build()
    }

    /// Expose one tmux server, saying explicitly where this process is and how
    /// much of the surface it may use.
    ///
    /// The environment is process-wide, so a test that needs a caller or a
    /// tier cannot set one without disturbing every other test. This is how it
    /// says so instead.
    #[must_use]
    pub fn builder(server: Server) -> Builder {
        Builder {
            server,
            caller: CallerIdentity::from_env(),
            safety: Safety::from_env(),
        }
    }

    /// List every session on the server.
    #[tool(
        description = "List every tmux session on the server",
        title = "List Sessions",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn list_sessions(&self) -> Result<Json<Sessions>, ErrorData> {
        let sessions = self.server.sessions().await.map_err(|e| tmux_error(&e))?;
        Ok(Json(Self::render_sessions(&sessions)))
    }

    /// Describe sessions, shared by the tool and the `tmux://` resource so the
    /// two cannot drift into different accounts of the same session.
    fn render_sessions(sessions: &[libtmux::Session]) -> Sessions {
        Sessions {
            sessions: sessions
                .iter()
                .map(|session| SessionView {
                    id: session.id().to_string(),
                    name: lossy(session.name()),
                    windows: session.window_count(),
                    attached: session.is_attached(),
                })
                .collect(),
        }
    }

    /// List every window on the server, one row per session link.
    #[tool(
        description = "List every window on the server. A window linked into several sessions \
                       appears once per link, so an id can repeat with a different session_id.",
        title = "List Windows",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn list_windows(&self) -> Result<Json<Windows>, ErrorData> {
        let windows = self.server.windows().await.map_err(|e| tmux_error(&e))?;
        Ok(Json(Self::render_windows(&windows)))
    }

    /// List every pane on the server.
    #[tool(
        description = "List every pane on the server",
        title = "List Panes",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        meta = always_load()
    )]
    pub async fn list_panes(&self) -> Result<Json<Panes>, ErrorData> {
        let panes = self.server.panes().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(self.render_panes(&panes).await))
    }

    /// Report the whole hierarchy in one call.
    #[tool(
        description = "Report every session with its windows and panes, in one call. \
                       Prefer this over calling the three listing tools separately: \
                       it costs tmux three commands rather than one per object.",
        title = "Describe Server",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        meta = always_load()
    )]
    pub async fn describe(&self) -> Result<Json<Tree>, ErrorData> {
        let tree = self.server.hierarchy().await.map_err(|e| tmux_error(&e))?;
        let sessions: Vec<_> = tree
            .iter()
            .map(|branch| Branch {
                id: branch.session.id().to_string(),
                name: lossy(branch.session.name()),
                attached: branch.session.is_attached(),
                windows: branch
                    .windows
                    .iter()
                    .map(|built| BranchWindow {
                        id: built.window.id().to_string(),
                        index: built.window.index(),
                        name: lossy(built.window.name()),
                        active: built.window.is_active(),
                        linked: built.window.is_linked(),
                        panes: built
                            .panes
                            .iter()
                            .map(|pane| BranchPane {
                                id: pane.id().to_string(),
                                command: lossy_optional(pane.current_command()),
                                active: pane.is_active(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
            .collect();

        Ok(Json(Tree { sessions }))
    }

    /// List one session's windows.
    #[tool(
        description = "List the windows in one session, by session name",
        title = "List Windows In Session",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn list_session_windows(
        &self,
        Parameters(SessionArgs { session }): Parameters<SessionArgs>,
    ) -> Result<Json<Windows>, ErrorData> {
        let session = self.find_session(&session).await?;
        let windows = session.windows().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Self::render_windows(&windows)))
    }

    /// List one window's panes.
    #[tool(
        description = "List the panes in one window, by window id",
        title = "List Panes In Window",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn list_window_panes(
        &self,
        Parameters(WindowArgs { window }): Parameters<WindowArgs>,
    ) -> Result<Json<Panes>, ErrorData> {
        let window = self.find_window(&window).await?;
        let panes = window.panes().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(self.render_panes(&panes).await))
    }

    /// Create a window in one session.
    #[tool(
        description = "Create a window in a session, without selecting it",
        title = "Create Window",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn new_window(
        &self,
        Parameters(NewWindowArgs { session, name }): Parameters<NewWindowArgs>,
    ) -> Result<Json<WindowView>, ErrorData> {
        let session = self.find_session(&session).await?;
        let options = name.map_or_else(
            libtmux::NewWindowOptions::unnamed,
            libtmux::NewWindowOptions::new,
        );
        let window = session
            .new_window(options)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(Self::one_window(&window)))
    }

    /// Refuse a plan holding an operation this server's tier does not offer.
    ///
    /// Named in the error, because "refused" without saying which step is a
    /// message an agent cannot act on.
    fn admit_plan(&self, plan: &Plan) -> Result<(), ErrorData> {
        for (index, op) in plan.steps().iter().enumerate() {
            if !self.safety.admits_operation(op.safety()) {
                return Err(ErrorData::invalid_params(
                    format!(
                        "step {index} is {}, which this server does not offer at the {} \
                         safety tier: {}",
                        match op.safety() {
                            PlanSafety::ReadOnly => "read-only",
                            PlanSafety::Mutating => "mutating",
                            PlanSafety::Destructive => "destructive",
                        },
                        self.safety.name(),
                        op.name(),
                    ),
                    None,
                ));
            }
        }
        Ok(())
    }

    /// Run several tmux operations described as one plan.
    ///
    /// A plan is data, so an agent describes the whole build in one call
    /// rather than one call per step, and every object a later step addresses
    /// is a reference to the step that makes it -- no ids to look up in
    /// between.
    #[tool(
        description = "Run several tmux operations described as one plan, instead of one \
                       call per step. Objects a later step uses are references to the step \
                       that creates them, so no ids are looked up in between. Every \
                       operation is checked against this server's safety tier before \
                       anything runs.",
        title = "Run Plan",
        annotations(
            read_only_hint = false,
            // The tool destroys nothing; a plan handed to it might, and that
            // is checked per operation before anything runs. Annotating the
            // tool destructive instead would withhold it from the mutating
            // tier entirely, which would refuse harmless plans and make the
            // per-operation check pointless.
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn run_plan(
        &self,
        Parameters(RunPlanArgs { plan, grouping }): Parameters<RunPlanArgs>,
    ) -> Result<Json<PlanRun>, ErrorData> {
        let plan: Plan = serde_json::from_value(plan).map_err(|error| {
            ErrorData::invalid_params(format!("the plan could not be read: {error}"), None)
        })?;
        let planner = match grouping.as_deref() {
            None | Some("sequential") => Planner::Sequential,
            Some("folding") => Planner::Folding,
            Some("marked") => Planner::Marked,
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("unknown grouping {other:?}; use sequential, folding, or marked"),
                    None,
                ));
            }
        };

        // A tool annotation describes the tool, and a plan is a bag of
        // operations, so the tier is enforced per operation instead. Checked
        // before anything runs: refusing halfway would leave the tmux server
        // in a state the caller did not ask for and cannot see.
        self.admit_plan(&plan)?;

        let result = plan
            .run(&self.server, planner)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(PlanRun {
            outcomes: plan
                .steps()
                .iter()
                .zip(result.outcomes())
                .map(|(op, outcome)| PlanStepOutcome {
                    command: op.name().to_owned(),
                    outcome: match outcome {
                        PlanOutcome::Complete => "complete",
                        PlanOutcome::Failed => "failed",
                        PlanOutcome::Skipped => "skipped",
                        PlanOutcome::Unknown => "unknown",
                    }
                    .to_owned(),
                })
                .collect(),
            dispatches: result.dispatches(),
            complete: result.is_complete(),
        }))
    }

    /// Kill one window.
    #[tool(
        description = "Kill a window, closing it in every session that links it",
        title = "Kill Window",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn kill_window(
        &self,
        Parameters(WindowArgs { window }): Parameters<WindowArgs>,
    ) -> Result<Json<Killed>, ErrorData> {
        let window = self.find_window(&window).await?;
        let id = window.id().to_string();
        if let Some(own) = self.own_pane().await {
            let panes = window.panes().await.map_err(|e| tmux_error(&e))?;
            if panes.iter().any(|pane| pane.id().to_string() == own) {
                return Err(Self::self_harm("window", own));
            }
        }
        window.kill().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Killed { id }))
    }

    /// Kill one pane.
    #[tool(
        description = "Kill a pane. Killing a window's last pane closes the window",
        title = "Kill Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn kill_pane(
        &self,
        Parameters(PaneArgs { pane }): Parameters<PaneArgs>,
    ) -> Result<Json<Killed>, ErrorData> {
        let pane = self.find_pane(&pane).await?;
        let id = pane.id().to_string();
        if self.own_pane().await == Some(id.as_str()) {
            return Err(Self::self_harm("pane", &id));
        }
        pane.kill().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Killed { id }))
    }

    /// Rename a session or a window.
    #[tool(
        description = "Rename a session or a window. The target is a $-prefixed session id \
                       or an @-prefixed window id.",
        title = "Rename Session Or Window",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn rename(
        &self,
        Parameters(RenameArgs { target, name }): Parameters<RenameArgs>,
    ) -> Result<Json<Renamed>, ErrorData> {
        if target.starts_with('@') {
            let mut window = self.find_window(&target).await?;
            window
                .rename(name.clone())
                .await
                .map_err(|e| tmux_error(&e))?;

            return Ok(Json(Renamed {
                id: window.id().to_string(),
                name,
            }));
        }

        let mut session = self
            .server
            .sessions()
            .await
            .map_err(|e| tmux_error(&e))?
            .into_iter()
            .find(|session| session.id().to_string() == target)
            .ok_or_else(|| object_gone("session", &target))?;
        session
            .rename(name.clone())
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(Renamed {
            id: session.id().to_string(),
            name,
        }))
    }

    /// Find panes matching a portable filter expression.
    #[tool(
        description = "Find panes with a libtmux filter expression. The envelope is \
                       {\"version\": 1, \"target\": \"pane\", \"expr\": {...}} and field \
                       names are tmux format names such as pane_current_command. \
                       An expression names a pane's own fields only; pass session or \
                       window to narrow which panes it is applied to. \
                       Layout is queryable here rather than through a tool: \
                       pane_at_top, pane_at_bottom, pane_at_left and pane_at_right are \
                       booleans, and pane_left, pane_right, pane_top, pane_bottom, pane_x \
                       and pane_y are coordinates, so the bottom-right pane is one \
                       expression with two conjuncts. \
                       Prefer this over listing everything and filtering yourself.",
        title = "Find Panes By Expression",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn find_panes(
        &self,
        Parameters(FilterArgs {
            filter,
            session,
            window,
        }): Parameters<FilterArgs>,
    ) -> Result<Json<Panes>, ErrorData> {
        // Decoding rejects unknown versions, fields, and operators, so an
        // expression that survives this is one the crate can evaluate.
        let expression: FilterExpr<libtmux::Pane> =
            serde_json::from_value(filter).map_err(|error| bad_input(error.to_string()))?;

        // Narrow with tmux's own scoping before matching, so a large server
        // does not have every pane listed to answer a question about one
        // window.
        let panes = match (session.as_deref(), window.as_deref()) {
            (_, Some(window)) => self.find_window(window).await?.panes().await,
            (Some(session), None) => self.find_session(session).await?.panes().await,
            (None, None) => self.server.panes().await,
        };
        let panes = panes.map_err(|e| tmux_error(&e))?;
        let views: Vec<_> = panes.iter().matching(&expression).cloned().collect();

        Ok(Json(self.render_panes(&views).await))
    }

    /// Read one pane's contents.
    #[tool(
        description = "Read a pane's contents. Reads the visible screen by default; set history \
                       to reach output that has scrolled off, or give a start and end line. \
                       Set last_command to get only what the last command printed, which is \
                       usually what you want and is far shorter -- it needs tmux 3.7 and a \
                       shell that marks its prompts, and says so when it cannot.",
        title = "Read Pane Contents",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn capture_pane(
        &self,
        Parameters(CapturePaneArgs {
            pane,
            history,
            last_command,
            start,
            end,
        }): Parameters<CapturePaneArgs>,
    ) -> Result<Json<Capture>, ErrorData> {
        if last_command {
            return self.capture_last_command(&pane).await;
        }

        let mut options = if history {
            CaptureOptions::history()
        } else {
            CaptureOptions::visible()
        };
        if let Some(start) = start {
            options = options.start(start);
        }
        if let Some(end) = end {
            options = options.end(end);
        }

        let pane = self.find_pane(&pane).await?;
        let lines = pane
            .capture_with(options)
            .await
            .map_err(|e| tmux_error(&e))?;

        let rendered: Vec<String> = lines
            .iter()
            .map(|line| line.to_string_lossy().into_owned())
            .collect();

        Ok(Json(Capture {
            pane: pane.id().to_string(),
            lines: rendered.len(),
            text: rendered.join("\n"),
            marks: Marks::NotAsked,
        }))
    }

    /// Read what the last command in a pane printed.
    ///
    /// tmux records where a prompt and its output begin from the OSC 133
    /// sequences a shell emits. Where those marks exist this is exact; where
    /// they do not, the whole screen comes back with `marks` saying why, so a
    /// caller reads a field rather than guessing from a suspiciously long
    /// answer.
    async fn capture_last_command(&self, pane: &str) -> Result<Json<Capture>, ErrorData> {
        let target = self.find_pane(pane).await?;
        let supported = self.server.capabilities().await.is_ok_and(|capabilities| {
            capabilities
                .tmux_version()
                .meets(&libtmux::since::CAPTURE_LINE_FLAGS)
        });

        let (rendered, marks) = if supported {
            let lines = target
                .capture_lines(CaptureOptions::history())
                .await
                .map_err(|e| tmux_error(&e))?;

            // The last run begins at the last line marked as output, and ends
            // where the next prompt begins -- which for the last command is
            // the end of what tmux holds.
            match lines.iter().rposition(|line| line.starts_output) {
                Some(from) => {
                    // Searched past the output's own line: a shell that emits
                    // both marks before printing anything puts them on one
                    // line, and that prompt cannot delimit its own output.
                    let to = lines[from + 1..]
                        .iter()
                        .position(|line| line.starts_prompt)
                        .map_or(lines.len(), |offset| from + 1 + offset);
                    (
                        lines[from..to]
                            .iter()
                            .map(|line| line.text.to_string_lossy().into_owned())
                            .collect::<Vec<_>>(),
                        Marks::Present,
                    )
                }
                None => (
                    lines
                        .iter()
                        .map(|line| line.text.to_string_lossy().into_owned())
                        .collect(),
                    Marks::Absent,
                ),
            }
        } else {
            let lines = target
                .capture_with(CaptureOptions::visible())
                .await
                .map_err(|e| tmux_error(&e))?;
            (
                lines
                    .iter()
                    .map(|line| line.to_string_lossy().into_owned())
                    .collect(),
                Marks::Unsupported,
            )
        };

        Ok(Json(Capture {
            pane: target.id().to_string(),
            lines: rendered.len(),
            text: rendered.join("\n"),
            marks,
        }))
    }

    /// Watch a pane produce output, without polling.
    #[tool(
        description = "Watch a pane and report everything it writes for a bounded time. \
                       Unlike capture_pane this misses nothing, including output that \
                       scrolls past, but it blocks for the requested duration",
        title = "Watch Pane Bytes",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn watch_pane(
        &self,
        Parameters(WatchPaneArgs {
            pane,
            seconds,
            max_bytes,
        }): Parameters<WatchPaneArgs>,
    ) -> Result<Json<Watch>, ErrorData> {
        // An agent that asks for an hour gets a minute: this call holds a
        // connection open and blocks its own response until it returns.
        let window = Duration::from_secs(seconds.clamp(1, 60));
        let budget = max_bytes.unwrap_or(WATCH_BYTES).clamp(1, WATCH_BYTES);

        let pane = self.find_pane(&pane).await?;
        let mut output = pane.stream_output().await.map_err(|e| tmux_error(&e))?;

        let mut collected = Vec::new();
        let mut stopped = "deadline";
        let deadline = tokio::time::Instant::now() + window;

        while collected.len() < budget {
            match tokio::time::timeout_at(deadline, output.next_chunk()).await {
                Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
                // The pane stopped writing for good, which is worth saying:
                // it is the difference between a busy pane and a dead one.
                Ok(None) => {
                    stopped = "pane closed";
                    break;
                }
                Err(_) => break,
            }
        }
        if collected.len() >= budget {
            collected.truncate(budget);
            stopped = "byte limit";
        }

        let view = Watch {
            pane: output.pane().to_string(),
            bytes: collected.len(),
            // A pane emits whatever bytes it likes, and JSON carries text.
            output: String::from_utf8_lossy(&collected).into_owned(),
            stopped: stopped.to_owned(),
        };

        output.shutdown().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Find sessions by what they contain.
    #[tool(
        description = "Find sessions with a libtmux filter expression that can ask about \
                       their windows and panes. The envelope is {\"version\": 1, \
                       \"target\": \"session_tree\", \"expr\": {...}}, where windows is a \
                       relation taking any, all, or none, and a window's panes is another. \
                       Use this for questions find_panes cannot ask, such as which sessions \
                       hold a window named build",
        title = "Find Sessions By Expression",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn find_sessions(
        &self,
        Parameters(TreeFilterArgs { filter }): Parameters<TreeFilterArgs>,
    ) -> Result<Json<Sessions>, ErrorData> {
        let expression: FilterExpr<libtmux::SessionTree> =
            serde_json::from_value(filter).map_err(|error| bad_input(error.to_string()))?;

        // One gathering of the hierarchy, three tmux commands, and the
        // expression decides among the branches locally.
        let branches = self.server.hierarchy().await.map_err(|e| tmux_error(&e))?;
        let sessions: Vec<_> = branches
            .iter()
            .matching(&expression)
            .map(|branch| SessionView {
                id: branch.session.id().to_string(),
                name: lossy(branch.session.name()),
                windows: branch.session.window_count(),
                attached: branch.session.is_attached(),
            })
            .collect();

        Ok(Json(Sessions { sessions }))
    }

    /// Create a detached session.
    #[tool(
        description = "Create a new detached tmux session",
        title = "Create Session",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn create_session(
        &self,
        Parameters(CreateSessionArgs {
            name,
            start_directory,
        }): Parameters<CreateSessionArgs>,
    ) -> Result<Json<SessionView>, ErrorData> {
        let mut options = NewSessionOptions::new(name);
        if let Some(directory) = start_directory {
            options = options.start_directory(directory);
        }

        let session = self
            .server
            .new_session(options)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(SessionView {
            id: session.id().to_string(),
            name: lossy(session.name()),
            windows: session.window_count(),
            attached: session.is_attached(),
        }))
    }

    /// Kill a session and everything in it.
    #[tool(
        description = "Kill a tmux session and everything in it",
        title = "Kill Session",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn kill_session(
        &self,
        Parameters(SessionArgs { session }): Parameters<SessionArgs>,
    ) -> Result<Json<Killed>, ErrorData> {
        let target = self.find_session(&session).await?;
        let id = target.id().to_string();
        if let Some(own) = self.own_pane().await {
            let panes = target.panes().await.map_err(|e| tmux_error(&e))?;
            if panes.iter().any(|pane| pane.id().to_string() == own) {
                return Err(Self::self_harm("session", own));
            }
        }
        target.kill().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Killed { id }))
    }

    /// Split the window holding a pane, creating another pane.
    #[tool(
        description = "Split a pane's window, creating a new pane beside it",
        title = "Split Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn split_pane(
        &self,
        Parameters(SplitPaneArgs {
            pane,
            direction,
            percent,
            command,
        }): Parameters<SplitPaneArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let direction = match direction.as_deref() {
            None | Some("below") => SplitDirection::Below,
            Some("above") => SplitDirection::Above,
            Some("left") => SplitDirection::Left,
            Some("right") => SplitDirection::Right,
            Some(other) => {
                return Err(bad_input(format!(
                    "direction must be above, below, left, or right, not {other}"
                )));
            }
        };

        let mut options = SplitOptions::new(direction);
        if let Some(percent) = percent {
            // Checked here rather than left to tmux, which does not agree with
            // itself: 3.7b refuses a percentage above 100 and 3.2a accepts it.
            // An agent should get the same answer from the same call whatever
            // tmux is underneath.
            if !(1..=100).contains(&percent) {
                return Err(bad_input(format!(
                    "percent must be between 1 and 100, not {percent}"
                )));
            }
            options = options.size(PaneSize::Percent(percent));
        }
        if let Some(command) = command {
            options = options.command(command);
        }

        // Divide the pane that was named, not whichever one is active.
        let created = self
            .find_pane(&pane)
            .await?
            .split(options)
            .await
            .map_err(|e| tmux_error(&e))?;

        let socket = self.socket().await;
        Ok(Json(self.pane_view(&created, socket)))
    }

    /// Move one edge of a pane.
    #[tool(
        description = "Move one edge of a pane by a number of rows or columns",
        title = "Resize Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn resize_pane(
        &self,
        Parameters(ResizePaneArgs {
            pane,
            direction,
            cells,
        }): Parameters<ResizePaneArgs>,
    ) -> Result<Json<Size>, ErrorData> {
        let direction = match direction.as_str() {
            "up" => ResizeDirection::Up,
            "down" => ResizeDirection::Down,
            "left" => ResizeDirection::Left,
            "right" => ResizeDirection::Right,
            other => {
                return Err(bad_input(format!(
                    "direction must be up, down, left, or right, not {other}"
                )));
            }
        };

        let mut pane = self.find_pane(&pane).await?;
        pane.resize_by(direction, cells)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(Size {
            pane: pane.id().to_string(),
            width: pane.width(),
            height: pane.height(),
        }))
    }

    /// Type text into a pane, or press keys in it.
    #[tool(
        description = "Type text into a pane, press named keys in it, or both. `text` is sent \
                       literally, so C-c in it types those three characters. Use `keys` for \
                       anything without a character of its own -- C-c to interrupt a running \
                       command, Escape, Up, C-d -- which are tmux key names and are \
                       interpreted. Text is sent first, then keys, then Enter if asked.",
        title = "Send Keys To Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn send_keys(
        &self,
        Parameters(SendKeysArgs {
            pane,
            text,
            keys,
            enter,
        }): Parameters<SendKeysArgs>,
    ) -> Result<Json<Sent>, ErrorData> {
        let keys = keys.unwrap_or_default();
        if text.is_none() && keys.is_empty() && !enter {
            return Err(bad_input("send_keys needs text, keys, or enter".to_owned()));
        }

        let target = self.find_pane(&pane).await?;
        if let Some(text) = text {
            target.send_keys(text).await.map_err(|e| tmux_error(&e))?;
        }
        if !keys.is_empty() {
            target
                .send_key_names(keys)
                .await
                .map_err(|e| tmux_error(&e))?;
        }
        if enter {
            target
                .send_key_names(["Enter"])
                .await
                .map_err(|e| tmux_error(&e))?;
        }

        Ok(Json(Sent {
            pane: target.id().to_string(),
        }))
    }

    /// Render one window as the protocol sees it.
    fn one_window(window: &libtmux::Window) -> WindowView {
        WindowView {
            id: window.id().to_string(),
            session_id: window.session_id().to_string(),
            index: window.index(),
            name: lossy(window.name()),
            panes: window.pane_count(),
            active: window.is_active(),
            linked: window.is_linked(),
        }
    }

    /// Render windows as the protocol sees them.
    fn render_windows(windows: &[libtmux::Window]) -> Windows {
        let windows: Vec<_> = windows.iter().map(Self::one_window).collect();

        Windows { windows }
    }

    /// Move focus to a pane, or to the one beside it.
    #[tool(
        description = "Select a pane, making it its window's active pane. Give a direction to \
                       move relative to it instead: up, down, left, and right follow the \
                       layout, last returns to the previously active pane, and next and \
                       previous step through the window in order.",
        title = "Select Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn select_pane(
        &self,
        Parameters(SelectPaneArgs { pane, direction }): Parameters<SelectPaneArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let target = self.find_pane(&pane).await?;

        // `next` and `previous` are resolved here rather than with a tmux
        // target, because tmux's `{next}` is relative to the active pane and
        // this tool is relative to the pane the caller named.
        let selected = match direction.as_deref() {
            Some("next" | "previous") => {
                let panes = self
                    .find_window(target.window_id().as_ref())
                    .await?
                    .panes()
                    .await
                    .map_err(|e| tmux_error(&e))?;
                let at = panes
                    .iter()
                    .position(|candidate| candidate.id() == target.id())
                    .ok_or_else(|| vanished("the pane vanished from its own window"))?;
                let step = if direction.as_deref() == Some("next") {
                    at + 1
                } else {
                    at + panes.len() - 1
                };
                let mut chosen = panes
                    .get(step % panes.len())
                    .cloned()
                    .ok_or_else(|| vanished("the window has no panes"))?;
                chosen.select().await.map_err(|e| tmux_error(&e))?;
                chosen
            }
            other => {
                let flag = match other {
                    None => None,
                    Some("up") => Some("-U"),
                    Some("down") => Some("-D"),
                    Some("left") => Some("-L"),
                    Some("right") => Some("-R"),
                    Some("last") => Some("-l"),
                    Some(unknown) => {
                        return Err(bad_input(format!(
                            "direction must be up, down, left, right, last, next, or \
                                 previous, not {unknown}"
                        )));
                    }
                };
                let mut command = Command::new("select-pane")
                    .arg("-t")
                    .arg(target.id().to_string());
                if let Some(flag) = flag {
                    command = command.arg(flag);
                }
                self.server.cmd(command).await.map_err(|e| tmux_error(&e))?;

                // Which pane that landed on is tmux's answer, not ours.
                self.find_window(target.window_id().as_ref())
                    .await?
                    .active_pane()
                    .await
                    .map_err(|e| tmux_error(&e))?
                    .ok_or_else(|| vanished("the window reported no active pane"))?
            }
        };

        let socket = self.socket().await;
        Ok(Json(self.pane_view(&selected, socket)))
    }

    /// Move focus to a window, or to the one beside it.
    #[tool(
        description = "Select a window, making it its session's active window. Give a \
                       direction to move relative to it instead: next and previous step \
                       through the session in index order, and last returns to the \
                       previously active window.",
        title = "Select Window",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn select_window(
        &self,
        Parameters(SelectWindowArgs { window, direction }): Parameters<SelectWindowArgs>,
    ) -> Result<Json<Windows>, ErrorData> {
        let mut target = self.find_window(&window).await?;

        match direction.as_deref() {
            None => {
                target.select().await.map_err(|e| tmux_error(&e))?;
            }
            Some(step) => {
                let flag = match step {
                    "next" => "-n",
                    "previous" => "-p",
                    "last" => "-l",
                    unknown => {
                        return Err(bad_input(format!(
                            "direction must be next, previous, or last, not {unknown}"
                        )));
                    }
                };
                // tmux resolves all three against the session, not against
                // `-t`: `cmd-select-window.c` calls `session_next`,
                // `session_previous` or `session_last` on the target's session
                // and never looks at the window. Selecting the named window
                // first is what makes a step relative to it.
                //
                // `last` is excluded from that. It means the session's
                // previously active window, so selecting the named one first
                // would rewrite the very pointer being asked about.
                if step != "last" {
                    target.select().await.map_err(|e| tmux_error(&e))?;
                }
                self.server
                    .cmd(
                        Command::new("select-window")
                            .arg(flag)
                            .arg("-t")
                            .arg(target.session_id().to_string()),
                    )
                    .await
                    .map_err(|e| tmux_error(&e))?;
            }
        }

        // Which window that landed on is tmux's answer, not ours.
        let session = self
            .server
            .session_by_id(target.session_id())
            .await
            .map_err(|e| tmux_error(&e))?
            .ok_or_else(|| vanished("the window's session is gone"))?;
        let active = session
            .active_window()
            .await
            .map_err(|e| tmux_error(&e))?
            .ok_or_else(|| vanished("the session reported no active window"))?;

        Ok(Json(Self::render_windows(&[active])))
    }

    /// Report everything about one pane in a single answer.
    #[tool(
        description = "Read a pane's whole state at once: what it is showing, plus the \
                       cursor position, whether it is in copy mode, and how far it is \
                       scrolled. Prefer this over capture_pane when you need to reason \
                       about where the pane is rather than only what it says -- a cursor \
                       at column zero on a fresh line is a shell waiting, and a pane in \
                       copy mode will not accept keys.",
        title = "Snapshot Pane State",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        ),
        meta = always_load()
    )]
    pub async fn snapshot_pane(
        &self,
        Parameters(SnapshotArgs {
            pane,
            max_lines,
            history,
        }): Parameters<SnapshotArgs>,
    ) -> Result<Json<Snapshot>, ErrorData> {
        let target = self.find_pane(&pane).await?;

        // One format query for the state a listing does not carry.
        let reading = self
            .server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(target.id().to_string())
                    .arg(format!(
                        "#{{cursor_x}}{SEPARATOR}#{{cursor_y}}{SEPARATOR}\
                         #{{pane_mode}}{SEPARATOR}#{{scroll_position}}"
                    )),
            )
            .await
            .map_err(|e| tmux_error(&e))?;
        let reading = reading.stdout_lossy();
        let mut fields = reading.trim_end_matches('\n').split(SEPARATOR);
        let cursor_x = fields.next().and_then(|field| field.parse::<u32>().ok());
        let cursor_y = fields.next().and_then(|field| field.parse::<u32>().ok());
        // tmux reports these empty for a pane that is not in a mode, which is
        // the ordinary case and not a failure to read them.
        let mode = fields.next().filter(|field| !field.is_empty());
        let scroll = fields.next().and_then(|field| field.parse::<i32>().ok());

        let options = if history {
            CaptureOptions::history()
        } else {
            CaptureOptions::visible()
        };
        let lines = target
            .capture_with(options)
            .await
            .map_err(|e| tmux_error(&e))?;
        let kept = max_lines.unwrap_or(lines.len()).min(lines.len());
        let dropped = lines.len() - kept;
        let content = lines
            .iter()
            .skip(dropped)
            .map(|line| line.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");

        let socket = self.socket().await;
        Ok(Json(Snapshot {
            pane: self.pane_view(&target, socket),
            width: target.width(),
            height: target.height(),
            cursor_x,
            cursor_y,
            in_mode: target.is_in_mode(),
            mode: mode.map(ToOwned::to_owned),
            scroll_position: scroll,
            dead: target.is_dead(),
            content,
            lines: kept,
            // Saying what was dropped is the difference between a short pane
            // and a long one the caller asked to see the end of.
            dropped,
        }))
    }

    /// Find which panes are showing something.
    #[tool(
        description = "Search what panes are displaying, and report the pane and line of \
                       every match. Use this to find where something is -- which pane has \
                       the failing test, which one printed the error -- instead of capturing \
                       panes one at a time. Searches the visible screen by default; set \
                       history to include scrollback.",
        title = "Search Pane Contents",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn search_panes(
        &self,
        Parameters(SearchPanesArgs {
            pattern,
            regex,
            match_case,
            history,
            session,
            window,
        }): Parameters<SearchPanesArgs>,
    ) -> Result<Json<Matches>, ErrorData> {
        let patterns = Patterns::compile(std::slice::from_ref(&pattern), regex, match_case)
            .map_err(|(source, reason)| {
                bad_input(format!("pattern {source} is invalid: {reason}"))
            })?;

        // Narrowed with tmux's own scoping first, so searching one window does
        // not read every pane on the server.
        let panes = match (session.as_deref(), window.as_deref()) {
            (_, Some(window)) => self.find_window(window).await?.panes().await,
            (Some(session), None) => self.find_session(session).await?.panes().await,
            (None, None) => self.server.panes().await,
        };
        let panes = panes.map_err(|e| tmux_error(&e))?;

        let options = if history {
            CaptureOptions::history()
        } else {
            CaptureOptions::visible()
        };
        let mut found: Vec<MatchView> = Vec::new();
        for pane in &panes {
            // A pane that cannot be read is not a reason to abandon the
            // search: it is usually one that closed while this ran.
            let Ok(lines) = pane.capture_with(options).await else {
                continue;
            };
            if found.len() >= SEARCH_MATCHES {
                // Reading the remaining panes could not change the answer, and
                // each one costs a capture.
                break;
            }
            for (number, line) in lines.iter().enumerate() {
                if found.len() >= SEARCH_MATCHES {
                    break;
                }
                if patterns.first_match(line.as_bytes()).is_some() {
                    found.push(MatchView {
                        pane: pane.id().to_string(),
                        window_id: pane.window_id().to_string(),
                        line: number,
                        text: line.to_string_lossy().into_owned(),
                    });
                }
            }
        }

        Ok(Json(Matches {
            // Saying the ceiling was reached is the difference between "that
            // is all of them" and "that is all you are getting".
            capped: found.len() >= SEARCH_MATCHES,
            matches: found,
            panes_searched: panes.len(),
        }))
    }

    /// Read a tmux option.
    #[tool(
        description = "Read a tmux option, such as history-limit or a user option like \
                       @theme. Name the scope the option lives in; global-session is what \
                       tmux uses when a command names no target.",
        title = "Read tmux Option",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn show_option(
        &self,
        Parameters(OptionArgs {
            name,
            scope,
            target,
            ..
        }): Parameters<OptionArgs>,
    ) -> Result<Json<OptionValue>, ErrorData> {
        let value = match self
            .option_scope(scope.as_deref(), target.as_deref())
            .await?
        {
            OptionScope::Server => self.server.get_option(&name).await,
            OptionScope::GlobalSession => self.server.get_global_option(&name).await,
            OptionScope::GlobalWindow => self.server.get_global_window_option(&name).await,
            OptionScope::Session(session) => session.get_option(&name).await,
            OptionScope::Window(window) => window.get_option(&name).await,
            OptionScope::Pane(pane) => pane.get_option(&name).await,
        }
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(OptionValue {
            name,
            // Absent and empty are different answers: tmux reports no value
            // for an option that has never been set at that scope.
            value: value.as_ref().map(lossy),
        }))
    }

    /// Write a tmux option.
    #[tool(
        description = "Set a tmux option at the scope you name. Setting an option changes \
                       how tmux behaves for everything in that scope, so prefer the \
                       narrowest one that works.",
        title = "Set tmux Option",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn set_option(
        &self,
        Parameters(OptionArgs {
            name,
            scope,
            target,
            value,
        }): Parameters<OptionArgs>,
    ) -> Result<Json<OptionSet>, ErrorData> {
        let value = value.ok_or_else(|| {
            bad_input("set_option needs a value; use show_option to read one".to_owned())
        })?;
        let value = value.to_string();

        match self
            .option_scope(scope.as_deref(), target.as_deref())
            .await?
        {
            OptionScope::Server => self.server.set_option(&name, &value).await,
            OptionScope::GlobalSession => self.server.set_global_option(&name, &value).await,
            OptionScope::GlobalWindow => self.server.set_global_window_option(&name, &value).await,
            OptionScope::Session(session) => session.set_option(&name, &value).await,
            OptionScope::Window(window) => window.set_option(&name, &value).await,
            OptionScope::Pane(pane) => pane.set_option(&name, &value).await,
        }
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(OptionSet {
            name,
            scope: scope.unwrap_or_else(|| "global-session".to_owned()),
        }))
    }

    /// Kill the whole server.
    #[tool(
        description = "Kill the tmux server, ending every session on it. This destroys all \
                       work in every pane and cannot be undone. Refused when this MCP server \
                       runs on that tmux server.",
        title = "Kill Server",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn kill_server(&self) -> Result<Json<ServerKilled>, ErrorData> {
        // Nothing on this server survives, so the caller's pane need not be
        // looked up: being here at all is disqualifying.
        if let Some(own) = self.own_pane().await {
            return Err(Self::self_harm("server", own));
        }
        // `Server::shutdown` closes this crate's own subprocess executor and
        // leaves the daemon running, which is the opposite of what this tool
        // promises.
        self.server
            .cmd(Command::new("kill-server"))
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(ServerKilled { killed: true }))
    }

    /// Resolve the object an option belongs to.
    async fn option_scope(
        &self,
        scope: Option<&str>,
        target: Option<&str>,
    ) -> Result<OptionScope, ErrorData> {
        let needs = |what: &str| bad_input(format!("scope {what} needs a target id"));

        match scope.unwrap_or("global-session") {
            "server" => Ok(OptionScope::Server),
            "global-session" => Ok(OptionScope::GlobalSession),
            "global-window" => Ok(OptionScope::GlobalWindow),
            "session" => {
                let target = target.ok_or_else(|| needs("session"))?;
                let session = self
                    .server
                    .sessions()
                    .await
                    .map_err(|e| tmux_error(&e))?
                    .into_iter()
                    .find(|session| {
                        session.id().to_string() == target || session.name() == target.as_bytes()
                    })
                    .ok_or_else(|| bad_input(format!("no session {target}")))?;
                Ok(OptionScope::Session(Box::new(session)))
            }
            "window" => Ok(OptionScope::Window(Box::new(
                self.find_window(target.ok_or_else(|| needs("window"))?)
                    .await?,
            ))),
            "pane" => Ok(OptionScope::Pane(Box::new(
                self.find_pane(target.ok_or_else(|| needs("pane"))?).await?,
            ))),
            unknown => Err(bad_input(format!(
                "scope must be server, global-session, global-window, session, window, \
                     or pane, not {unknown}"
            ))),
        }
    }

    /// Run a command in a pane and report how it went.
    #[tool(
        description = "Run a shell command in a pane, wait for it to finish, and report its \
                       exit status with everything it wrote. This is the tool for \"run this \
                       and tell me if it worked\". Output is read from the pane's live stream, \
                       so nothing is missed and the shell prompt is not included. The command \
                       runs in a subshell, so cd and export do not persist. \
                       Reaching the deadline stops the waiting, not the command: the pane is \
                       still busy afterwards, and another run there reports no_shell until it \
                       finishes. Send C-c with send_keys to stop it.",
        title = "Run Command In Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn run_command(
        &self,
        Parameters(RunCommandArgs {
            pane,
            command,
            seconds,
            suppress_history,
        }): Parameters<RunCommandArgs>,
        cancelled: tokio_util::sync::CancellationToken,
    ) -> Result<Json<RunView>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        // A pane in copy mode does not pass keys to the shell, so the command
        // would be read as navigation and the wait would run to its deadline
        // with nothing to show for it.
        if target.is_in_mode() {
            return Err(bad_input(format!(
                "pane {pane} is in copy mode, where keys move the cursor rather than \
                     reaching the shell. Leave it first."
            )));
        }
        let view = exec::run_command(
            &target,
            &command,
            Self::budget(seconds),
            suppress_history,
            &cancelled,
        )
        .await
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Start a command without waiting for it.
    #[tool(
        description = "Start a shell command in a pane and return at once with a job id, \
                       instead of holding this call until it finishes. Use this for anything \
                       slow -- a build, a test suite, a deploy -- and for running several at \
                       once: the answer is collected whether or not you are waiting for it. \
                       Poll with job_status, which returns only what is new. Prefer \
                       run_command when the command is quick and you want its answer now.",
        title = "Start Command In Background",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn start_command(
        &self,
        Parameters(StartCommandArgs {
            pane,
            command,
            suppress_history,
        }): Parameters<StartCommandArgs>,
    ) -> Result<Json<jobs::JobView>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        // A pane in copy mode does not pass keys to the shell, so the command
        // would be read as navigation and the job would never start.
        if target.is_in_mode() {
            return Err(bad_input(format!(
                "pane {pane} is in copy mode, where keys move the cursor rather than \
                     reaching the shell. Leave it first."
            )));
        }

        let view = self
            .jobs
            .start(&target, &command, suppress_history)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Report how a background command is getting on.
    #[tool(
        description = "Report whether a job started with start_command is still running, its \
                       exit status once it is not, and what it has written since the cursor \
                       you were given last. Pass that cursor back to read only what is new. \
                       Give seconds to wait for it to finish, which returns as soon as it \
                       does rather than at the deadline.",
        title = "Check Background Command",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn job_status(
        &self,
        Parameters(JobStatusArgs {
            job,
            cursor,
            seconds,
        }): Parameters<JobStatusArgs>,
    ) -> Result<Json<jobs::JobProgress>, ErrorData> {
        if let Some(seconds) = seconds.filter(|seconds| *seconds > 0) {
            self.jobs.wait(&job, Self::budget(Some(seconds))).await;
        }

        self.jobs
            .read(&job, cursor)
            .map(Json)
            .ok_or_else(|| unknown_job(&job))
    }

    /// List the background commands this server is holding.
    #[tool(
        description = "List every job started with start_command, running and finished, \
                       newest first. A finished job is kept so its answer can still be \
                       collected, and the oldest is forgotten once too many pile up.",
        title = "List Background Commands",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn list_jobs(&self) -> Result<Json<JobList>, ErrorData> {
        Ok(Json(JobList {
            jobs: self.jobs.list(),
        }))
    }

    /// Stop a background command and forget it.
    #[tool(
        description = "Interrupt a running job with C-c and forget it. A job that has already \
                       finished is forgotten without touching its pane. This sends the \
                       interrupt to the pane the job runs in, so anything else that pane is \
                       doing is interrupted too.",
        title = "Cancel Background Command",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn cancel_job(
        &self,
        Parameters(CancelJobArgs { job }): Parameters<CancelJobArgs>,
    ) -> Result<Json<JobCancelled>, ErrorData> {
        let (pane, running) = self
            .jobs
            .running_in(&job)
            .ok_or_else(|| unknown_job(&job))?;

        if running {
            let target = self.find_pane(&pane).await?;
            target
                .send_key_names(["C-c"])
                .await
                .map_err(|e| tmux_error(&e))?;
        }
        self.jobs.forget(&job);

        Ok(Json(JobCancelled {
            job,
            pane,
            interrupted: running,
        }))
    }

    /// Find the tmux servers running on this machine.
    #[tool(
        description = "Find every tmux server on this machine, by looking where tmux puts its \
                       sockets. These tools are bound to one server for their whole life, so \
                       this is the only way to learn that another exists; acting on one means \
                       starting a server pointed at its socket. A pane id means nothing \
                       across servers: %1 names a different pane on each.",
        title = "List tmux Servers",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn list_servers(&self) -> Result<Json<ServerListings>, ErrorData> {
        let bound = self.socket().await.map(Path::to_path_buf);
        let mut searched = Vec::new();
        let mut found: Vec<PathBuf> = Vec::new();

        // tmux puts its sockets in `$TMUX_TMPDIR/tmux-<uid>`, defaulting to
        // /tmp. The directory is per-user, so this never reaches another
        // user's servers even when /tmp is shared. The uid comes from tmux
        // rather than from this process, because it is tmux's own choice of
        // directory that is being predicted.
        let mut roots = Vec::new();
        if let Ok(uid) = self.server.format(None, "#{uid}").await {
            roots.push(
                std::env::var_os("TMUX_TMPDIR")
                    .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
                    .join(format!("tmux-{}", lossy(&uid).trim())),
            );
        }
        // A server reached through --socket sits wherever it was put, and its
        // neighbours are worth finding too.
        if let Some(parent) = bound.as_deref().and_then(Path::parent) {
            let parent = parent.to_path_buf();
            if !roots.contains(&parent) {
                roots.push(parent);
            }
        }

        for root in roots {
            searched.push(root.display().to_string());
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if std::fs::metadata(&path)
                    .is_ok_and(|meta| std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()))
                    && !found.contains(&path)
                {
                    found.push(path);
                }
            }
        }

        // The bound server may sit outside that directory, which is exactly
        // what --socket is for, so it is added rather than searched for.
        if let Some(bound) = bound.as_ref()
            && !found.contains(bound)
        {
            found.push(bound.clone());
        }
        found.sort();

        let mut servers = Vec::with_capacity(found.len());
        for socket in found {
            let current = bound.as_ref() == Some(&socket);
            let (sessions, unreachable) = match Server::builder()
                .socket_path(&socket)
                .build()
                .map_err(|error| error.to_string())
            {
                Ok(server) => match server.sessions().await {
                    Ok(sessions) => (u32::try_from(sessions.len()).ok(), None),
                    Err(error) => (None, Some(error.to_string())),
                },
                Err(error) => (None, Some(error)),
            };

            servers.push(ServerListing {
                name: socket
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
                socket: socket.display().to_string(),
                sessions,
                current,
                unreachable,
            });
        }
        servers.sort_by_key(|listing| !listing.current);

        Ok(Json(ServerListings { servers, searched }))
    }

    /// Expand a tmux format string.
    #[tool(
        description = "Expand a tmux format such as #{pane_unseen_changes} or \
                       #{window_activity_flag} and return what it evaluates to. This reaches \
                       every field tmux publishes, including ones no tool here has of its \
                       own, so use it for questions the listings cannot answer. Name a pane \
                       for anything pane, window or session shaped: tmux resolves the window \
                       and session from it.",
        title = "Expand A tmux Format",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn expand_format(
        &self,
        Parameters(FormatArgs { format, pane }): Parameters<FormatArgs>,
    ) -> Result<Json<Formatted>, ErrorData> {
        let target = match pane.as_deref() {
            Some(pane) => Some(self.find_pane(pane).await?),
            None => None,
        };

        let value = self
            .server
            .format(target.as_ref(), &format)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(Formatted {
            format,
            value: lossy(&value),
            pane,
        }))
    }

    /// Read a tmux environment.
    #[tool(
        description = "Read the environment tmux hands to processes it starts, for the server \
                       or for one session. This is not the environment of anything already \
                       running: a pane started before a change keeps what it was given.",
        title = "Show tmux Environment",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn show_environment(
        &self,
        Parameters(ShowEnvironmentArgs { session }): Parameters<ShowEnvironmentArgs>,
    ) -> Result<Json<Environment>, ErrorData> {
        let entries = match session.as_deref() {
            Some(name) => self.find_session(name).await?.environment_all().await,
            None => self.server.environment_all().await,
        }
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(Environment {
            entries: entries
                .into_iter()
                .map(|(name, entry)| EnvironmentEntry {
                    name,
                    value: match entry {
                        libtmux::EnvironmentEntry::Set(value) => Some(lossy(&value)),
                        libtmux::EnvironmentEntry::Removed => None,
                    },
                })
                .collect(),
            session,
        }))
    }

    /// Write a tmux environment variable.
    #[tool(
        description = "Set or remove a variable in the environment tmux hands to processes it \
                       starts. Panes created afterwards see it; panes already running do not.",
        title = "Set tmux Environment Variable",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn set_environment(
        &self,
        Parameters(SetEnvironmentArgs {
            name,
            value,
            session,
        }): Parameters<SetEnvironmentArgs>,
    ) -> Result<Json<EnvironmentSet>, ErrorData> {
        let removed = value.is_none();
        match (session.as_deref(), value) {
            (Some(target), Some(value)) => {
                self.find_session(target)
                    .await?
                    .set_environment(&name, value)
                    .await
            }
            (Some(target), None) => {
                self.find_session(target)
                    .await?
                    .unset_environment(&name)
                    .await
            }
            (None, Some(value)) => self.server.set_environment(&name, value).await,
            (None, None) => self.server.unset_environment(&name).await,
        }
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(EnvironmentSet {
            name,
            session,
            removed,
        }))
    }

    /// Read the hooks tmux runs on its own events.
    #[tool(
        description = "List the hooks tmux runs when something happens on the server, such as \
                       a pane exiting. Read-only: a hook set from here would vanish with this \
                       process, so hooks belong in a tmux config file. Reach for this when \
                       tmux does something no tool here asked for.",
        title = "Show tmux Hooks",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn show_hooks(
        &self,
        Parameters(ShowHooksArgs { session }): Parameters<ShowHooksArgs>,
    ) -> Result<Json<Hooks>, ErrorData> {
        let found = match session.as_deref() {
            Some(name) => self.find_session(name).await?.hooks().await,
            None => self.server.hooks().await,
        }
        .map_err(|e| tmux_error(&e))?;

        let mut hooks = Vec::new();
        for (name, indexed) in found {
            for (index, command) in &indexed {
                hooks.push(Hook {
                    name: name.clone(),
                    // tmux numbers an array hook and leaves a single one bare.
                    index: (indexed.len() > 1).then_some(*index),
                    command: lossy(command),
                });
            }
        }

        Ok(Json(Hooks { hooks }))
    }

    /// Send a pane's output to a command as it arrives.
    #[tool(
        description = "Feed everything a pane writes to a shell command, such as tee to a \
                       file, until told to stop. tmux runs the command itself, so the pipe \
                       outlives this server: one left on keeps writing after the agent has \
                       gone. Prefer capture_since for reading a pane yourself; this is for \
                       handing the stream to something else.",
        title = "Pipe A Pane Elsewhere",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn pipe_pane(
        &self,
        Parameters(PipePaneArgs { pane, command }): Parameters<PipePaneArgs>,
    ) -> Result<Json<Piped>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        let piping = command.is_some();
        target.pipe(command).await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Piped {
            pane: target.id().to_string(),
            piping,
        }))
    }

    /// Arrange a window's panes.
    #[tool(
        description = "Rearrange a window's panes into a named layout, or into a layout \
                       string tmux gave you earlier. Use even-horizontal, even-vertical, \
                       main-horizontal, main-vertical or tiled.",
        title = "Arrange Window Panes",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn select_layout(
        &self,
        Parameters(SelectLayoutArgs { window, layout }): Parameters<SelectLayoutArgs>,
    ) -> Result<Json<Layout>, ErrorData> {
        let target = self.find_window(&window).await?;
        self.server
            .cmd(
                Command::new("select-layout")
                    .arg("-t")
                    .arg(target.id().to_string())
                    .arg(layout.clone()),
            )
            .await
            .map_err(|e| tmux_error(&e))
            .and_then(|result| {
                result.success().then_some(()).ok_or_else(|| {
                    bad_input(format!(
                        "tmux refused the layout {layout:?}: {}",
                        result.stderr_lossy().trim()
                    ))
                })
            })?;

        Ok(Json(Layout {
            window: target.id().to_string(),
            layout,
        }))
    }

    /// Empty a pane's scrollback.
    #[tool(
        description = "Discard a pane's scrollback, so the next capture_pane returns only \
                       what happens next. Use this before running something whose output you \
                       want to read cleanly: it is far cheaper than reading past the old \
                       output every time. The visible screen is left alone.",
        title = "Clear Pane History",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn clear_pane(
        &self,
        Parameters(PaneArgs { pane }): Parameters<PaneArgs>,
    ) -> Result<Json<PaneChanged>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        target.clear_history().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(PaneChanged {
            pane: target.id().to_string(),
        }))
    }

    /// Restart what a pane runs.
    #[tool(
        description = "Run a command in an existing pane again, keeping the pane and its \
                       place in the layout. This is how a dead pane is brought back without \
                       killing and re-splitting. A pane whose process is still alive is left \
                       alone unless kill_first is set.",
        title = "Restart A Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn respawn_pane(
        &self,
        Parameters(RespawnPaneArgs {
            pane,
            command,
            kill_first,
        }): Parameters<RespawnPaneArgs>,
    ) -> Result<Json<PaneChanged>, ErrorData> {
        let mut target = self.find_pane(&pane).await?;
        target
            .respawn(command, kill_first)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(PaneChanged {
            pane: target.id().to_string(),
        }))
    }

    /// Deliver text to a pane without typing it.
    #[tool(
        description = "Put text into a pane through a tmux paste buffer instead of typing it \
                       key by key. Use this for anything long or awkward: send_keys types the \
                       text, so a shell reading it can react to each character, and a \
                       bracketed-paste aware program treats a paste as one block. The buffer \
                       is deleted afterwards.",
        title = "Paste Text Into Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn paste_text(
        &self,
        Parameters(PasteTextArgs { pane, text }): Parameters<PasteTextArgs>,
    ) -> Result<Json<Pasted>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        let bytes = text.len();
        // Named after this process so two servers cannot collide, and deleted
        // below so a buffer this created does not outlive the paste.
        let buffer = format!("tmux-mcp-{}", std::process::id());

        self.server
            .set_buffer(Some(&buffer), std::ffi::OsString::from(text))
            .await
            .map_err(|e| tmux_error(&e))?;
        let pasted = target.paste_buffer(Some(&buffer)).await;
        let _ = self.server.delete_buffer(&buffer).await;
        pasted.map_err(|e| tmux_error(&e))?;

        Ok(Json(Pasted {
            pane: target.id().to_string(),
            bytes,
        }))
    }

    /// Wait until a pane stops writing.
    #[tool(
        description = "Wait until a pane has written nothing for a few seconds. Use this when \
                       you cannot name what success looks like: a TUI settling, an installer \
                       finishing, a prompt whose glyph you cannot predict. Prefer run_command \
                       for a command you sent yourself, and wait_for_text when you know the \
                       text to look for -- both are exact, and this one infers.",
        title = "Wait For Pane To Go Quiet",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn wait_for_idle(
        &self,
        Parameters(WaitForIdleArgs {
            pane,
            quiet_seconds,
            seconds,
        }): Parameters<WaitForIdleArgs>,
        cancelled: tokio_util::sync::CancellationToken,
    ) -> Result<Json<IdleView>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        // Clamped against the total, because quiet longer than the deadline
        // could never be observed and would always answer `deadline`.
        let budget = Self::budget(seconds);
        let quiet = Duration::from_secs(quiet_seconds.unwrap_or(2).max(1)).min(budget);

        let view = exec::wait_for_idle(&target, quiet, budget, &cancelled)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Wait until a pane writes something a caller is looking for.
    #[tool(
        description = "Wait until a pane writes matching text. Reads the pane's live output \
                       stream, so text that scrolls past between checks is still seen. Prefer \
                       run_command for commands you are sending yourself: it reports an exit \
                       status instead of guessing from output. Use this for output you did \
                       not author, such as a server logging that it is ready.",
        title = "Wait For Pane Text",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn wait_for_text(
        &self,
        Parameters(WaitForTextArgs {
            pane,
            patterns,
            stop,
            regex,
            match_case,
            seconds,
        }): Parameters<WaitForTextArgs>,
        cancelled: tokio_util::sync::CancellationToken,
    ) -> Result<Json<WaitView>, ErrorData> {
        let compile = |sources: Vec<String>| {
            Patterns::compile(&sources, regex, match_case).map_err(|(source, reason)| {
                bad_input(format!("pattern {source} is invalid: {reason}"))
            })
        };
        let wanted = compile(patterns.unwrap_or_default())?;
        let stops = compile(stop.unwrap_or_default())?;

        let target = self.find_pane(&pane).await?;
        let view = exec::wait_for_text(&target, &wanted, &stops, Self::budget(seconds), &cancelled)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Report what a pane has written since the last look.
    #[tool(
        description = "Read what a pane wrote since the previous call. The first call, with no \
                       cursor, starts watching and returns a cursor; later calls pass it back \
                       and receive only what is new. Use this to follow a pane over several \
                       turns without re-reading the whole screen. The answer says missed=true \
                       if output was dropped, which only happens when a pane outruns the \
                       buffer.",
        title = "Read New Pane Output",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn capture_since(
        &self,
        Parameters(CaptureSinceArgs { pane, cursor }): Parameters<CaptureSinceArgs>,
    ) -> Result<Json<Since>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        let cursor = cursor
            .as_deref()
            .map(Cursor::decode)
            .transpose()
            .map_err(|text| bad_input(format!("{text} is not a cursor this server issued")))?;
        if let Some(cursor) = &cursor
            && cursor.pane() != target.id().to_string()
        {
            return Err(bad_input(format!(
                "that cursor belongs to pane {}, not {pane}",
                cursor.pane()
            )));
        }

        let first = cursor.is_none();
        let since = self
            .tails
            .read(&target, cursor.as_ref())
            .await
            .map_err(|e| tmux_error(&e))?;

        // A tail can only report what it saw, and on the first call it has
        // seen nothing. Answering with the visible screen makes the tool
        // usable on its own rather than requiring a wasted first round trip.
        let text = if first {
            let lines = target
                .capture_with(CaptureOptions::visible())
                .await
                .map_err(|e| tmux_error(&e))?;
            lines
                .iter()
                .map(|line| line.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            since.text
        };

        Ok(Json(Since {
            pane: target.id().to_string(),
            text,
            cursor: since.cursor.encode(),
            missed: since.missed,
            closed: since.closed,
            // The first answer is the screen as it stands; every later one is
            // what the pane wrote since the cursor.
            first,
        }))
    }

    /// Wait for a `wait-for` channel to be signalled.
    #[tool(
        description = "Block until something signals a tmux wait-for channel. Pair this with \
                       a shell command that ends in `tmux wait-for -S <channel>` to \
                       synchronise with work this server did not start.",
        title = "Wait For Channel",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn wait_for_channel(
        &self,
        Parameters(ChannelArgs { channel, seconds }): Parameters<ChannelArgs>,
    ) -> Result<Json<ChannelWait>, ErrorData> {
        // This wait is a tmux client rather than a control-mode command, so
        // libtmux's own command timeout bounds it too. Asking for longer than
        // that would report a transport failure where the caller expects a
        // deadline, so the shorter of the two is the honest budget.
        let budget = Self::budget(seconds).min(self.server.default_timeout());

        // tmux blocks the client until the channel fires. Losing the race
        // drops the future, and libtmux kills the process it was waiting on,
        // so a deadline leaves nothing behind.
        let waited = tokio::time::timeout(
            budget,
            self.server
                .cmd(Command::new("wait-for").arg(channel.as_str())),
        )
        .await;

        let outcome = match waited {
            Ok(Ok(_)) => "signalled",
            // libtmux reaching its own limit first is the same event.
            Ok(Err(error)) if error.kind() == libtmux::ErrorKind::Timeout => "deadline",
            Ok(Err(error)) => return Err(tmux_error(&error)),
            Err(_) => "deadline",
        };

        Ok(Json(ChannelWait {
            channel,
            outcome: outcome.to_owned(),
        }))
    }

    /// Signal a `wait-for` channel.
    #[tool(
        description = "Signal a tmux wait-for channel, releasing whatever waits on it",
        title = "Signal Channel",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn signal_channel(
        &self,
        Parameters(ChannelArgs { channel, .. }): Parameters<ChannelArgs>,
    ) -> Result<Json<ChannelSignal>, ErrorData> {
        self.server
            .signal_channel(&channel)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(ChannelSignal { channel }))
    }

    /// How long a blocking tool may hold the caller's turn.
    ///
    /// An MCP call blocks the agent that made it, so an unbounded wait costs a
    /// whole turn with nothing to show. The ceiling is generous enough for a
    /// slow build and short enough that a wedged wait is an annoyance.
    fn budget(seconds: Option<u64>) -> Duration {
        Duration::from_secs(seconds.unwrap_or(30).clamp(1, 600))
    }

    /// Render panes as the protocol sees them, saying which one is our own.
    async fn render_panes(&self, panes: &[libtmux::Pane]) -> Panes {
        let socket = self.socket().await;
        let panes: Vec<_> = panes
            .iter()
            .map(|pane| self.pane_view(pane, socket))
            .collect();

        Panes { panes }
    }

    /// Describe one pane, including where it stands relative to this process.
    fn pane_view(&self, pane: &libtmux::Pane, socket: Option<&Path>) -> PaneView {
        let id = pane.id().to_string();
        PaneView {
            caller: self
                .caller
                .as_ref()
                .map_or(Relation::Unknown, |caller| caller.relation_to(&id, socket)),
            id,
            window_id: pane.window_id().to_string(),
            command: lossy_optional(pane.current_command()),
            path: lossy_optional(pane.current_path()),
            active: pane.is_active(),
        }
    }

    /// The socket path tmux itself reports for this server.
    ///
    /// Resolved once. Two calls racing compute the same answer, so the loser
    /// discarding its own is harmless.
    async fn socket(&self) -> Option<&Path> {
        if let Some(cached) = self.socket.get() {
            return cached.as_deref();
        }

        let resolved = self
            .server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("#{socket_path}"),
            )
            .await
            .ok()
            .map(|result| result.stdout_lossy().trim().to_owned())
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);

        // Only a real answer is kept. tmux cannot report a socket before it
        // has a session, and caching that emptiness would leave every later
        // caller comparison guessing for the life of the process.
        if resolved.is_some() {
            let _ = self.socket.set(resolved);
            return self.socket.get().and_then(Option::as_deref);
        }
        None
    }

    /// The pane this process runs in, if it is a pane on *this* server.
    ///
    /// `None` means no destructive command needs checking: either tmux did not
    /// start this process, or it started it somewhere else.
    async fn own_pane(&self) -> Option<&str> {
        let caller = self.caller.as_deref()?;
        let pane = caller.pane_id()?;
        let socket_name = self.server.socket_name().and_then(|name| name.to_str());
        caller
            .may_be_on(self.socket().await, socket_name)
            .then_some(pane)
    }

    /// The tools this server offers, after the tier has taken its cut.
    #[must_use]
    pub fn offered(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// The pane this process runs in, when tmux named one.
    ///
    /// Reported without checking it against the server, because this is for
    /// saying what the environment claimed rather than for deciding anything.
    #[must_use]
    pub fn caller_pane(&self) -> Option<&str> {
        self.caller.as_ref().and_then(|caller| caller.pane_id())
    }

    /// Refuse a command that would destroy the pane this process talks through.
    fn self_harm(what: &str, own: &str) -> ErrorData {
        ErrorData::invalid_params(
            format!(
                "refusing to kill this {what}: it holds pane {own}, the pane this MCP server \
                 runs in, so killing it would end this conversation. Run the command in a \
                 terminal if that is what you meant."
            ),
            // Its own kind, because this is the server declining rather than
            // tmux: an agent that reads `refused` might reasonably try a
            // different argument, and no argument gets past this one.
            Some(serde_json::json!({
                "kind": "self_protection",
                "retryable": false,
                "stale": false,
            })),
        )
    }

    /// Answer one resource, doing the same tmux work the matching tool does.
    ///
    /// Reusing the renderers keeps a resource and its tool from drifting into
    /// two descriptions of the same pane.
    async fn read_target(
        &self,
        uri: &str,
        target: resources::Target,
    ) -> Result<rmcp::model::ReadResourceResult, ErrorData> {
        use resources::Target;

        match target {
            Target::Server => {
                let sessions = self.server.sessions().await.map_err(|e| tmux_error(&e))?;
                resources::json(
                    uri,
                    &ServerView {
                        socket: self
                            .socket()
                            .await
                            .map(|path| path.to_string_lossy().into_owned()),
                        caller_pane: self.caller_pane().map(ToOwned::to_owned),
                        sessions: sessions.len(),
                    },
                )
            }
            Target::Sessions => {
                let sessions = self.server.sessions().await.map_err(|e| tmux_error(&e))?;
                resources::json(uri, &Self::render_sessions(&sessions))
            }
            Target::Windows => {
                let windows = self.server.windows().await.map_err(|e| tmux_error(&e))?;
                resources::json(uri, &Self::render_windows(&windows))
            }
            Target::Panes => {
                let panes = self.server.panes().await.map_err(|e| tmux_error(&e))?;
                resources::json(uri, &self.render_panes(&panes).await)
            }
            Target::Session(name) => {
                // A URI that names one session answers with that session, not
                // a collection holding it. The list wrapper the tools use is
                // there because structured tool content has to be an object;
                // a resource body has no such constraint.
                let session = self.find_session(&name).await?;
                let mut rendered = Self::render_sessions(std::slice::from_ref(&session));
                resources::json(uri, &rendered.sessions.remove(0))
            }
            Target::SessionWindows(name) => {
                let session = self.find_session(&name).await?;
                let windows = session.windows().await.map_err(|e| tmux_error(&e))?;
                resources::json(uri, &Self::render_windows(&windows))
            }
            Target::Window(name, index) => {
                let session = self.find_session(&name).await?;
                let windows = session.windows().await.map_err(|e| tmux_error(&e))?;
                let window = windows
                    .into_iter()
                    .find(|window| window.index().to_string() == index)
                    .ok_or_else(|| object_gone("window", &format!("{name}:{index}")))?;
                let mut rendered = Self::render_windows(std::slice::from_ref(&window));
                resources::json(uri, &rendered.windows.remove(0))
            }
            Target::Pane(id) => {
                let pane = self.find_pane(&id).await?;
                let mut rendered = self.render_panes(std::slice::from_ref(&pane)).await;
                resources::json(uri, &rendered.panes.remove(0))
            }
            Target::PaneContent(id) => {
                let pane = self.find_pane(&id).await?;
                let lines = pane
                    .capture_with(CaptureOptions::visible())
                    .await
                    .map_err(|e| tmux_error(&e))?;
                let body = lines
                    .iter()
                    .map(|line| line.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(resources::text(uri, body))
            }
        }
    }

    /// Resolve a window id, reporting an unknown one as invalid input.
    async fn find_window(&self, id: &str) -> Result<libtmux::Window, ErrorData> {
        self.server
            .windows()
            .await
            .map_err(|e| tmux_error(&e))?
            .into_iter()
            .find(|window| window.id().to_string() == id)
            .ok_or_else(|| object_gone("window", id))
    }

    /// Resolve a pane id, reporting an unknown one as invalid input.
    async fn find_pane(&self, id: &str) -> Result<libtmux::Pane, ErrorData> {
        self.server
            .panes()
            .await
            .map_err(|e| tmux_error(&e))?
            .into_iter()
            .find(|pane| pane.id().to_string() == id)
            .ok_or_else(|| object_gone("pane", id))
    }

    /// Resolve a session by name, reporting an unknown one as invalid input.
    async fn find_session(&self, name: &str) -> Result<libtmux::Session, ErrorData> {
        self.server
            .sessions()
            .await
            .map_err(|e| tmux_error(&e))?
            .into_iter()
            .find(|session| session.name() == name.as_bytes())
            .ok_or_else(|| object_gone("session", name))
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for TmuxTools {
    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, ErrorData> {
        Ok(resources::listed())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListResourceTemplatesResult, ErrorData> {
        Ok(resources::templates())
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, ErrorData> {
        let uri = request.uri.as_str();
        let target = resources::Target::parse(uri).ok_or_else(|| {
            // The same classification the tools use, so a client that reads
            // `data` does not need a second vocabulary for resources.
            ErrorData::invalid_params(
                format!("no resource {uri}"),
                Some(serde_json::json!({
                    "kind": "invalid_input",
                    "retryable": false,
                    "stale": false,
                })),
            )
        })?;
        Ok(self.read_target(uri, target).await?.into())
    }

    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive], so it is built from the default
        // rather than named field by field.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .enable_resources()
            .build();
        let mut instructions = String::from(INSTRUCTIONS);
        instructions.push_str("\n\nSURFACE: the ");
        instructions.push_str(self.safety.name());
        instructions.push_str(match self.safety {
            Safety::ReadOnly => " tier — nothing here changes the server.",
            Safety::Mutating => {
                " tier — you can create, split and type, but the tools that \
                 destroy work are not offered."
            }
            Safety::Destructive => {
                " tier — every tool is offered, including the four that destroy work."
            }
        });
        instructions
            .push_str(" An operator sets this with TMUX_MCP_SAFETY=readonly|mutating|destructive.");
        // Saying where this process sits saves the agent a round trip, and
        // makes the refusal it would otherwise hit from kill_session
        // predictable rather than surprising.
        if let Some(pane) = self.caller.as_ref().and_then(|caller| caller.pane_id()) {
            instructions.push_str("\n\nWHERE YOU ARE: this server runs in pane ");
            instructions.push_str(pane);
            instructions.push_str(
                ". Pane listings mark it caller=self, and the tools that would destroy \
                 it refuse.",
            );
        }
        info.instructions = Some(instructions);
        info
    }
}
