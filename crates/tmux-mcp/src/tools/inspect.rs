use std::path::{Path, PathBuf};

use libtmux::query::QueryIteratorExt as _;
use libtmux::{CaptureOptions, Command, Server};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ErrorData;
use rmcp::{tool, tool_router};

use crate::exec::Patterns;
use crate::{
    Branch, BranchPane, BranchWindow, Busy, Capture, CapturePaneArgs, Changes, Environment,
    EnvironmentEntry, FilterArgs, FormatArgs, Formatted, Hook, Hooks, Marks, MatchView, Matches,
    OptionArgs, OptionValue, Panes, SearchPanesArgs, ServerListing, ServerListings, SessionArgs,
    SessionView, Sessions, ShowEnvironmentArgs, ShowHooksArgs, Snapshot, SnapshotArgs, TmuxTools,
    Tree, TreeFilterArgs, WhatChangedArgs, WindowArgs, Windows,
};

use super::error::{bad_input, tmux_error};
use super::{OptionScope, lossy, lossy_optional};

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

#[tool_router(router = inspect_router, vis = "pub(super)")]
impl TmuxTools {
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
        // The typed protocol boundary rejects unknown versions, fields, and
        // operators before this route runs.
        let expression = filter;

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
        let expression = filter;

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
                       tmux uses when a command names no target. tmux expands the option \
                       name as a format, so #(command) can start a shell command.",
        title = "Read tmux Option",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
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
        let scope = self
            .option_scope(scope.as_deref(), target.as_deref())
            .await?;
        let value = match scope {
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

    /// Find tmux servers in the known socket locations.
    #[tool(
        description = "Find tmux servers in the standard per-user socket directory and beside \
                       the selected socket. Custom sockets elsewhere and other users' servers \
                       are not included. These tools are bound to one server for their whole \
                       life; acting on another means starting a server pointed at its socket. \
                       A pane id means nothing across servers: %1 names a different pane on each.",
        title = "List tmux Servers",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn list_servers(&self) -> Result<Json<ServerListings>, ErrorData> {
        let bound = self.socket().await.map(Path::to_path_buf);

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

        // A `stat` per entry, on a directory whose size belongs to whoever
        // else uses this machine: a shared /tmp held 836 sockets while this
        // was written, which is close to two milliseconds with the cache warm
        // and unbounded without it. That is work for a blocking thread rather
        // than for the one driving every other request. One handoff covers the
        // whole scan, where `tokio::fs` would take one per entry.
        let (scanned, listed) = tokio::task::spawn_blocking(move || {
            let mut searched = Vec::new();
            let mut found: Vec<PathBuf> = Vec::new();
            for root in roots {
                searched.push(root.display().to_string());
                let Ok(entries) = std::fs::read_dir(&root) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if std::fs::metadata(&path).is_ok_and(|meta| {
                        std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type())
                    }) {
                        found.push(path);
                    }
                }
            }
            // Sorted to deduplicate, because two roots can name one socket and
            // asking `contains` per entry compares every path against every
            // path kept so far.
            found.sort();
            found.dedup();
            (searched, found)
        })
        .await
        .map_err(|error| {
            ErrorData::internal_error(format!("the socket scan did not finish: {error}"), None)
        })?;
        let searched = scanned;
        let mut found = listed;

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

    /// Report which windows have produced output.
    #[tool(
        description = "Report which windows have written output, most recent first, and a \
                       timestamp to pass back next time. This is the cheap way to re-orient \
                       on a busy machine: one call instead of capturing every pane to find \
                       out which one is doing something. It answers at window granularity, \
                       so follow up with list_window_panes and capture_since. tmux stamps \
                       this on every byte a pane writes, so it needs no tmux options set.",
        title = "Report What Has Been Busy",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn what_changed(
        &self,
        Parameters(WhatChangedArgs { since }): Parameters<WhatChangedArgs>,
    ) -> Result<Json<Changes>, ErrorData> {
        let windows = self.server.windows().await.map_err(|e| tmux_error(&e))?;
        let checked = windows.len();

        let mut busy: Vec<_> = windows
            .iter()
            .filter(|window| since.is_none_or(|since| window.last_activity() > since))
            .map(|window| Busy {
                id: window.id().to_string(),
                session_id: window.session_id().to_string(),
                name: lossy(window.name()),
                activity: window.last_activity(),
                panes: window.pane_count(),
                active: window.is_active(),
            })
            .collect();
        busy.sort_by_key(|window| std::cmp::Reverse(window.activity));

        // The latest activity seen, rather than a clock reading. Comparing a
        // caller's value against timestamps tmux wrote means both have to come
        // from tmux, and this needs no second source to agree with.
        let now = windows
            .iter()
            .map(libtmux::Window::last_activity)
            .max()
            .unwrap_or_default()
            .max(since.unwrap_or_default());

        Ok(Json(Changes {
            windows: busy,
            now,
            windows_checked: checked,
        }))
    }

    /// Expand a tmux format string.
    #[tool(
        description = "Expand a tmux format such as #{pane_unseen_changes} or \
                       #{window_activity_flag} and return what it evaluates to. This reaches \
                       every field tmux publishes, including ones no tool here has of its \
                       own, so use it for questions the listings cannot answer. Name a pane \
                       for anything pane, window or session shaped: tmux resolves the window \
                       and session from it. A literal #(command), or a value expanded \
                       recursively, can start a shell command asynchronously. Use only \
                       simple, validated #{field} lookups when reading untrusted state.",
        title = "Expand A tmux Format",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
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

    /// Read the hooks tmux runs on its own events.
    #[tool(
        description = "List the hooks tmux runs when something happens on the server, such as \
                       a pane exiting. This tool does not set hooks. Hooks set through another \
                       path remain in their server or session until unset; configuration files \
                       persist them across server restarts. Reach for this when tmux does \
                       something no tool here asked for.",
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
}
