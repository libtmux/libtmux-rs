use std::ffi::OsString;
use std::sync::atomic::{AtomicU64, Ordering};

use libtmux::{Command, CommandChain, Error, NewSessionOptions, ResizeDirection};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ErrorData;
use rmcp::{tool, tool_router};

use crate::{
    ChannelArgs, ChannelSignal, CreateSessionArgs, Killed, Layout, PaneArgs, PaneChanged, PaneView,
    PasteTextArgs, Pasted, ResizePaneArgs, SelectLayoutArgs, SelectPaneArgs, SelectWindowArgs,
    SendKeysArgs, Sent, SessionArgs, SessionView, Size, TmuxTools, WindowArgs, Windows,
};

use super::error::{EffectBoundary, bad_input, tmux_error, vanished};
use super::lossy;
use super::pane_input::{MissingSource, PaneInputReach, active_run_error};

/// Numbers temporary paste buffers so concurrent calls cannot share one.
static PASTE_BUFFER_COUNTER: AtomicU64 = AtomicU64::new(0);

fn literal_input(pane: &str, text: String) -> Command {
    Command::new("send-keys")
        .arg("-t")
        .arg(pane)
        .arg("-l")
        .arg("--")
        .sensitive_arg(OsString::from(text))
}

fn named_input(pane: &str, keys: Vec<String>, enter: bool) -> Command {
    let mut command = Command::new("send-keys").arg("-t").arg(pane).arg("--");
    for key in keys {
        command = command.arg(key);
    }
    if enter {
        command = command.arg("Enter");
    }
    command
}

fn input_dispatch(
    pane: &str,
    text: Option<String>,
    keys: Vec<String>,
    enter: bool,
) -> Option<CommandChain> {
    let mut commands = Vec::with_capacity(2);
    if let Some(text) = text {
        commands.push(literal_input(pane, text));
    }
    if !keys.is_empty() || enter {
        commands.push(named_input(pane, keys, enter));
    }
    let mut commands = commands.into_iter();
    let first = commands.next()?;
    Some(commands.fold(CommandChain::new(first), CommandChain::then))
}

async fn delete_private_paste_buffer(server: &libtmux::Server, name: &str) -> Result<(), Error> {
    if server.buffer(name).await?.is_none() {
        return Ok(());
    }
    server.delete_buffer(name).await
}

fn cleanup_after_refusal(primary: ErrorData, cleanup: Result<(), Error>) -> ErrorData {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => {
            let mut boundary = EffectBoundary::new("paste_text");
            boundary.mark();
            boundary.local(format!(
                "{}; temporary paste buffer cleanup failed: {cleanup}",
                primary.message
            ))
        }
    }
}

impl TmuxTools {
    /// Refuse to destroy a window that currently contains the caller pane.
    pub(super) async fn protect_window_caller(
        &self,
        window: &libtmux::Window,
    ) -> Result<(), ErrorData> {
        let Some(own) = self.protected_pane().await? else {
            return Ok(());
        };
        let panes = window.panes().await.map_err(|e| tmux_error(&e))?;
        if panes.iter().any(|pane| pane.id().to_string() == own) {
            return Err(Self::self_harm("window", own));
        }
        Ok(())
    }

    /// Refuse to destroy a session that currently contains the caller pane.
    async fn protect_session_caller(&self, session: &libtmux::Session) -> Result<(), ErrorData> {
        let Some(own) = self.protected_pane().await? else {
            return Ok(());
        };
        let panes = session.panes().await.map_err(|e| tmux_error(&e))?;
        if panes.iter().any(|pane| pane.id().to_string() == own) {
            return Err(Self::self_harm("session", own));
        }
        Ok(())
    }

    pub(crate) async fn send_keys_one(
        &self,
        SendKeysArgs {
            pane,
            text,
            keys,
            enter,
        }: SendKeysArgs,
    ) -> Result<Json<Sent>, ErrorData> {
        let keys = keys.unwrap_or_default();
        if text.is_none() && keys.is_empty() && !enter {
            return Err(bad_input("send_keys needs text, keys, or enter".to_owned()));
        }

        let initial = self
            .preflight_pane_input(
                &pane,
                PaneInputReach::Synchronized,
                MissingSource::CallerInput,
            )
            .await?;
        let reservation = initial
            .reserve()
            .ok_or_else(|| active_run_error(initial.target.id().as_ref()))?;
        let plan = self
            .preflight_reserved_pane_input(
                &pane,
                PaneInputReach::Synchronized,
                MissingSource::CallerInput,
                &reservation,
            )
            .await?;
        if !initial.same_authority(&plan) || !plan.owns(&reservation) {
            return Err(bad_input(format!(
                "pane {pane} changed its configured input authority before send dispatch"
            )));
        }
        let dispatch = input_dispatch(plan.target.id().as_ref(), text, keys, enter)
            .ok_or_else(|| bad_input("send_keys needs text, keys, or enter".to_owned()))?;
        let mut boundary = EffectBoundary::new("send_keys");
        if dispatch.command_count() > 1 {
            boundary.mark();
        }
        let result = self
            .server
            .chain(dispatch)
            .await
            .map_err(|error| boundary.error(error))?;
        if let Some(error) = result.refusal_for("send-keys") {
            return Err(boundary.error(error));
        }

        Ok(Json(Sent {
            pane: plan.target.id().to_string(),
            panes: plan.configured,
        }))
    }
}

#[tool_router(router = control_router, vis = "pub(super)")]
impl TmuxTools {
    /// Kill one window.
    #[tool(
        description = "Kill a window, closing it in every session that links it",
        title = "Kill Window",
        meta = crate::capability_meta!(Teardown, None, [Delete], [TmuxMetadata], true, true, {
            "window" => [TmuxLookup]
        })
    )]
    pub async fn kill_window(
        &self,
        Parameters(WindowArgs { window }): Parameters<WindowArgs>,
    ) -> Result<Json<Killed>, ErrorData> {
        let window = self.find_window(&window).await?;
        let id = window.id().to_string();
        self.protect_window_caller(&window).await?;
        window.kill().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Killed { id }))
    }

    /// Kill one pane.
    #[tool(
        description = "Kill a pane. Killing a window's last pane closes the window",
        title = "Kill Pane",
        meta = crate::capability_meta!(Teardown, None, [Delete], [TmuxMetadata], true, true, {
            "pane" => [TmuxLookup]
        })
    )]
    pub async fn kill_pane(
        &self,
        Parameters(PaneArgs { pane }): Parameters<PaneArgs>,
    ) -> Result<Json<Killed>, ErrorData> {
        let pane = self.find_pane(&pane).await?;
        let id = pane.id().to_string();
        if self.protected_pane().await? == Some(id.as_str()) {
            return Err(Self::self_harm("pane", &id));
        }
        pane.kill().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Killed { id }))
    }

    /// Create a detached session.
    #[tool(
        description = "Create a new detached tmux session",
        title = "Create Session",
        meta = crate::capability_meta!(
            Execute, ConfiguredProcess,
            effects = [Change],
            outputs = [TmuxMetadata],
            secrets = true,
            untrusted = true,
            sinks = {
                "name" => [TmuxState, TmuxFormat],
                "start_directory" => [TmuxState, TmuxFormat]
            },
            literalized = ["name", "start_directory"],
            nested = [],
            self_bounded = false,
            always_load = false,
        )
    )]
    pub async fn create_session(
        &self,
        Parameters(CreateSessionArgs {
            name,
            start_directory,
        }): Parameters<CreateSessionArgs>,
    ) -> Result<Json<SessionView>, ErrorData> {
        let mut options = NewSessionOptions::new(libtmux::escape_format(name));
        if let Some(directory) = start_directory {
            options = options.start_directory(libtmux::escape_format(directory));
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
        meta = crate::capability_meta!(Teardown, None, [Delete], [TmuxMetadata], true, true, {
            "session" => [TmuxLookup]
        })
    )]
    pub async fn kill_session(
        &self,
        Parameters(SessionArgs { session }): Parameters<SessionArgs>,
    ) -> Result<Json<Killed>, ErrorData> {
        let target = self.find_session(&session).await?;
        let id = target.id().to_string();
        self.protect_session_caller(&target).await?;
        target.kill().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(Killed { id }))
    }

    /// Move one edge of a pane.
    #[tool(
        description = "Move one edge of a pane by a number of rows or columns",
        title = "Resize Pane",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "pane" => [TmuxLookup],
            "direction" => [TmuxState],
            "cells" => [TmuxState]
        })
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
                       interpreted. Text, keys, and optional Enter keep that order in one tmux \
                       dispatch. Before input, the configured synchronized-pane cohort is \
                       observed; a dead, \
                       input-disabled, mode-owned, terminal-attended, or inherited-caller member \
                       refuses the whole call. Returned pane IDs describe configured membership, \
                       not confirmed delivery. The \
                       observation can race with tmux processing the input.",
        title = "Send Keys To Pane",
        meta = crate::capability_meta!(Execute, PaneInput, [Change], [TmuxMetadata], true, true, {
            "pane" => [TmuxLookup],
            "text" => [PaneInput],
            "keys" => [PaneInput],
            "enter" => [PaneInput]
        })
    )]
    pub async fn send_keys(
        &self,
        Parameters(args): Parameters<SendKeysArgs>,
    ) -> Result<Json<Sent>, ErrorData> {
        self.send_keys_one(args).await
    }

    /// Move focus to a pane, or to the one beside it.
    #[tool(
        description = "Select a pane, making it its window's active pane. Give a direction to \
                       move relative to it instead: up, down, left, and right follow the \
                       layout, last returns to the previously active pane, and next and \
                       previous step through the window in order.",
        title = "Select Pane",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "pane" => [TmuxLookup],
            "direction" => [TmuxState]
        })
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
                let step = if matches!(direction.as_deref(), Some("next")) {
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
                let mut boundary = EffectBoundary::new("select_pane");
                let flag = match other {
                    None | Some("next" | "previous") => None,
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
                let result = boundary.tmux(self.server.cmd(command).await)?;
                if let Some(error) = result.refusal_for("select-pane") {
                    return Err(boundary.error(error));
                }
                boundary.mark();

                // Which pane that landed on is tmux's answer, not ours.
                let window = boundary
                    .tmux(self.server.window_by_id(target.window_id()).await)?
                    .ok_or_else(|| boundary.local("the pane's window is gone"))?;
                boundary
                    .tmux(window.active_pane().await)?
                    .ok_or_else(|| boundary.local("the window reported no active pane"))?
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
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "window" => [TmuxLookup],
            "direction" => [TmuxState]
        })
    )]
    pub async fn select_window(
        &self,
        Parameters(SelectWindowArgs { window, direction }): Parameters<SelectWindowArgs>,
    ) -> Result<Json<Windows>, ErrorData> {
        let mut target = self.find_window(&window).await?;
        let mut boundary = EffectBoundary::new("select_window");

        match direction.as_deref() {
            None => {
                boundary.tmux(target.select().await)?;
                boundary.mark();
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
                    boundary.tmux(target.select().await)?;
                    boundary.mark();
                }
                let result = boundary.tmux(
                    self.server
                        .cmd(
                            Command::new("select-window")
                                .arg(flag)
                                .arg("-t")
                                .arg(target.session_id().to_string()),
                        )
                        .await,
                )?;
                if let Some(error) = result.refusal_for("select-window") {
                    return Err(boundary.error(error));
                }
                boundary.mark();
            }
        }

        // Which window that landed on is tmux's answer, not ours.
        let session = boundary
            .tmux(self.server.session_by_id(target.session_id()).await)?
            .ok_or_else(|| boundary.local("the window's session is gone"))?;
        let active = boundary
            .tmux(session.active_window().await)?
            .ok_or_else(|| boundary.local("the session reported no active window"))?;

        Ok(Json(Self::render_windows(&[active])))
    }

    /// Arrange a window's panes.
    #[tool(
        description = "Rearrange a window's panes into a named layout, or into a layout \
                       string tmux gave you earlier. Use even-horizontal, even-vertical, \
                       main-horizontal, main-vertical or tiled.",
        title = "Arrange Window Panes",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "window" => [TmuxLookup],
            "layout" => [TmuxState]
        })
    )]
    pub async fn select_layout(
        &self,
        Parameters(SelectLayoutArgs { window, layout }): Parameters<SelectLayoutArgs>,
    ) -> Result<Json<Layout>, ErrorData> {
        let target = self.find_window(&window).await?;
        let result = self
            .server
            .cmd(
                Command::new("select-layout")
                    .arg("-t")
                    .arg(target.id().to_string())
                    // `select-layout` has flags of its own, and a layout is
                    // the caller's text. Without the separator, asking for
                    // `-E` spread the panes evenly and reported `-E` back as
                    // the layout that had been applied.
                    .arg("--")
                    .arg(layout.clone()),
            )
            .await
            .map_err(|e| tmux_error(&e))?;
        if let Some(error) = result.refusal_for("select-layout") {
            return Err(tmux_error(&error));
        }

        Ok(Json(Layout {
            window: target.id().to_string(),
            layout,
        }))
    }

    /// Empty a pane's scrollback.
    #[tool(
        name = "clear_pane_scrollback",
        description = "Discard a pane's scrollback, so the next capture_pane returns only \
                       what happens next. Use this before running something whose output you \
                       want to read cleanly: it is far cheaper than reading past the old \
                       output every time. The visible screen is left alone.",
        title = "Clear Pane History",
        meta = crate::capability_meta!(Teardown, None, [Delete], [TmuxMetadata], true, true, {
            "pane" => [TmuxLookup]
        })
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

    /// Deliver text to a pane without typing it.
    #[tool(
        description = "Put text into a pane through a tmux paste buffer instead of typing it \
                       key by key. Use this for anything long or awkward: send_keys types the \
                       text, so a shell reading it can react to each character, and a \
                       bracketed-paste aware program treats a paste as one block. Optional \
                       Enter is appended to that same block. Empty text without Enter is a \
                       guarded buffer-free no-op. Paste targets only the named pane, even when \
                       synchronized input is enabled. A dead, input-disabled, mode-owned, \
                       terminal-attended, or inherited-caller target is refused before setup \
                       and again immediately before paste. The private buffer is deleted after \
                       setup, refusal, and paste outcomes; observations can still race with tmux.",
        title = "Paste Text Into Pane",
        meta = crate::capability_meta!(Execute, PaneInput, [Change], [TmuxMetadata], true, true, {
            "pane" => [TmuxLookup],
            "text" => [PaneInput],
            "enter" => [PaneInput]
        })
    )]
    pub async fn paste_text(
        &self,
        Parameters(PasteTextArgs { pane, text, enter }): Parameters<PasteTextArgs>,
    ) -> Result<Json<Pasted>, ErrorData> {
        let initial = self
            .preflight_pane_input(
                &pane,
                PaneInputReach::TargetOnly,
                MissingSource::CallerInput,
            )
            .await?;
        let bytes = text.len();
        if text.is_empty() && !enter {
            return Ok(Json(Pasted {
                pane: initial.target.id().to_string(),
                bytes,
            }));
        }
        let mut payload = text;
        if enter {
            payload.push('\n');
        }
        let buffer = format!(
            "tmux-mcp-{}-{}",
            std::process::id(),
            PASTE_BUFFER_COUNTER.fetch_add(1, Ordering::Relaxed)
        );

        if let Err(error) = self
            .server
            .set_buffer(Some(&buffer), OsString::from(payload))
            .await
        {
            let primary = tmux_error(&error);
            return Err(cleanup_after_refusal(
                primary,
                delete_private_paste_buffer(&self.server, &buffer).await,
            ));
        }
        let Some(reservation) = initial.reserve() else {
            return Err(cleanup_after_refusal(
                active_run_error(initial.target.id().as_ref()),
                delete_private_paste_buffer(&self.server, &buffer).await,
            ));
        };
        let target = match self
            .preflight_reserved_pane_input(
                &pane,
                PaneInputReach::TargetOnly,
                MissingSource::PasteTransition,
                &reservation,
            )
            .await
        {
            Ok(plan) if initial.same_authority(&plan) && plan.owns(&reservation) => plan.target,
            Ok(_) => {
                return Err(cleanup_after_refusal(
                    bad_input(format!(
                        "pane {pane} changed its configured input authority before paste dispatch"
                    )),
                    delete_private_paste_buffer(&self.server, &buffer).await,
                ));
            }
            Err(primary) => {
                return Err(cleanup_after_refusal(
                    primary,
                    delete_private_paste_buffer(&self.server, &buffer).await,
                ));
            }
        };
        let pasted = target.paste_buffer(Some(&buffer)).await;
        let deleted = delete_private_paste_buffer(&self.server, &buffer).await;
        paste_outcome(pasted, deleted).map_err(|error| tmux_error(&error))?;

        Ok(Json(Pasted {
            pane: target.id().to_string(),
            bytes,
        }))
    }

    /// Signal a `wait-for` channel.
    #[tool(
        description = "Signal a tmux wait-for channel, releasing every current waiter. With \
                       no waiter, one signal is latched; signalling the same channel again \
                       clears that latch.",
        title = "Signal Channel",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "channel" => [TmuxState],
            "seconds" => [None]
        })
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
}

fn paste_outcome(pasted: Result<(), Error>, deleted: Result<(), Error>) -> Result<(), Error> {
    match deleted {
        Ok(()) => pasted,
        Err(cleanup) => {
            let error = match pasted {
                Ok(()) => cleanup,
                Err(paste) => paste,
            };
            Err(error.after_effect("paste_text"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use libtmux::test::TestServer;
    use libtmux::{Command, Error, ErrorKind, Server, ServerGoneKind};
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::model::ErrorCode;

    use crate::{SendKeysArgs, TmuxTools};

    use super::paste_outcome;

    fn cleanup_error() -> Error {
        Error::ServerGone {
            command: "delete-buffer",
            kind: ServerGoneKind::NotRunning,
        }
    }

    #[test]
    fn paste_cleanup_decides_whether_replay_is_safe() {
        assert!(paste_outcome(Ok(()), Ok(())).is_ok());

        let cleanup = paste_outcome(Ok(()), Err(cleanup_error()))
            .expect_err("a leaked buffer follows a completed paste");
        assert_eq!(cleanup.kind(), ErrorKind::PartialEffect);

        let paste = paste_outcome(Err(Error::RuntimeNested), Ok(()))
            .expect_err("successful cleanup restores the paste error");
        assert_eq!(paste.kind(), ErrorKind::InvalidInput);

        let both = paste_outcome(Err(Error::RuntimeNested), Err(cleanup_error()))
            .expect_err("failed cleanup leaves the setup effect behind");
        assert!(
            matches!(
                both,
                Error::AfterEffect { source, .. }
                    if source.kind() == ErrorKind::InvalidInput
            ),
            "the paste failure remains the source",
        );
    }

    #[tokio::test]
    async fn a_later_send_keys_failure_reports_the_first_effect() {
        let guard = TestServer::builder().start().await.expect("tmux starts");
        let session = guard
            .server()
            .new_session("send-boundary")
            .await
            .expect("a session starts");
        session
            .set_hook(
                "after-send-keys",
                "if-shell -F '#{?hook_flag_l,0,1}' \
                 'wait-for -S retry-send-held; wait-for retry-send-release'",
            )
            .await
            .expect("the Enter reply is held");
        let pane = session
            .panes()
            .await
            .expect("panes are listed")
            .into_iter()
            .next()
            .expect("the session has a pane");
        let bounded = Server::builder()
            .socket_path(guard.socket_path())
            .config_file(guard.server().config_file().expect("the fixture config"))
            .tmux_executable(guard.server().tmux_executable())
            .default_timeout(Duration::from_secs(2))
            .build()
            .expect("a bounded handle");
        let tools = TmuxTools::builder(bounded.clone()).caller(None).build();

        let result = tools
            .send_keys(Parameters(SendKeysArgs {
                pane: pane.id().to_string(),
                text: Some(String::from("printf retry-boundary")),
                keys: None,
                enter: true,
            }))
            .await;
        let held = guard
            .server()
            .wait_for_channel("retry-send-held", Duration::from_secs(2))
            .await
            .expect("the hook channel is readable");
        guard
            .server()
            .cmd(Command::new("wait-for").arg("-S").arg("retry-send-release"))
            .await
            .expect("the hook is released");
        drop(tools);
        bounded.shutdown().await.expect("the bounded handle stops");
        guard.shutdown().await.expect("tmux fixture shuts down");

        assert_eq!(held, libtmux::ChannelWait::Signalled);
        let Err(error) = result else {
            panic!("Enter reached tmux but its held reply did not fail");
        };
        assert_eq!(error.code, ErrorCode::INTERNAL_ERROR);
        let detail = error.data.expect("the error carries detail");
        assert_eq!(detail["kind"], "partial_effect", "{detail}");
        assert_eq!(detail["retryable"], false, "{detail}");
        assert_eq!(detail["stale"], false, "{detail}");
    }
}
