use std::time::{Duration, Instant};

use libtmux::{CaptureOptions, Command};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ErrorData;
use rmcp::{tool, tool_router};

use crate::exec::Patterns;
use crate::{
    Branch, BranchPane, BranchWindow, Capture, CapturePaneArgs, Environment, EnvironmentEntry,
    Hook, Hooks, Marks, MatchView, Matches, OptionArgs, OptionValue, Panes, SearchPanesArgs,
    Sessions, ShowEnvironmentArgs, ShowHooksArgs, Snapshot, SnapshotArgs, TmuxTools, Tree, Windows,
};

use super::error::{bad_input, tmux_error};
use super::{OptionScope, lossy, lossy_optional};

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
const SEARCH_BYTES: usize = 1 << 20;
const SEARCH_PANES: usize = 64;
const SEARCH_LINES: usize = 8192;
const SEARCH_MATCH_TIME: Duration = Duration::from_millis(250);
const SEARCH_CAPTURE_TIME: Duration = Duration::from_secs(5);

struct SearchBudget {
    remaining_bytes: usize,
    remaining_panes: usize,
    remaining_lines: usize,
    remaining_match_time: Duration,
    remaining_capture_time: Duration,
    capped: bool,
}

impl SearchBudget {
    const fn new() -> Self {
        Self {
            remaining_bytes: SEARCH_BYTES,
            remaining_panes: SEARCH_PANES,
            remaining_lines: SEARCH_LINES,
            remaining_match_time: SEARCH_MATCH_TIME,
            remaining_capture_time: SEARCH_CAPTURE_TIME,
            capped: false,
        }
    }

    fn take(&mut self, bytes: usize) -> bool {
        if self.remaining_lines == 0 || bytes > self.remaining_bytes {
            self.capped = true;
            false
        } else {
            self.remaining_lines -= 1;
            self.remaining_bytes -= bytes;
            true
        }
    }

    fn begin_pane(&mut self) -> bool {
        if self.remaining_panes == 0 {
            self.capped = true;
            false
        } else {
            self.remaining_panes -= 1;
            true
        }
    }

    fn capture_time(&mut self) -> Option<Duration> {
        if self.remaining_capture_time.is_zero() {
            self.capped = true;
            None
        } else {
            Some(self.remaining_capture_time)
        }
    }

    fn charge_capture(&mut self, elapsed: Duration) {
        self.remaining_capture_time = self.remaining_capture_time.saturating_sub(elapsed);
    }

    fn charge_match(&mut self, elapsed: Duration) -> bool {
        if elapsed >= self.remaining_match_time {
            self.remaining_match_time = Duration::ZERO;
            self.capped = true;
            false
        } else {
            self.remaining_match_time -= elapsed;
            true
        }
    }

    fn cap(&mut self) {
        self.capped = true;
    }
}

#[tool_router(router = inspect_router, vis = "pub(super)")]
impl TmuxTools {
    /// List every session on the server.
    #[tool(
        description = "List every tmux session on the server",
        title = "List Sessions",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {})
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
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {})
    )]
    pub async fn list_windows(&self) -> Result<Json<Windows>, ErrorData> {
        let windows = self.server.windows().await.map_err(|e| tmux_error(&e))?;
        Ok(Json(Self::render_windows(&windows)))
    }

    /// List every pane on the server.
    #[tool(
        description = "List every pane on the server",
        title = "List Panes",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {}; always_load)
    )]
    pub async fn list_panes(&self) -> Result<Json<Panes>, ErrorData> {
        let panes = self.server.panes().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(self.render_panes(&panes).await))
    }

    /// Report the whole hierarchy in one call.
    #[tool(
        name = "get_server_info",
        description = "Report every session with its windows and panes, in one call. \
                       Prefer this over calling the three listing tools separately: \
                       it costs tmux three commands rather than one per object.",
        title = "Describe Server",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata], true, true, {}; always_load)
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

    /// Read one pane's contents.
    #[tool(
        description = "Read a pane's contents. Reads the visible screen by default; set history \
                       to reach output that has scrolled off, or give a start and end line. \
                       Set last_command to get only what the last command printed, which is \
                       usually what you want and is far shorter -- it needs tmux 3.7 and a \
                       shell that marks its prompts, and says so when it cannot.",
        title = "Read Pane Contents",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata, TerminalContent], true, true, {
            "pane" => [TmuxLookup],
            "history" => [None],
            "last_command" => [None],
            "start" => [TmuxState],
            "end" => [TmuxState]
        })
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

    /// Report pane state and bounded content in one answer.
    #[tool(
        description = "Read pane content with cursor position, mode state, and scroll \
                       position in one reply. The state query and capture are separate, \
                       so the result is not atomic. Prefer this over capture_pane when \
                       you need to reason about where the pane is rather than only what \
                       it says -- a cursor at column zero on a fresh line is a shell \
                       waiting, and a pane in a mode may route keys to tmux instead of \
                       the workload.",
        title = "Snapshot Pane State",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata, TerminalContent], true, true, {
            "pane" => [TmuxLookup],
            "max_lines" => [None],
            "history" => [None]
        }; always_load)
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
        description = "Search what panes are displaying with Rust's linear-time regex engine. \
                       Accept at most 4,096 pattern bytes; search at most 64 panes, 8,192 lines, \
                       and 1 MiB; spend at most 250 ms matching and five seconds capturing. \
                       Report the pane and line of \
                       every match. Use this to find where something is -- which pane has \
                       the failing test, which one printed the error -- instead of capturing \
                       panes one at a time. Searches the visible screen by default; set \
                       history to include scrollback.",
        title = "Search Pane Contents",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata, TerminalContent], true, true, {
            "pattern" => [Regex],
            "regex" => [None],
            "match_case" => [None],
            "history" => [None],
            "session" => [TmuxLookup],
            "window" => [TmuxLookup]
        })
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
        let mut budget = SearchBudget::new();
        let mut panes_searched = 0;
        'panes: for pane in &panes {
            if !budget.begin_pane() {
                break;
            }
            let Some(capture_time) = budget.capture_time() else {
                break;
            };
            // A pane that cannot be read is not a reason to abandon the
            // search: it is usually one that closed while this ran.
            let capture_started = Instant::now();
            let captured = tokio::time::timeout(capture_time, pane.capture_with(options)).await;
            budget.charge_capture(capture_started.elapsed());
            let lines = match captured {
                Ok(Ok(lines)) => lines,
                Ok(Err(_)) => continue,
                Err(_) => {
                    budget.cap();
                    break;
                }
            };
            panes_searched += 1;
            if found.len() >= SEARCH_MATCHES {
                // Reading the remaining panes could not change the answer, and
                // each one costs a capture.
                budget.capped = true;
                break;
            }
            for (number, line) in lines.iter().enumerate() {
                if found.len() >= SEARCH_MATCHES {
                    budget.capped = true;
                    break 'panes;
                }
                if !budget.take(line.as_bytes().len()) {
                    break 'panes;
                }
                let match_started = Instant::now();
                let matched = patterns.first_match(line.as_bytes()).is_some();
                if !budget.charge_match(match_started.elapsed()) {
                    break 'panes;
                }
                if matched {
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
            capped: budget.capped || found.len() >= SEARCH_MATCHES,
            matches: found,
            panes_searched,
        }))
    }

    /// Read a tmux option.
    #[tool(
        description = "Read a tmux option, such as history-limit or a user option like \
                       @theme. Name the scope the option lives in; global-session is what \
                       tmux uses when a command names no target.",
        title = "Read tmux Option",
        meta = crate::capability_meta!(
            Inspect, None,
            effects = [Observe],
            outputs = [TmuxMetadata, ConfiguredCommand],
            secrets = true,
            untrusted = true,
            sinks = {
                "name" => [TmuxLookup, TmuxFormat],
                "scope" => [None],
                "target" => [TmuxLookup]
            },
            literalized = ["name"],
            nested = [],
            self_bounded = false,
            always_load = false,
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
        let literal_name = libtmux::escape_format(&name).to_string_lossy().into_owned();
        let value = match scope {
            OptionScope::Server => self.server.get_option(&literal_name).await,
            OptionScope::GlobalSession => self.server.get_global_option(&literal_name).await,
            OptionScope::GlobalWindow => self.server.get_global_window_option(&literal_name).await,
            OptionScope::Session(session) => session.get_option(&literal_name).await,
            OptionScope::Window(window) => window.get_option(&literal_name).await,
            OptionScope::Pane(pane) => pane.get_option(&literal_name).await,
        }
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(OptionValue {
            name,
            // Absent and empty are different answers: tmux reports no value
            // for an option that has never been set at that scope.
            value: value.as_ref().map(lossy),
        }))
    }

    /// Read a tmux environment.
    #[tool(
        description = "Read the environment tmux hands to processes it starts, for the server \
                       or for one session. This is not the environment of anything already \
                       running: a pane started before a change keeps what it was given.",
        title = "Show tmux Environment",
        meta = crate::capability_meta!(Inspect, None, [Observe], [ProcessEnvironment], true, true, {
            "session" => [TmuxLookup]
        })
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
        meta = crate::capability_meta!(Inspect, None, [Observe], [ConfiguredCommand], true, true, {
            "session" => [TmuxLookup]
        })
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

#[cfg(test)]
mod tests {
    use super::SearchBudget;

    #[test]
    fn search_budget_refuses_bytes_beyond_the_limit() {
        let mut budget = SearchBudget::new();
        budget.remaining_bytes = 3;

        assert!(budget.take(2));
        assert!(!budget.take(2));
        assert!(budget.capped);
    }

    #[test]
    fn search_budget_counts_zero_byte_lines_as_work() {
        let mut budget = SearchBudget::new();

        for _ in 0..8192 {
            assert!(budget.take(0));
        }
        assert!(!budget.take(0));
        assert!(budget.capped);
    }
}
