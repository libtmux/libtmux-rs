mod control;
mod error;
mod inspect;
mod observe;
mod plan;

use std::path::{Path, PathBuf};
use std::time::Duration;

use libtmux::{CaptureOptions, Command, TmuxText};
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;

use crate::caller::Relation;
use crate::{
    Capture, Marks, OptionScopeName, PaneView, Panes, ServerView, SessionView, Sessions, TmuxTools,
    WindowView, Windows, resources,
};

use error::{bad_input, object_gone, tmux_error};

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

pub(super) fn router() -> rmcp::handler::server::router::tool::ToolRouter<TmuxTools> {
    TmuxTools::inspect_router()
        + TmuxTools::control_router()
        + TmuxTools::observe_router()
        + TmuxTools::plan_router()
}

impl TmuxTools {
    /// Describe sessions, shared by the tool and the `tmux://` resource so the
    /// two cannot drift into different accounts of the same session.
    pub(super) fn render_sessions(sessions: &[libtmux::Session]) -> Sessions {
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

    /// Read what the last command in a pane printed.
    ///
    /// tmux records where a prompt and its output begin from the OSC 133
    /// sequences a shell emits. Where those marks exist this is exact; where
    /// they do not, the whole screen comes back with `marks` saying why, so a
    /// caller reads a field rather than guessing from a suspiciously long
    /// answer.
    pub(super) async fn capture_last_command(
        &self,
        pane: &str,
    ) -> Result<Json<Capture>, ErrorData> {
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
                // Falling back to the history would answer a request for one
                // command's output with everything the pane ever printed,
                // which is the most expensive answer available. The visible
                // screen is the bounded approximation.
                None => (
                    target
                        .capture_with(CaptureOptions::visible())
                        .await
                        .map_err(|e| tmux_error(&e))?
                        .iter()
                        .map(|line| line.to_string_lossy().into_owned())
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

    /// Render one window as the protocol sees it.
    pub(super) fn one_window(window: &libtmux::Window) -> WindowView {
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
    pub(super) fn render_windows(windows: &[libtmux::Window]) -> Windows {
        let windows: Vec<_> = windows.iter().map(Self::one_window).collect();

        Windows { windows }
    }

    /// Resolve the object an option belongs to.
    async fn option_scope(
        &self,
        scope: Option<&OptionScopeName>,
        target: Option<&str>,
    ) -> Result<OptionScope, ErrorData> {
        let needs = |what: &str| bad_input(format!("scope {what} needs a target id"));

        match scope {
            Some(OptionScopeName::Server) => Ok(OptionScope::Server),
            None | Some(OptionScopeName::GlobalSession) => Ok(OptionScope::GlobalSession),
            Some(OptionScopeName::GlobalWindow) => Ok(OptionScope::GlobalWindow),
            Some(OptionScopeName::Session) => {
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
            Some(OptionScopeName::Window) => Ok(OptionScope::Window(Box::new(
                self.find_window(target.ok_or_else(|| needs("window"))?)
                    .await?,
            ))),
            Some(OptionScopeName::Pane) => Ok(OptionScope::Pane(Box::new(
                self.find_pane(target.ok_or_else(|| needs("pane"))?).await?,
            ))),
            Some(OptionScopeName::Other(unknown)) => Err(bad_input(format!(
                "scope must be server, global-session, global-window, session, window, \
                     or pane, not {unknown}"
            ))),
        }
    }

    /// How long a blocking tool may hold the caller's turn.
    ///
    /// An MCP call blocks the agent that made it, so an unbounded wait costs a
    /// whole turn with nothing to show. The ceiling is generous enough for a
    /// slow build and short enough that a wedged wait is an annoyance.
    pub(super) fn budget(seconds: Option<u64>) -> Duration {
        Duration::from_secs(seconds.unwrap_or(30).clamp(1, 600))
    }

    /// Render panes as the protocol sees them, saying which one is our own.
    pub(super) async fn render_panes(&self, panes: &[libtmux::Pane]) -> Panes {
        let socket = self.socket().await;
        let panes: Vec<_> = panes
            .iter()
            .map(|pane| self.pane_view(pane, socket))
            .collect();

        Panes { panes }
    }

    /// Describe one pane, including where it stands relative to this process.
    pub(super) fn pane_view(&self, pane: &libtmux::Pane, socket: Option<&Path>) -> PaneView {
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
    pub(super) async fn socket(&self) -> Option<&Path> {
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

    /// The pane protected as the inherited caller on this server.
    ///
    /// This errs toward protection when socket evidence is incomplete, so a
    /// returned pane is not necessarily a confirmed location.
    pub(super) async fn protected_pane(&self) -> Option<&str> {
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

    /// Refuse a command that may destroy the pane this process talks through.
    pub(super) fn self_harm(what: &str, own: &str) -> ErrorData {
        ErrorData::invalid_params(
            format!(
                "refusing to kill this {what}: pane {own} matches this MCP server's inherited \
                 caller context, so killing it may end this conversation. Run the command in \
                 a terminal if that is what you meant."
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
    pub(super) async fn read_target(
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
                        inherited_caller_pane: self.caller_pane().map(ToOwned::to_owned),
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
    pub(super) async fn find_window(&self, id: &str) -> Result<libtmux::Window, ErrorData> {
        self.server
            .windows()
            .await
            .map_err(|e| tmux_error(&e))?
            .into_iter()
            .find(|window| window.id().to_string() == id)
            .ok_or_else(|| object_gone("window", id))
    }

    /// Resolve a pane id, reporting an unknown one as invalid input.
    pub(super) async fn find_pane(&self, id: &str) -> Result<libtmux::Pane, ErrorData> {
        self.server
            .panes()
            .await
            .map_err(|e| tmux_error(&e))?
            .into_iter()
            .find(|pane| pane.id().to_string() == id)
            .ok_or_else(|| object_gone("pane", id))
    }

    /// Resolve a session by name, reporting an unknown one as invalid input.
    pub(super) async fn find_session(&self, name: &str) -> Result<libtmux::Session, ErrorData> {
        self.server
            .sessions()
            .await
            .map_err(|e| tmux_error(&e))?
            .into_iter()
            .find(|session| session.name() == name.as_bytes())
            .ok_or_else(|| object_gone("session", name))
    }
}
