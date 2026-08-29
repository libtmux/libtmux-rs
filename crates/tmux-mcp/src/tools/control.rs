use super::super::{
    Asking, ChannelArgs, ChannelSignal, Command, CreateSessionArgs, EnvironmentSet, ErrorData,
    Json, Killed, Layout, NewSessionOptions, NewWindowArgs, OptionArgs, OptionScope, OptionSet,
    PaneArgs, PaneChanged, PaneSize, PaneView, Parameters, PasteTextArgs, Pasted, PipePaneArgs,
    Piped, RenameArgs, Renamed, ResizeDirection, ResizePaneArgs, RespawnPaneArgs, SelectLayoutArgs,
    SelectPaneArgs, SelectWindowArgs, SendKeysArgs, Sent, ServerKilled, SessionArgs, SessionView,
    SetEnvironmentArgs, Size, SplitDirection, SplitOptions, SplitPaneArgs, TmuxTools, WindowArgs,
    WindowView, Windows, bad_input, lossy, object_gone, tmux_error, tool, tool_router, vanished,
};

#[tool_router(router = control_router, vis = "pub(super)")]
impl TmuxTools {
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
        asking: Asking,
    ) -> Result<Json<Killed>, ErrorData> {
        let window = self.find_window(&window).await?;
        let id = window.id().to_string();
        self.permitted(&asking, &format!("window {id}")).await?;
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
        asking: Asking,
    ) -> Result<Json<Killed>, ErrorData> {
        let pane = self.find_pane(&pane).await?;
        let id = pane.id().to_string();
        self.permitted(&asking, &format!("pane {id}")).await?;
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
        asking: Asking,
    ) -> Result<Json<Killed>, ErrorData> {
        let target = self.find_session(&session).await?;
        let id = target.id().to_string();
        self.permitted(&asking, &format!("session {id}")).await?;
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
    pub async fn kill_server(&self, asking: Asking) -> Result<Json<ServerKilled>, ErrorData> {
        // Nothing on this server survives, so the caller's pane need not be
        // looked up: being here at all is disqualifying.
        if let Some(own) = self.own_pane().await {
            return Err(Self::self_harm("server", own));
        }
        self.permitted(&asking, "this tmux server and every session on it")
            .await?;
        // `Server::shutdown` closes this crate's own subprocess executor and
        // leaves the daemon running, which is the opposite of what this tool
        // promises.
        self.server
            .cmd(Command::new("kill-server"))
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(ServerKilled { killed: true }))
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
}
