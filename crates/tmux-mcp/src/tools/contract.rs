#![allow(
    missing_docs,
    reason = "native route descriptions are the protocol documentation"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use libtmux::{PaneSize, SplitDirection, SplitOptions};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolRequestParams, ErrorData};
use rmcp::schemars;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router};
use serde::{Deserialize, Serialize};

use crate::{PaneView, SessionView, TmuxTools, WindowView};

use super::error::{bad_input, object_gone, tmux_error};
use super::lossy;

const READ_BATCH_MAX_OPERATIONS: usize = 16;
const READ_BATCH_MAX_BYTES: usize = 1_000_000;
const READ_BATCH_TRUNCATED_ERROR: &str =
    "nested tool error was truncated to fit the batch response";

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
pub struct VariablesArgs {
    #[schemars(
        length(min = 1, max = 32),
        inner(regex(pattern = "^[A-Za-z][A-Za-z0-9_]*$"))
    )]
    pub names: Vec<String>,
    pub pane: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct VariablesValue {
    pub values: BTreeMap<String, String>,
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
    #[serde(default)]
    pub(crate) on_error: OnError,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchItem {
    pub(crate) index: usize,
    pub(crate) tool: String,
    pub(crate) success: bool,
    pub(crate) result: Option<serde_json::Value>,
    pub(crate) result_truncated: bool,
    pub(crate) error: Option<ErrorData>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BatchResult {
    pub(crate) results: Vec<BatchItem>,
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
    pub(crate) stopped_at: Option<usize>,
    pub(crate) truncated: bool,
    pub(crate) truncated_bytes: usize,
    pub(crate) on_error: OnError,
}

impl BatchResult {
    fn complete(results: Vec<BatchItem>, on_error: OnError, stopped_at: Option<usize>) -> Self {
        Self::from_results(results, on_error, stopped_at, false, 0)
    }

    fn from_results(
        results: Vec<BatchItem>,
        on_error: OnError,
        stopped_at: Option<usize>,
        truncated: bool,
        truncated_bytes: usize,
    ) -> Self {
        let succeeded = results.iter().filter(|item| item.success).count();
        Self {
            failed: results.len() - succeeded,
            succeeded,
            results,
            stopped_at,
            truncated,
            truncated_bytes,
            on_error,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchResultRef<'a> {
    results: &'a [BatchItem],
    succeeded: usize,
    failed: usize,
    stopped_at: Option<usize>,
    truncated: bool,
    truncated_bytes: usize,
    on_error: OnError,
}

#[derive(Default)]
struct ByteCounter(usize);

impl io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ReadBatchAccumulator {
    results: Vec<BatchItem>,
    truncated: bool,
    truncated_bytes: usize,
    stopped_at: Option<usize>,
    on_error: OnError,
}

impl Default for ReadBatchAccumulator {
    fn default() -> Self {
        Self::new(OnError::Stop)
    }
}

impl ReadBatchAccumulator {
    const fn new(on_error: OnError) -> Self {
        Self {
            results: Vec::new(),
            truncated: false,
            truncated_bytes: 0,
            stopped_at: None,
            on_error,
        }
    }

    fn push(&mut self, item: BatchItem) -> bool {
        self.results.push(item);
        self.fit()
    }

    fn fit(&mut self) -> bool {
        while self.response_bytes() > READ_BATCH_MAX_BYTES {
            if let Some(item) = self.results.iter_mut().find(|item| item.result.is_some()) {
                let removed = item
                    .result
                    .as_ref()
                    .and_then(|result| serde_json::to_vec(result).ok())
                    .map_or(0, |encoded| encoded.len());
                item.result = None;
                item.result_truncated = true;
                self.truncated_bytes = self.truncated_bytes.saturating_add(removed);
                self.truncated = true;
                continue;
            }
            if let Some(item) = self.results.iter_mut().find(|item| {
                item.error
                    .as_ref()
                    .is_some_and(|error| error.message != READ_BATCH_TRUNCATED_ERROR)
            }) {
                let Some(error) = item.error.as_mut() else {
                    return false;
                };
                let before = serde_json::to_vec(error).map_or(0, |encoded| encoded.len());
                *error = ErrorData::new(error.code, READ_BATCH_TRUNCATED_ERROR, None);
                let after = serde_json::to_vec(error).map_or(0, |encoded| encoded.len());
                item.result_truncated = true;
                self.truncated_bytes = self
                    .truncated_bytes
                    .saturating_add(before.saturating_sub(after));
                self.truncated = true;
                continue;
            }
            return false;
        }
        true
    }

    fn response_bytes(&self) -> usize {
        let succeeded = self.results.iter().filter(|item| item.success).count();
        let view = BatchResultRef {
            failed: self.results.len() - succeeded,
            succeeded,
            results: &self.results,
            stopped_at: self.stopped_at,
            truncated: self.truncated,
            truncated_bytes: self.truncated_bytes,
            on_error: self.on_error,
        };
        let Ok(value) = serde_json::to_value(&view) else {
            return usize::MAX;
        };
        let response = rmcp::model::CallToolResult::structured(value);
        let mut bytes = ByteCounter::default();
        serde_json::to_writer(&mut bytes, &response).map_or(usize::MAX, |()| bytes.0)
    }

    fn stop_at(&mut self, index: usize) {
        self.stopped_at = Some(index);
        let _ = self.fit();
    }

    fn finish(self) -> BatchResult {
        BatchResult::from_results(
            self.results,
            self.on_error,
            self.stopped_at,
            self.truncated,
            self.truncated_bytes,
        )
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OnError {
    #[default]
    Stop,
    Continue,
}

impl OnError {
    const fn stops(self) -> bool {
        matches!(self, Self::Stop)
    }
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
    #[schemars(length(min = 1, max = 16))]
    pub(crate) operations: Vec<ReadOperation>,
    #[serde(default)]
    pub(crate) on_error: OnError,
}

fn is_variable_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[tool_router(router = contract_router, vis = "pub(super)")]
impl TmuxTools {
    #[tool(
        description = "Return metadata for one session",
        title = "Get Session Info",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {
            "session" => [TmuxLookup]
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
            "window" => [TmuxLookup]
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
            "pane" => [TmuxLookup]
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
            "window" => [TmuxLookup],
            "corner" => [TmuxLookup]
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
            sinks = {"names" => [TmuxFormat], "pane" => [TmuxLookup]},
            literalized = [],
            format_validated = ["names"],
            nested = [],
            self_bounded = false,
            always_load = false,
        )
    )]
    pub async fn get_tmux_variables(
        &self,
        Parameters(VariablesArgs { names, pane }): Parameters<VariablesArgs>,
    ) -> Result<Json<VariablesValue>, ErrorData> {
        if !(1..=32).contains(&names.len()) {
            return Err(bad_input(
                "names must contain between one and 32 tmux variables".to_owned(),
            ));
        }
        let pane = match pane {
            Some(target) => Some(self.find_pane(&target).await?),
            None => None,
        };
        let mut values = BTreeMap::new();
        for name in names {
            if !is_variable_name(&name) {
                return Err(bad_input(format!(
                    "{name:?} is not a tmux variable name; use letters, digits, and underscores"
                )));
            }
            let format = format!("#{{{name}}}");
            let value = self
                .server
                .format(pane.as_ref(), &format)
                .await
                .map_err(|error| tmux_error(&error))?;
            values.insert(name, lossy(&value));
        }
        Ok(Json(VariablesValue { values }))
    }

    #[tool(
        description = "Rename one session",
        title = "Rename Session",
        meta = crate::capability_meta!(
            Manage, None,
            effects = [Change], outputs = [TmuxMetadata], secrets = true, untrusted = true,
            sinks = {"session" => [TmuxLookup], "name" => [TmuxState, TmuxFormat]},
            literalized = ["name"], nested = [], self_bounded = false,
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
            sinks = {"window" => [TmuxLookup], "name" => [TmuxState, TmuxFormat]},
            literalized = ["name"], nested = [], self_bounded = false,
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
            "window" => [TmuxLookup], "width" => [TmuxState], "height" => [TmuxState]
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
            "window" => [TmuxLookup],
            "destination_session" => [TmuxLookup],
            "destination_index" => [TmuxState]
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
            "source_pane" => [TmuxLookup], "target_pane" => [TmuxLookup]
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
            sinks = {"pane" => [TmuxLookup], "title" => [TmuxState, TmuxFormat]},
            literalized = ["title"], nested = [], self_bounded = false,
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
            "pane" => [TmuxLookup]
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
            "pane" => [TmuxLookup]
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
            "session" => [TmuxLookup], "enabled" => [TmuxState]
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
            "session" => [TmuxLookup], "limit" => [TmuxState]
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
                "session" => [TmuxLookup], "name" => [TmuxState, TmuxFormat],
                "start_directory" => [TmuxState, TmuxFormat]
            },
            literalized = ["name", "start_directory"], nested = [],
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
                "pane" => [TmuxLookup], "direction" => [TmuxState], "percent" => [TmuxState],
                "start_directory" => [TmuxState, TmuxFormat]
            },
            literalized = ["start_directory"], nested = [],
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
            "pane" => [TmuxLookup], "kill_first" => [None]
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
            sinks = {"window" => [TmuxLookup], "enabled" => [TmuxState]},
            literalized = [], nested = [],
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
            "operations" => [TmuxLookup, PaneInput], "on_error" => [None]
        })
    )]
    pub async fn send_keys_batch(
        &self,
        Parameters(SendBatchArgs {
            operations,
            on_error,
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
                    result_truncated: false,
                    error: None,
                }),
                Err(error) => {
                    results.push(BatchItem {
                        index,
                        tool,
                        success: false,
                        result: None,
                        result_truncated: false,
                        error: Some(error),
                    });
                    if on_error.stops() {
                        break;
                    }
                }
            }
        }
        let stopped_at = on_error
            .stops()
            .then(|| results.last())
            .flatten()
            .filter(|item| !item.success)
            .map(|item| item.index);
        Ok(Json(BatchResult::complete(results, on_error, stopped_at)))
    }

    #[tool(
        description = "Call a serial batch of at most sixteen enabled inspect tools. One \
                       approval for this batch covers every enabled nested name; inner tools do \
                       not receive separate client approval. The full serialized outer MCP \
                       response is capped at 1,000,000 bytes; truncated payloads and omitted \
                       bytes are explicit.",
        title = "Call Read Tools Batch",
        meta = crate::capability_meta!(
            Inspect, None,
            effects = [Observe, Change],
            outputs = [TmuxMetadata, TerminalContent, ProcessEnvironment, ConfiguredCommand],
            secrets = true,
            untrusted = true,
            sinks = {"operations" => [NestedTool], "on_error" => [None]},
            literalized = [],
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
            on_error,
        }): Parameters<ReadBatchArgs>,
        context: RequestContext<rmcp::RoleServer>,
    ) -> Result<Json<BatchResult>, ErrorData> {
        if operations.is_empty() || operations.len() > READ_BATCH_MAX_OPERATIONS {
            return Err(bad_input(format!(
                "operations must contain 1 through {READ_BATCH_MAX_OPERATIONS} items"
            )));
        }
        let allowed: BTreeSet<_> = self
            .capability_report
            .tools
            .iter()
            .find(|row| row.name == "call_read_tools_batch")
            .into_iter()
            .flat_map(|row| row.capability.nested_authority.iter().map(String::as_str))
            .collect();
        let mut batch = ReadBatchAccumulator::new(on_error);
        batch.results.reserve(operations.len());
        for (index, operation) in operations.into_iter().enumerate() {
            let tool = operation.tool;
            if !allowed.contains(tool.as_str()) {
                if !batch.push(BatchItem {
                    index,
                    error: Some(bad_input(format!("{tool} is not an enabled inspect tool"))),
                    tool,
                    success: false,
                    result: None,
                    result_truncated: false,
                }) {
                    break;
                }
                if on_error.stops() {
                    batch.stop_at(index);
                    break;
                }
                continue;
            }
            let request =
                CallToolRequestParams::new(tool.clone()).with_arguments(operation.arguments);
            match self
                .nested_tool_router
                .call(ToolCallContext::new(self, request, context.clone()))
                .await
            {
                Ok(rmcp::model::CallToolResponse::Complete(result)) => {
                    let failed = result.is_error == Some(true);
                    if !batch.push(BatchItem {
                        index,
                        tool,
                        success: !failed,
                        result: serde_json::to_value(result).ok(),
                        result_truncated: false,
                        error: None,
                    }) {
                        break;
                    }
                    if failed && on_error.stops() {
                        batch.stop_at(index);
                        break;
                    }
                }
                Ok(_) => {
                    if !batch.push(BatchItem {
                        index,
                        tool,
                        success: false,
                        result: None,
                        result_truncated: false,
                        error: Some(ErrorData::internal_error(
                            "nested tool did not complete synchronously",
                            Some(serde_json::json!({
                                "kind": "internal",
                                "retryable": false,
                                "stale": false,
                            })),
                        )),
                    }) {
                        break;
                    }
                    if on_error.stops() {
                        batch.stop_at(index);
                        break;
                    }
                }
                Err(error) => {
                    if !batch.push(BatchItem {
                        index,
                        tool,
                        success: false,
                        result: None,
                        result_truncated: false,
                        error: Some(error),
                    }) {
                        break;
                    }
                    if on_error.stops() {
                        batch.stop_at(index);
                        break;
                    }
                }
            }
        }
        Ok(Json(batch.finish()))
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn read_batch_caps_the_full_outer_response_at_one_million_bytes() {
        let item = |index, text: String| BatchItem {
            index,
            tool: "capture_pane".to_owned(),
            success: true,
            result: Some(serde_json::json!({
                "structuredContent": {"text": text}
            })),
            result_truncated: false,
            error: None,
        };
        let mut batch = ReadBatchAccumulator::default();

        assert!(batch.push(item(0, "x".repeat(510_000))));
        let result = batch.finish();
        let value = serde_json::to_value(&result).expect("batch result converts to JSON");
        let envelope = rmcp::model::CallToolResult::structured(value);
        let encoded = serde_json::to_vec(&envelope).expect("outer tool result serializes");
        let report = serde_json::to_value(&result).expect("batch result converts to JSON");

        assert_eq!(report["truncated"], true);
        assert!(
            report["truncatedBytes"]
                .as_u64()
                .is_some_and(|bytes| bytes > 0)
        );
        assert_eq!(report["results"][0]["resultTruncated"], true);
        assert_eq!(report["onError"], "stop");
        assert!(report.get("truncated_bytes").is_none());
        assert!(report["results"][0].get("result_truncated").is_none());
        assert_eq!(result.results.len(), 1);
        assert!(encoded.len() <= 1_000_000, "{} bytes", encoded.len());
    }

    #[test]
    fn read_batch_preserves_every_executed_error_row_when_it_truncates() {
        let mut batch = ReadBatchAccumulator::new(OnError::Continue);

        for index in 0..READ_BATCH_MAX_OPERATIONS {
            assert!(batch.push(BatchItem {
                index,
                tool: "get_pane_info".to_owned(),
                success: false,
                result: None,
                result_truncated: false,
                error: Some(ErrorData::invalid_params("x".repeat(70_000), None)),
            }));
        }

        let result = batch.finish();
        let envelope = rmcp::model::CallToolResult::structured(
            serde_json::to_value(&result).expect("batch result converts to JSON"),
        );
        let encoded = serde_json::to_vec(&envelope).expect("outer tool result serializes");

        assert_eq!(result.results.len(), READ_BATCH_MAX_OPERATIONS);
        assert_eq!(result.failed, READ_BATCH_MAX_OPERATIONS);
        assert_eq!(result.succeeded, 0);
        assert!(result.truncated);
        assert!(result.truncated_bytes > 0);
        assert!(result.results.iter().all(|item| item.error.is_some()));
        assert_eq!(
            result
                .results
                .iter()
                .map(|item| item.index)
                .collect::<Vec<_>>(),
            (0..READ_BATCH_MAX_OPERATIONS).collect::<Vec<_>>()
        );
        assert!(encoded.len() <= 1_000_000, "{} bytes", encoded.len());
    }
}
