//! A Model Context Protocol server exposing tmux through `libtmux`.
//!
//! The server freezes one 47-tool surface at startup from the unordered
//! `inspect`, `manage`, `execute`, and `teardown` toolsets. Every native route
//! carries its process reach, tmux effects, output classes, interpreter sinks,
//! and whole-call annotations in one typed manifest. The effective manifest is
//! available as `tmux://capabilities`.
//!
//! The dependency runs one way. `libtmux` knows nothing about MCP.
//!
//! # Startup boundary
//!
//! One process selects one socket. The binary uses the dedicated
//! `libtmux-mcp` socket and minimal configuration by default; explicit socket
//! or configuration settings carry conservative provenance. Teardown is in
//! the implicit default only for a newly dedicated minimal daemon. The
//! `LIBTMUX_TOOLSETS`, `LIBTMUX_TOOLS`, and `LIBTMUX_EXCLUDE_TOOLS` settings
//! are parsed before tmux access, and exclusions win.
//!
//! # Trust boundary
//!
//! Pane input and pane commands run with the tmux user's permissions. Pane
//! output may be sensitive or untrusted; tmux environment values may contain
//! secrets; hooks may contain executable configuration. Configured-process
//! routes accept neither command nor environment payloads. There is no public
//! host-command route.
//!
//! # Knowing where you are
//!
//! An inherited `TMUX` and `TMUX_PANE` identify the caller only after socket
//! provenance matches. Pane listings report `self`, `other`, or `unknown`, and
//! teardown routes fail closed when the target may contain the caller.

#![forbid(unsafe_code)]

pub mod cli;
pub mod resources;

mod caller;
mod exec;
mod identity;
mod manifest;
mod model;
mod policy;
mod retained;
mod run_request;
mod schema;
mod tail;
mod text;
mod tools;
mod views;

pub use caller::{CallerIdentity, Relation};
pub use exec::{RunOutcome, RunView, WaitOutcome, WaitView};
pub use manifest::CapabilityReport;
pub use model::*;
pub use policy::{
    Builder, EXCLUDE_TOOLS_ENV, RETIRED_RUST_SAFETY_ENV, RETIRED_SAFETY_ENV, Reporter, Selection,
    SocketProvenance, SurfaceError, TOOLS_ENV, TOOLSETS_ENV, Toolset,
};
pub use tail::Cursor;
pub use views::*;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use libtmux::Server;
use rmcp::model::{ErrorData, ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool_handler};

use tail::Tails;

/// A tmux server presented as MCP tools.
#[derive(Clone)]
pub struct TmuxTools {
    server: Arc<Server>,
    /// Where this process is running, when tmux started it.
    caller: Option<Arc<CallerIdentity>>,
    /// The startup-frozen authority behind the native tool router.
    capability_report: Arc<CapabilityReport>,
    /// The server's own socket path, resolved once and kept.
    ///
    /// `Server::socket_path` reports what this crate was configured with,
    /// which for a named socket is a reconstruction. tmux knows the real one,
    /// and every caller comparison rests on it.
    socket: Arc<OnceLock<Option<PathBuf>>>,
    /// Live per-pane output, for `capture_since`.
    tails: Arc<Tails>,
    /// The startup-resolved router used for both listing and dispatch.
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
    /// Aggregate-only child routes, retained without advertising direct calls.
    nested_tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

// The resolved socket path stays out, as `ServerIdentity`'s own `Debug` keeps
// it out: this server's logs go wherever the agent's do.
impl std::fmt::Debug for TmuxTools {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TmuxTools")
            .field("server", &self.server)
            .field("caller", &self.caller)
            .finish_non_exhaustive()
    }
}

/// Server-wide guidance supplied once during initialization.
const INSTRUCTIONS: &str = concat!(
    "Inspect and drive one tmux server. The hierarchy is Server > Session > \
     Window > Pane. Prefer stable ids: $ for sessions, @ for windows, and % for panes.",
    "\n\nTOOLSETS: inspect reads state and output; manage changes tmux state without \
     executable input; execute starts configured processes or supplies pane input and \
     commands; teardown deletes state. The startup-frozen surface is reported at \
     tmux://capabilities.",
    "\n\nTRUST: pane commands and input run with the tmux user's permissions. Pane \
     output may be sensitive or untrusted, environment values may contain secrets, and \
     hooks may contain executable configuration.",
    "\n\nWAIT, DO NOT POLL: wait_for_text and capture_since observe live output. \
     run_shell_command reports command completion. Inspect state before retrying any call \
     that reports partial_effect or an unknown outcome.",
);

#[tool_handler(router = self.tool_router)]
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
        if uri != resources::CAPABILITIES_URI {
            return Err(ErrorData::invalid_params(
                format!("no resource {uri}"),
                Some(serde_json::json!({
                    "kind": "invalid_input",
                    "retryable": false,
                    "stale": false,
                })),
            ));
        }
        Ok(resources::capabilities(self.capability_report.as_ref())?.into())
    }

    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive], so it is built from the default
        // rather than named field by field.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        let mut instructions = String::from(INSTRUCTIONS);
        instructions.push_str(
            "\n\nTOOL SURFACE IS NOT AUTHORIZATION: startup-frozen toolsets shape what this \
             server advertises and accepts. Pane input and pane commands still run with the \
             tmux user's authority, and configured tmux behavior may add effects.",
        );
        // This is launch context, not a claim about the selected server. The
        // socket comparison that marks a listing as `self` happens later.
        if let Some(pane) = self.caller.as_ref().and_then(|caller| caller.pane_id()) {
            instructions.push_str("\n\nLAUNCH CONTEXT: this process inherited pane ");
            instructions.push_str(pane);
            instructions.push_str(
                " from tmux. If its socket matches the selected server, pane listings \
                 mark it caller=self. Teardown tools use a conservative caller guard.",
            );
        }
        info.instructions = Some(instructions);
        info
    }
}
