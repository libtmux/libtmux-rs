//! The saved placement a window selection or move addresses.

use libtmux_tokio::{ServerGeneration, SessionId, Window, WindowId, WindowLink};
use serde::{Deserialize, Serialize};

use crate::TmuxTools;
use crate::tools::error::{ToolError, bad_input, object_gone, tmux_error};

/// The daemon generation which supplied a listed window placement.
///
/// # Examples
///
/// ```
/// use tmux_mcp::WindowLinkGeneration;
///
/// let generation: WindowLinkGeneration = serde_json::from_str(
///     r#"{"pid":123,"started":456}"#,
/// )?;
/// assert_eq!(generation.pid, 123);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Observed daemon generation.")]
pub struct WindowLinkGeneration {
    /// Server process ID.
    #[schemars(range(min = 1))]
    pub pid: u32,
    /// Whole-second Unix start time.
    pub started: i64,
}

/// One listed window at an exact session index on the observed daemon.
///
/// Copy `WindowView.link` into `select_window` or `move_window`. A withdrawn
/// or reindexed placement is refused even when another link keeps the window
/// alive. This reference belongs to the MCP server's configured endpoint.
///
/// # Examples
///
/// ```
/// use tmux_mcp::WindowLinkRef;
///
/// let link: WindowLinkRef = serde_json::from_value(serde_json::json!({
///     "session_id": "$1", "index": 2, "window_id": "@3",
///     "generation": {"pid": 123, "started": 456},
/// }))?;
/// assert_eq!(link.session_id, "$1");
/// assert_eq!(link.index, 2);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Saved window placement.")]
pub struct WindowLinkRef {
    /// Session ID.
    #[schemars(regex(pattern = r"^\$[0-9]+$"))]
    pub session_id: String,
    /// Session index.
    pub index: i32,
    /// Window ID.
    #[schemars(regex(pattern = r"^@[0-9]+$"))]
    pub window_id: String,
    /// Observed daemon.
    pub generation: WindowLinkGeneration,
}

impl WindowLinkRef {
    pub(crate) fn observed(window: &Window) -> Self {
        let generation = window.key().generation();
        Self {
            session_id: window.session_id().to_string(),
            index: window.index(),
            window_id: window.id().to_string(),
            generation: WindowLinkGeneration {
                pid: generation.pid(),
                started: generation.unix_start_time(),
            },
        }
    }

    fn parsed(&self) -> Result<(SessionId, WindowId, ServerGeneration), ToolError> {
        let session = self.session_id.parse().map_err(|_| {
            bad_input("link.session_id must be a $-prefixed session ID from list_windows")
        })?;
        let window = self.window_id.parse().map_err(|_| {
            bad_input("link.window_id must be an @-prefixed window ID from list_windows")
        })?;
        if self.generation.pid == 0 {
            return Err(bad_input(
                "link.generation.pid must be the listed daemon's process ID",
            ));
        }
        Ok((
            session,
            window,
            ServerGeneration::from_raw(self.generation.pid, self.generation.started),
        ))
    }

    fn gone(&self) -> ToolError {
        rmcp::model::ErrorData::invalid_params(
            format!(
                "window link {}:{} no longer names {}; refresh list_windows",
                self.session_id, self.index, self.window_id
            ),
            Some(serde_json::json!({
                "kind": "link_gone", "retryable": false, "stale": true,
            })),
        )
        .into()
    }
}

impl TmuxTools {
    pub(crate) async fn find_window_link(
        &self,
        reference: &WindowLinkRef,
    ) -> Result<WindowLink, ToolError> {
        let (session, window, generation) = reference.parsed()?;
        self.server
            .require_generation(generation)
            .await
            .map_err(|error| tmux_error(&error))?;
        let links = self
            .server
            .window_links()
            .await
            .map_err(|error| tmux_error(&error))?;
        let mut window_exists = false;
        for link in links {
            let snapshot = link.snapshot();
            let found = snapshot.window().generation();
            if found != generation {
                return Err(tmux_error(&libtmux_tokio::Error::ServerGenerationChanged {
                    expected: generation,
                    found,
                }));
            }
            if snapshot.window().id() == window {
                window_exists = true;
                if snapshot.session() == &session && snapshot.index() == reference.index {
                    return Ok(link);
                }
            }
        }
        self.server
            .require_generation(generation)
            .await
            .map_err(|error| tmux_error(&error))?;
        if window_exists {
            Err(reference.gone())
        } else {
            Err(object_gone("window", &reference.window_id))
        }
    }
}
