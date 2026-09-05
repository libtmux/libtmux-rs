#![allow(
    missing_docs,
    reason = "native route descriptions are the protocol documentation"
)]

use std::collections::BTreeSet;

use libtmux::{PaneSize, SplitDirection, SplitOptions};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolRequestParams, ErrorData};
use rmcp::schemars;
use rmcp::service::RequestContext;
use rmcp::{ServerHandler, tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::{PaneView, SessionView, TmuxTools, WindowView};

use super::error::{bad_input, object_gone, tmux_error};
use super::lossy;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameSessionArgs {
    pub(crate) session: String,
    pub(crate) name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameWindowArgs {
    pub(crate) window: String,
    pub(crate) name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowSizeArgs {
    pub(crate) window: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MoveWindowArgs {
    pub(crate) window: String,
    pub(crate) destination_session: String,
    pub(crate) destination_index: i32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SwapPaneArgs {
    pub(crate) source_pane: String,
    pub(crate) target_pane: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaneTitleArgs {
    pub(crate) pane: String,
    pub(crate) title: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PositionArgs {
    pub(crate) window: String,
    pub(crate) corner: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct VariablesArgs {
    pub(crate) pane: String,
    pub(crate) format: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct VariablesValue {
    pub(crate) value: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionFlagArgs {
    pub(crate) session: Option<String>,
    pub(crate) enabled: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct HistoryLimitArgs {
    pub(crate) session: Option<String>,
    pub(crate) limit: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowFlagArgs {
    pub(crate) window: String,
    pub(crate) enabled: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct SettingChanged {
    pub(crate) name: String,
    pub(crate) target: Option<String>,
    pub(crate) value: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateWindowArgs {
    pub(crate) session: String,
    pub(crate) name: Option<String>,
    pub(crate) start_directory: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SplitWindowArgs {
    pub(crate) pane: String,
    #[schemars(with = "Option<crate::schema::SplitDirectionSchema>")]
    pub(crate) direction: Option<String>,
    pub(crate) percent: Option<u32>,
    pub(crate) start_directory: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RespawnArgs {
    pub(crate) pane: String,
    pub(crate) kill_first: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendOperation {
    pub(crate) pane: String,
    pub(crate) text: Option<String>,
    pub(crate) keys: Option<Vec<String>>,
    pub(crate) enter: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendBatchArgs {
    pub(crate) operations: Vec<SendOperation>,
    pub(crate) continue_on_error: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BatchItem {
    pub(crate) index: usize,
    pub(crate) tool: String,
    pub(crate) success: bool,
    pub(crate) result: Option<serde_json::Value>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct BatchResult {
    pub(crate) results: Vec<BatchItem>,
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadOperation {
    pub(crate) tool: String,
    #[serde(default)]
    pub(crate) arguments: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadBatchArgs {
    pub(crate) operations: Vec<ReadOperation>,
    pub(crate) continue_on_error: bool,
}

fn format_variables_are_bounded(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'#' {
            at += 1;
            continue;
        }
        if bytes.get(at + 1) != Some(&b'{') {
            return false;
        }
        let Some(close) = bytes[at + 2..].iter().position(|byte| *byte == b'}') else {
            return false;
        };
        let name = &bytes[at + 2..at + 2 + close];
        if name.is_empty()
            || !matches!(name[0], b'A'..=b'Z' | b'a'..=b'z' | b'_' | b'@')
            || !name[1..]
                .iter()
                .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        {
            return false;
        }
        at += close + 3;
    }
    true
}

#[tool_router(router = contract_router, vis = "pub(super)")]
impl TmuxTools {
    #[tool(
        description = "Return metadata for one session",
        title = "Get Session Info",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {
            "session" => [TmuxArgument]
        })
    )]
    pub async fn get_session_info(
        &self,
        Parameters(crate::SessionArgs { session }): Parameters<crate::SessionArgs>,
    ) -> Result<Json<SessionView>, ErrorData> {
        let session = self.find_session(&session).await?;
        Ok(Json(SessionView {
            id: session.id().to_string(),
            name: lossy(session.name()),
            windows: session.window_count(),
            attached: session.is_attached(),
        }))
    }

    #[tool(
        description = "Return metadata for one window",
        title = "Get Window Info",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {
            "window" => [TmuxArgument]
        })
    )]
    pub async fn get_window_info(
        &self,
        Parameters(crate::WindowArgs { window }): Parameters<crate::WindowArgs>,
    ) -> Result<Json<WindowView>, ErrorData> {
        Ok(Json(Self::one_window(&self.find_window(&window).await?)))
    }

    #[tool(
        description = "Return metadata for one pane",
        title = "Get Pane Info",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {
            "pane" => [TmuxArgument]
        })
    )]
    pub async fn get_pane_info(
        &self,
        Parameters(crate::PaneArgs { pane }): Parameters<crate::PaneArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let pane = self.find_pane(&pane).await?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&pane, socket)))
    }

    #[tool(
        description = "Find the pane touching a named window corner",
        title = "Find Pane By Position",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {
            "window" => [TmuxArgument],
            "corner" => [None]
        })
    )]
    pub async fn find_pane_by_position(
        &self,
        Parameters(PositionArgs { window, corner }): Parameters<PositionArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let panes = self
            .find_window(&window)
            .await?
            .panes()
            .await
            .map_err(|error| tmux_error(&error))?;
        let pane = panes
            .into_iter()
            .find(|pane| match corner.as_str() {
                "top-left" => pane.is_at_top() && pane.is_at_left(),
                "top-right" => pane.is_at_top() && pane.is_at_right(),
                "bottom-left" => pane.is_at_bottom() && pane.is_at_left(),
                "bottom-right" => pane.is_at_bottom() && pane.is_at_right(),
                _ => false,
            })
            .ok_or_else(|| {
                if matches!(
                    corner.as_str(),
                    "top-left" | "top-right" | "bottom-left" | "bottom-right"
                ) {
                    object_gone("pane at corner", &corner)
                } else {
                    bad_input(format!(
                        "corner must be top-left, top-right, bottom-left, or bottom-right, not {corner}"
                    ))
                }
            })?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&pane, socket)))
    }

    #[tool(
        description = "Read a bounded set of tmux variables against one pane",
        title = "Get tmux Variables",
        meta = crate::capability_meta!(
            Inspect, None,
            effects = [Observe],
            outputs = [TmuxMetadata, ConfiguredCommand],
            secrets = true,
            untrusted = true,
            sinks = {"pane" => [TmuxArgument], "format" => [TmuxFormat]},
            literalized = [],
            validated = ["format"],
            nested = [],
            self_bounded = true,
            always_load = false,
        )
    )]
    pub async fn get_tmux_variables(
        &self,
        Parameters(VariablesArgs { pane, format }): Parameters<VariablesArgs>,
    ) -> Result<Json<VariablesValue>, ErrorData> {
        if !format_variables_are_bounded(&format) {
            return Err(bad_input(
                "format accepts literal text and #{variable} references only".to_owned(),
            ));
        }
        let pane = self.find_pane(&pane).await?;
        let value = self
            .server
            .format(Some(&pane), &format)
            .await
            .map_err(|error| tmux_error(&error))?;
        Ok(Json(VariablesValue {
            value: lossy(&value),
        }))
    }

    #[tool(
        description = "Rename one session",
        title = "Rename Session",
        meta = crate::capability_meta!(
            Manage, None,
            effects = [Change], outputs = [TmuxMetadata], secrets = true, untrusted = true,
            sinks = {"session" => [TmuxArgument], "name" => [TmuxFormat]},
            literalized = ["name"], validated = [], nested = [], self_bounded = false,
            always_load = false,
        )
    )]
    pub async fn rename_session(
        &self,
        Parameters(RenameSessionArgs { session, name }): Parameters<RenameSessionArgs>,
    ) -> Result<Json<SessionView>, ErrorData> {
        let mut session = self.find_session(&session).await?;
        session
            .rename(libtmux::escape_format(name))
            .await
            .map_err(|error| tmux_error(&error))?;
        Ok(Json(SessionView {
            id: session.id().to_string(),
            name: lossy(session.name()),
            windows: session.window_count(),
            attached: session.is_attached(),
        }))
    }

    #[tool(
        description = "Rename one window",
        title = "Rename Window",
        meta = crate::capability_meta!(
            Manage, None,
            effects = [Change], outputs = [TmuxMetadata], secrets = true, untrusted = true,
            sinks = {"window" => [TmuxArgument], "name" => [TmuxFormat]},
            literalized = ["name"], validated = [], nested = [], self_bounded = false,
            always_load = false,
        )
    )]
    pub async fn rename_window(
        &self,
        Parameters(RenameWindowArgs { window, name }): Parameters<RenameWindowArgs>,
    ) -> Result<Json<WindowView>, ErrorData> {
        let mut window = self.find_window(&window).await?;
        window
            .rename(libtmux::escape_format(name))
            .await
            .map_err(|error| tmux_error(&error))?;
        Ok(Json(Self::one_window(&window)))
    }

    #[tool(
        description = "Resize one window to exact cell dimensions",
        title = "Resize Window",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "window" => [TmuxArgument], "width" => [None], "height" => [None]
        })
    )]
    pub async fn resize_window(
        &self,
        Parameters(WindowSizeArgs {
            window,
            width,
            height,
        }): Parameters<WindowSizeArgs>,
    ) -> Result<Json<WindowView>, ErrorData> {
        let mut window = self.find_window(&window).await?;
        window
            .resize(width, height)
            .await
            .map_err(|error| tmux_error(&error))?;
        Ok(Json(Self::one_window(&window)))
    }

    #[tool(
        description = "Move one window to a session and index",
        title = "Move Window",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "window" => [TmuxArgument],
            "destination_session" => [TmuxArgument],
            "destination_index" => [None]
        })
    )]
    pub async fn move_window(
        &self,
        Parameters(MoveWindowArgs {
            window,
            destination_session,
            destination_index,
        }): Parameters<MoveWindowArgs>,
    ) -> Result<Json<WindowView>, ErrorData> {
        let session = self.find_session(&destination_session).await?;
        let mut window = self.find_window(&window).await?;
        window
            .move_to(&session, destination_index)
            .await
            .map_err(|error| tmux_error(&error))?;
        Ok(Json(Self::one_window(&window)))
    }

    #[tool(
        description = "Swap the positions of two panes",
        title = "Swap Panes",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "source_pane" => [TmuxArgument], "target_pane" => [TmuxArgument]
        })
    )]
    pub async fn swap_pane(
        &self,
        Parameters(SwapPaneArgs {
            source_pane,
            target_pane,
        }): Parameters<SwapPaneArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let target = self.find_pane(&target_pane).await?;
        let mut source = self.find_pane(&source_pane).await?;
        source
            .swap_with(&target)
            .await
            .map_err(|error| tmux_error(&error))?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&source, socket)))
    }

    #[tool(
        description = "Set one pane's title",
        title = "Set Pane Title",
        meta = crate::capability_meta!(
            Manage, None,
            effects = [Change], outputs = [TmuxMetadata], secrets = true, untrusted = true,
            sinks = {"pane" => [TmuxArgument], "title" => [TmuxFormat]},
            literalized = ["title"], validated = [], nested = [], self_bounded = false,
            always_load = false,
        )
    )]
    pub async fn set_pane_title(
        &self,
        Parameters(PaneTitleArgs { pane, title }): Parameters<PaneTitleArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let mut pane = self.find_pane(&pane).await?;
        pane.set_title(libtmux::escape_format(title))
            .await
            .map_err(|error| tmux_error(&error))?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&pane, socket)))
    }

    #[tool(
        description = "Enter copy mode in one pane",
        title = "Enter Copy Mode",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "pane" => [TmuxArgument]
        })
    )]
    pub async fn enter_copy_mode(
        &self,
        Parameters(crate::PaneArgs { pane }): Parameters<crate::PaneArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let pane = self.find_pane(&pane).await?;
        pane.copy_mode().await.map_err(|error| tmux_error(&error))?;
        let pane = self.find_pane(pane.id().as_ref()).await?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&pane, socket)))
    }

    #[tool(
        description = "Exit copy mode or another pane mode",
        title = "Exit Copy Mode",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "pane" => [TmuxArgument]
        })
    )]
    pub async fn exit_copy_mode(
        &self,
        Parameters(crate::PaneArgs { pane }): Parameters<crate::PaneArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let pane = self.find_pane(&pane).await?;
        pane.exit_mode().await.map_err(|error| tmux_error(&error))?;
        let pane = self.find_pane(pane.id().as_ref()).await?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&pane, socket)))
    }

    #[tool(
        description = "Set mouse handling for a session or the global session default",
        title = "Set Mouse Enabled",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "session" => [TmuxArgument], "enabled" => [None]
        })
    )]
    pub async fn set_mouse_enabled(
        &self,
        Parameters(SessionFlagArgs { session, enabled }): Parameters<SessionFlagArgs>,
    ) -> Result<Json<SettingChanged>, ErrorData> {
        let value = if enabled { "on" } else { "off" };
        if let Some(name) = session.as_deref() {
            self.find_session(name)
                .await?
                .set_option("mouse", value)
                .await
                .map_err(|error| tmux_error(&error))?;
        } else {
            self.server
                .set_global_option("mouse", value)
                .await
                .map_err(|error| tmux_error(&error))?;
        }
        Ok(Json(SettingChanged {
            name: "mouse".to_owned(),
            target: session,
            value: value.to_owned(),
        }))
    }

    #[tool(
        description = "Set the scrollback history limit for a session or its global default",
        title = "Set History Limit",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "session" => [TmuxArgument], "limit" => [None]
        })
    )]
    pub async fn set_history_limit(
        &self,
        Parameters(HistoryLimitArgs { session, limit }): Parameters<HistoryLimitArgs>,
    ) -> Result<Json<SettingChanged>, ErrorData> {
        if let Some(name) = session.as_deref() {
            self.find_session(name)
                .await?
                .set_option("history-limit", limit.to_string())
                .await
                .map_err(|error| tmux_error(&error))?;
        } else {
            self.server
                .set_global_option("history-limit", limit.to_string())
                .await
                .map_err(|error| tmux_error(&error))?;
        }
        Ok(Json(SettingChanged {
            name: "history-limit".to_owned(),
            target: session,
            value: limit.to_string(),
        }))
    }

    #[tool(
        description = "Create a window running its configured process",
        title = "Create Window",
        meta = crate::capability_meta!(
            Execute, ConfiguredProcess,
            effects = [Change], outputs = [TmuxMetadata], secrets = true, untrusted = true,
            sinks = {
                "session" => [TmuxArgument], "name" => [TmuxFormat],
                "start_directory" => [FilesystemPath, TmuxFormat]
            },
            literalized = ["name", "start_directory"], validated = [], nested = [],
            self_bounded = false, always_load = false,
        )
    )]
    pub async fn create_window(
        &self,
        Parameters(CreateWindowArgs {
            session,
            name,
            start_directory,
        }): Parameters<CreateWindowArgs>,
    ) -> Result<Json<WindowView>, ErrorData> {
        let session = self.find_session(&session).await?;
        let mut options = name.map(libtmux::escape_format).map_or_else(
            libtmux::NewWindowOptions::unnamed,
            libtmux::NewWindowOptions::new,
        );
        if let Some(directory) = start_directory {
            options = options.start_directory(libtmux::escape_format(directory));
        }
        let window = session
            .new_window(options)
            .await
            .map_err(|error| tmux_error(&error))?;
        Ok(Json(Self::one_window(&window)))
    }

    #[tool(
        description = "Split a window and start the configured process with no command payload",
        title = "Split Window",
        meta = crate::capability_meta!(
            Execute, ConfiguredProcess,
            effects = [Change], outputs = [TmuxMetadata], secrets = true, untrusted = true,
            sinks = {
                "pane" => [TmuxArgument], "direction" => [None], "percent" => [None],
                "start_directory" => [FilesystemPath, TmuxFormat]
            },
            literalized = ["start_directory"], validated = [], nested = [],
            self_bounded = false, always_load = false,
        )
    )]
    pub async fn split_window(
        &self,
        Parameters(SplitWindowArgs {
            pane,
            direction,
            percent,
            start_directory,
        }): Parameters<SplitWindowArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let direction = match direction.as_deref() {
            None | Some("below") => SplitDirection::Below,
            Some("above") => SplitDirection::Above,
            Some("left") => SplitDirection::Left,
            Some("right") => SplitDirection::Right,
            Some(other) => return Err(bad_input(format!("unknown split direction {other}"))),
        };
        let mut options = SplitOptions::new(direction);
        if let Some(percent) = percent {
            if !(1..=100).contains(&percent) {
                return Err(bad_input(format!(
                    "percent must be 1 through 100, not {percent}"
                )));
            }
            options = options.size(PaneSize::Percent(percent));
        }
        if let Some(directory) = start_directory {
            options = options.start_directory(libtmux::escape_format(directory));
        }
        let created = self
            .find_pane(&pane)
            .await?
            .split(options)
            .await
            .map_err(|error| tmux_error(&error))?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&created, socket)))
    }

    #[tool(
        name = "respawn_pane",
        description = "Restart a pane's configured process with no command payload",
        title = "Respawn Pane",
        meta = crate::capability_meta!(Execute, ConfiguredProcess, [Change, Delete], [TmuxMetadata], true, true, {
            "pane" => [TmuxArgument], "kill_first" => [None]
        })
    )]
    pub async fn respawn_pane_configured(
        &self,
        Parameters(RespawnArgs { pane, kill_first }): Parameters<RespawnArgs>,
    ) -> Result<Json<PaneView>, ErrorData> {
        let mut pane = self.find_pane(&pane).await?;
        pane.respawn(None::<String>, kill_first)
            .await
            .map_err(|error| tmux_error(&error))?;
        let socket = self.socket().await;
        Ok(Json(self.pane_view(&pane, socket)))
    }

    #[tool(
        name = "set_synchronize_panes",
        description = "Set whether input to a window is copied to every pane. Enabling this \
                       duplicates subsequent pane input to every pane in the window, amplifying \
                       what one send_keys call reaches.",
        title = "Set Synchronize Panes",
        meta = crate::capability_meta!(
            Execute, None,
            effects = [Change], outputs = [TmuxMetadata], secrets = true, untrusted = true,
            sinks = {"window" => [TmuxArgument], "enabled" => [None]},
            literalized = [], validated = [], nested = [],
            amplifies_future_input = true,
            self_bounded = false, always_load = false,
        )
    )]
    pub async fn set_synchronize_panes(
        &self,
        Parameters(WindowFlagArgs { window, enabled }): Parameters<WindowFlagArgs>,
    ) -> Result<Json<SettingChanged>, ErrorData> {
        let value = if enabled { "on" } else { "off" };
        self.find_window(&window)
            .await?
            .set_option("synchronize-panes", value)
            .await
            .map_err(|error| tmux_error(&error))?;
        Ok(Json(SettingChanged {
            name: "synchronize-panes".to_owned(),
            target: Some(window),
            value: value.to_owned(),
        }))
    }

    #[tool(
        description = "Send an ordered batch of input operations to panes",
        title = "Send Keys Batch",
        meta = crate::capability_meta!(Execute, PaneInput, [Change], [TmuxMetadata], true, true, {
            "operations" => [PaneInput], "continue_on_error" => [None]
        })
    )]
    pub async fn send_keys_batch(
        &self,
        Parameters(SendBatchArgs {
            operations,
            continue_on_error,
        }): Parameters<SendBatchArgs>,
    ) -> Result<Json<BatchResult>, ErrorData> {
        if operations.is_empty() || operations.len() > 64 {
            return Err(bad_input(
                "operations must contain 1 through 64 items".to_owned(),
            ));
        }
        let mut results = Vec::with_capacity(operations.len());
        for (index, operation) in operations.into_iter().enumerate() {
            let tool = "send_keys".to_owned();
            let outcome = self
                .send_keys(Parameters(crate::SendKeysArgs {
                    pane: operation.pane,
                    text: operation.text,
                    keys: operation.keys,
                    enter: operation.enter,
                }))
                .await;
            match outcome {
                Ok(value) => results.push(BatchItem {
                    index,
                    tool,
                    success: true,
                    result: serde_json::to_value(value.0).ok(),
                    error: None,
                }),
                Err(error) => {
                    results.push(BatchItem {
                        index,
                        tool,
                        success: false,
                        result: None,
                        error: Some(error.message.into_owned()),
                    });
                    if !continue_on_error {
                        break;
                    }
                }
            }
        }
        let succeeded = results.iter().filter(|item| item.success).count();
        Ok(Json(BatchResult {
            failed: results.len() - succeeded,
            succeeded,
            results,
        }))
    }

    #[tool(
        description = "Call a bounded serial batch of enabled inspect tools",
        title = "Call Read Tools Batch",
        meta = crate::capability_meta!(
            Inspect, None,
            effects = [Observe, Change],
            outputs = [TmuxMetadata, TerminalContent, ProcessEnvironment, ConfiguredCommand],
            secrets = true,
            untrusted = true,
            sinks = {"operations" => [NestedTool], "continue_on_error" => [None]},
            literalized = [],
            validated = [],
            nested = [
                "list_sessions", "list_windows", "list_panes", "get_server_info",
                "get_session_info", "get_window_info", "get_pane_info", "capture_pane",
                "capture_since", "snapshot_pane", "search_panes", "find_pane_by_position",
                "get_tmux_variables", "show_option", "show_environment", "show_hooks"
            ],
            self_bounded = true,
            always_load = false,
        )
    )]
    pub async fn call_read_tools_batch(
        &self,
        Parameters(ReadBatchArgs {
            operations,
            continue_on_error,
        }): Parameters<ReadBatchArgs>,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<Json<BatchResult>, ErrorData> {
        if operations.is_empty() || operations.len() > 32 {
            return Err(bad_input(
                "operations must contain 1 through 32 items".to_owned(),
            ));
        }
        let allowed: BTreeSet<_> = self
            .capability_report
            .tools
            .iter()
            .find(|row| row.name == "call_read_tools_batch")
            .into_iter()
            .flat_map(|row| row.capability.nested_authority.iter().map(String::as_str))
            .collect();
        let mut results = Vec::with_capacity(operations.len());
        for (index, operation) in operations.into_iter().enumerate() {
            let tool = operation.tool;
            if !allowed.contains(tool.as_str()) {
                results.push(BatchItem {
                    index,
                    error: Some(format!("{tool} is not an enabled inspect tool")),
                    tool,
                    success: false,
                    result: None,
                });
                if !continue_on_error {
                    break;
                }
                continue;
            }
            let request =
                CallToolRequestParams::new(tool.clone()).with_arguments(operation.arguments);
            match ServerHandler::call_tool(self, request, context.clone()).await {
                Ok(rmcp::model::CallToolResponse::Complete(result)) => {
                    let failed = result.is_error == Some(true);
                    results.push(BatchItem {
                        index,
                        tool,
                        success: !failed,
                        result: serde_json::to_value(result).ok(),
                        error: None,
                    });
                    if failed && !continue_on_error {
                        break;
                    }
                }
                Ok(_) => {
                    results.push(BatchItem {
                        index,
                        tool,
                        success: false,
                        result: None,
                        error: Some("nested tool did not complete synchronously".to_owned()),
                    });
                    if !continue_on_error {
                        break;
                    }
                }
                Err(error) => {
                    results.push(BatchItem {
                        index,
                        tool,
                        success: false,
                        result: None,
                        error: Some(error.message.into_owned()),
                    });
                    if !continue_on_error {
                        break;
                    }
                }
            }
        }
        let succeeded = results.iter().filter(|item| item.success).count();
        Ok(Json(BatchResult {
            failed: results.len() - succeeded,
            succeeded,
            results,
        }))
    }
}
