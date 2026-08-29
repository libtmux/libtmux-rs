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
//! Its deadline ends the waiting rather than the command and returns a job id
//! for following or cancelling the run.
//!
//! # Driving a pane
//!
//! `send_keys` both types and presses. Its `text` is sent literally, so `C-c`
//! there types three characters; its `keys` are tmux key names, which is the
//! only way to press something with no character of its own — `C-c` to
//! interrupt, `Escape`, `Up`. Use `cancel_job` for a run with a job id;
//! `send_keys` addresses whatever the pane is doing when the key arrives.
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
mod model;
mod policy;
mod prompts;
mod schema;
mod tail;
mod text;
mod tools;
mod views;

pub use caller::{CallerIdentity, Relation};
pub use exec::{IdleOutcome, IdleView, RunOutcome, RunView, WaitOutcome, WaitView};
pub use model::*;
pub use policy::{
    Asking, Builder, CONFIRM_ENV, Confirmation, Reporter, SAFETY_ENV, Safety, confirm_from_env,
};
pub use prompts::{PanePrompt, RunPrompt};
pub use tail::Cursor;
pub use views::*;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use libtmux::Server;
use rmcp::model::{ErrorData, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, prompt_handler, tool_handler};

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
    /// Whether a person is asked before work is destroyed.
    confirm: bool,
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
     status. If it stops waiting first, continue from its job id with \
     job_status or cancel_job. start_command returns that id immediately, for \
     anything slow or for several at once. \
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
                " tier — every tool is offered, including plans that destroy work."
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
