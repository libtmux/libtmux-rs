mod contract;
mod control;
mod error;
mod inspect;
mod observe;
mod pane_input;

use std::path::Path;
use std::time::Duration;

use libtmux::{CaptureOptions, TmuxText};
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;

use crate::caller::Relation;
use crate::{
    Capture, Marks, PaneView, Panes, SessionView, Sessions, TmuxTools, WindowView, Windows,
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
        + TmuxTools::contract_router()
        + TmuxTools::observe_router()
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
        scope: Option<&str>,
        target: Option<&str>,
    ) -> Result<OptionScope, ErrorData> {
        let needs = |what: &str| bad_input(format!("scope {what} needs a target id"));

        match scope {
            Some("server") => Ok(OptionScope::Server),
            None | Some("global-session") => Ok(OptionScope::GlobalSession),
            Some("global-window") => Ok(OptionScope::GlobalWindow),
            Some("session") => {
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
            Some("window") => Ok(OptionScope::Window(Box::new(
                self.find_window(target.ok_or_else(|| needs("window"))?)
                    .await?,
            ))),
            Some("pane") => Ok(OptionScope::Pane(Box::new(
                self.find_pane(target.ok_or_else(|| needs("pane"))?).await?,
            ))),
            Some(unknown) => Err(bad_input(format!(
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
    pub(super) fn render_panes(&self, panes: &[libtmux::Pane]) -> Panes {
        let socket = self.socket();
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

    /// The socket path this process connects through.
    ///
    /// Taken from this crate's configuration rather than from
    /// `#{socket_path}`, for the reasons `pane_input_endpoint` records: tmux
    /// stores a non-printable byte in the path as an octal escape and
    /// releases disagree about it, and reading the answer back as lossy UTF-8
    /// replaced any non-UTF-8 byte regardless of version. Both produced a
    /// path that matched nothing, which for a caller comparison means failing
    /// to recognize the caller's own pane.
    ///
    /// Resolved once. Two calls racing compute the same answer, so the loser
    /// discarding its own is harmless.
    pub(super) fn socket(&self) -> Option<&Path> {
        self.socket
            .get_or_init(|| Some(self.server.socket_path().to_path_buf()))
            .as_deref()
    }

    /// The pane protected as the inherited caller on this server.
    ///
    /// A returned pane has been resolved in the caller's claimed session on
    /// the selected daemon. Malformed or stale context refuses the operation.
    pub(super) async fn protected_pane(&self) -> Result<Option<&str>, ErrorData> {
        if self.caller.is_none() {
            return Ok(None);
        }
        let generation = self.server.generation().await.map_err(|e| tmux_error(&e))?;
        let panes = self.server.panes().await.map_err(|e| tmux_error(&e))?;
        let socket = self.socket().ok_or_else(|| {
            Self::caller_context_refusal("tmux did not report its selected socket")
        })?;
        self.server
            .require_generation(generation)
            .await
            .map_err(|e| tmux_error(&e))?;
        self.caller_pane_for_snapshot(socket, generation, &panes)
    }

    pub(super) fn caller_pane_for_snapshot<'a>(
        &'a self,
        socket: &Path,
        generation: libtmux::ServerGeneration,
        panes: &[libtmux::Pane],
    ) -> Result<Option<&'a str>, ErrorData> {
        let Some(caller) = self.caller.as_deref() else {
            return Ok(None);
        };
        caller
            .resolve_on(socket, generation, panes)
            .map_err(|detail| {
                Self::caller_context_refusal(&format!("inherited caller context is {detail}"))
            })
    }

    /// The tools this server offers after startup selection.
    #[must_use]
    pub fn offered(&self) -> Vec<rmcp::model::Tool> {
        self.tool_router.list_all()
    }

    /// The pane this process runs in, when tmux named a complete identity.
    ///
    /// Reported without checking it against the server, because this is for
    /// saying what the environment claimed rather than for deciding anything.
    #[must_use]
    pub fn caller_pane(&self) -> Option<&str> {
        self.caller.as_ref().and_then(|caller| caller.pane_id())
    }

    /// Classify a refusal that protects the pane this process talks through.
    pub(super) fn self_protection(message: String) -> ErrorData {
        ErrorData::invalid_params(
            message,
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

    fn caller_context_refusal(detail: &str) -> ErrorData {
        Self::self_protection(format!(
            "refusing this operation because {detail}; restart the MCP outside tmux or with a complete current TMUX and TMUX_PANE context"
        ))
    }

    /// Refuse a command that may destroy the pane this process talks through.
    pub(super) fn self_harm(what: &str, own: &str) -> ErrorData {
        Self::self_protection(format!(
            "refusing to kill this {what}: pane {own} matches this MCP server's inherited \
             caller context, so killing it may end this conversation. Run the command in \
             a terminal if that is what you meant."
        ))
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
