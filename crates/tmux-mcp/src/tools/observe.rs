use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ErrorData;
use rmcp::{tool, tool_router};

use crate::exec::{self, Patterns};
use crate::policy::reporting;
use crate::run_request;
use crate::tail::TailError;
use crate::{
    CaptureSinceArgs, ChannelArgs, ChannelWait, Cursor, Reporter, RunCommandArgs, RunView, Since,
    TmuxTools, WaitForTextArgs, WaitView,
};

use super::error::{EffectBoundary, bad_input, tmux_error};
use super::pane_input::{MissingSource, PaneInputReach};

/// Translate a request-owned run failure at the protocol boundary.
fn run_error(error: run_request::RunError) -> ErrorData {
    match error {
        run_request::RunError::Tmux(error) => tmux_error(&error),
        run_request::RunError::DispatchUnknown(cause) => ErrorData::internal_error(
            format!(
                "tmux did not confirm whether it started the pane command: {cause}; inspect the pane \
                     before acting because the command may be running. Do not retry \
                     automatically. To interrupt, use pane-wide send_keys with keys=[\"C-c\"], \
                     which can discard unrelated queued input"
            ),
            Some(serde_json::json!({
                "kind": "dispatch_unknown",
                "retryable": false,
                "stale": false,
            })),
        ),
        run_request::RunError::Guard(error) => error,
        run_request::RunError::Frame => ErrorData::internal_error(
            "run_shell_command could not construct a secure completion frame; no pane input was sent"
                .to_owned(),
            Some(serde_json::json!({
                "kind": "unreachable",
                "retryable": false,
                "stale": false,
            })),
        ),
    }
}

fn tail_error(error: TailError) -> ErrorData {
    match error {
        TailError::Tmux(error) => tmux_error(&error),
        TailError::Snapshot { error, opened } => tail_snapshot_error(error, opened),
        TailError::SnapshotBusy { opened, limit } => {
            if opened {
                let mut boundary = EffectBoundary::new("capture_since");
                boundary.mark();
                boundary.local(
                    "the pane tail opened, but another baseline capture is already queued; inspect \
                     the pane before retrying",
                )
            } else {
                ErrorData::internal_error(
                    "another baseline capture is already queued; retry capture_since after it \
                     finishes"
                        .to_owned(),
                    Some(serde_json::json!({
                        "kind": "capacity",
                        "retryable": true,
                        "stale": false,
                        "resource": "tail_snapshot",
                        "capacity": limit,
                    })),
                )
            }
        }
        TailError::ReaderStopped { opened } => {
            if opened {
                let mut boundary = EffectBoundary::new("capture_since");
                boundary.mark();
                boundary.local(
                    "the pane tail opened, but its reader stopped before the baseline completed; \
                     inspect the pane before retrying",
                )
            } else {
                ErrorData::internal_error(
                    "the retained pane tail reader stopped; retry capture_since to replace it"
                        .to_owned(),
                    Some(serde_json::json!({
                        "kind": "unreachable",
                        "retryable": true,
                        "stale": false,
                    })),
                )
            }
        }
        TailError::OwnerUnavailable => ErrorData::internal_error(
            "capture cursor identity is unavailable".to_owned(),
            Some(serde_json::json!({
                "kind": "unreachable",
                "retryable": true,
                "stale": false,
            })),
        ),
        TailError::OpeningAtCapacity { limit } => ErrorData::internal_error(
            "another pane tail is opening; retry capture_since after it finishes".to_owned(),
            Some(serde_json::json!({
                "kind": "capacity",
                "retryable": true,
                "stale": false,
                "resource": "tail_opening",
                "capacity": limit,
            })),
        ),
    }
}

fn tail_snapshot_error(error: libtmux::Error, opened: bool) -> ErrorData {
    let mut boundary = EffectBoundary::new("capture_since");
    if opened {
        boundary.mark();
    }
    boundary.error(error)
}

#[tool_router(router = observe_router, vis = "pub(super)")]
impl TmuxTools {
    /// Run a command in a pane and report how it went.
    #[tool(
        name = "run_shell_command",
        description = "Run a shell command in a pane, wait for it to finish, and report its \
                       exit status with everything it wrote. This is the tool for \"run this \
                       and tell me if it worked\". Output is read from the pane's live stream, \
                       so nothing is missed and the shell prompt is not included. The command \
                       runs in a subshell, so cd and export do not persist and invalid syntax \
                       completes with a nonzero status. It requires one configured input \
                       recipient and observes its mode, liveness, input-off state, attended-client \
                       state, cohort, inherited-caller relation, and foreground command before \
                       watcher setup and again before dispatch. The resolved tmux executable and \
                       socket path must contain no \
                       ASCII terminal-control bytes. These checks do not lock the pane and can \
                       race with tmux processing the input. The pane must run a trusted \
                       POSIX-compatible shell whose reserved words and special builtins retain \
                       their meanings, against a trusted tmux server and configuration. Reaching \
                       the deadline or cancelling this request stops the waiting, not the \
                       command; inspect the pane before sending more input.",
        title = "Run Command In Pane",
        meta = crate::capability_meta!(Execute, PaneCommand, [Change], [TmuxMetadata, TerminalContent], true, true, {
            "pane" => [TmuxLookup],
            "command" => [PaneInput, ShellCommand],
            "seconds" => [None],
            "suppress_history" => [None]
        })
    )]
    pub async fn run_command(
        &self,
        Parameters(RunCommandArgs {
            pane,
            command,
            seconds,
            suppress_history,
        }): Parameters<RunCommandArgs>,
        cancelled: tokio_util::sync::CancellationToken,
        reporter: Reporter,
    ) -> Result<Json<RunView>, ErrorData> {
        if command.as_bytes().contains(&0) {
            return Err(bad_input("command must not contain a NUL byte".to_owned()));
        }
        let executable = self.server.resolved_tmux_executable().ok_or_else(|| {
            ErrorData::internal_error(
                "the configured tmux executable cannot be resolved from its captured launch context"
                    .to_owned(),
                Some(serde_json::json!({
                    "kind": "unreachable",
                    "retryable": false,
                    "stale": false,
                })),
            )
        })?;
        let socket = self.server.socket_path().to_path_buf();
        if !exec::route_is_terminal_safe(executable.as_os_str(), &socket) {
            return Err(run_error(run_request::RunError::Frame));
        }
        let initial = self
            .preflight_pane_input(
                &pane,
                PaneInputReach::Synchronized,
                MissingSource::CallerInput,
            )
            .await?;
        if initial.configured.len() != 1 {
            return Err(bad_input(format!(
                "pane {pane} has synchronized input enabled for {} panes; run_shell_command requires one configured recipient",
                initial.configured.len()
            )));
        }
        let foreground = initial
            .target
            .current_command()
            .cloned()
            .ok_or_else(|| bad_input(format!("pane {pane} reported no foreground command")))?;
        let checkpoint_pane = initial.target.id().to_string();
        let final_check = async {
            let final_plan = self
                .preflight_pane_input(
                    &checkpoint_pane,
                    PaneInputReach::Synchronized,
                    MissingSource::ObservedTransition,
                )
                .await?;
            if final_plan.configured.len() != 1 {
                return Err(bad_input(format!(
                    "pane {checkpoint_pane} gained synchronized recipients between run checkpoints"
                )));
            }
            let final_foreground = final_plan.target.current_command().ok_or_else(|| {
                bad_input(format!(
                    "pane {checkpoint_pane} reported no foreground command at the final checkpoint"
                ))
            })?;
            if final_foreground != &foreground {
                return Err(bad_input(format!(
                    "pane {checkpoint_pane} changed foreground command between run checkpoints"
                )));
            }
            Ok(())
        };
        let view = reporting(
            reporter,
            "still running",
            run_request::run(
                &initial.target,
                &command,
                Self::budget(seconds),
                suppress_history,
                &cancelled,
                (executable.as_os_str(), &socket),
                final_check,
            ),
        )
        .await
        .map_err(run_error)?;

        Ok(Json(view))
    }

    /// Wait until a pane writes something a caller is looking for.
    #[tool(
        description = "Wait until a pane writes matching text. Reads the pane's live output \
                       stream, so text that scrolls past between checks is still seen. Prefer \
                       run_shell_command for commands you are sending yourself: it reports an exit \
                       status instead of guessing from output. Use this for output you did \
                       not author, such as a server logging that it is ready. The live stream \
                       attaches a client while waiting, changing the session's attached-client \
                       state. Each list accepts at most 32 patterns, each at most 4,096 bytes, \
                       using Rust's linear-time regex engine.",
        title = "Wait For Pane Text",
        meta = crate::capability_meta!(
            Inspect, None,
            effects = [Observe, Change],
            outputs = [TmuxMetadata, TerminalContent],
            secrets = true,
            untrusted = true,
            sinks = {
                "pane" => [TmuxLookup],
                "patterns" => [Regex],
                "stop" => [Regex],
                "regex" => [None],
                "match_case" => [None],
                "seconds" => [None]
            },
            literalized = [],
            nested = [],
            self_bounded = true,
            always_load = false,
        )
    )]
    pub async fn wait_for_text(
        &self,
        Parameters(WaitForTextArgs {
            pane,
            patterns,
            stop,
            regex,
            match_case,
            seconds,
        }): Parameters<WaitForTextArgs>,
        cancelled: tokio_util::sync::CancellationToken,
        reporter: Reporter,
    ) -> Result<Json<WaitView>, ErrorData> {
        let compile = |sources: Vec<String>| {
            Patterns::compile(&sources, regex, match_case).map_err(|(source, reason)| {
                bad_input(format!("pattern {source} is invalid: {reason}"))
            })
        };
        let wanted = compile(patterns.unwrap_or_default())?;
        let stops = compile(stop.unwrap_or_default())?;

        let target = self.find_pane(&pane).await?;
        let view = reporting(
            reporter,
            "still watching for the pattern",
            exec::wait_for_text(&target, &wanted, &stops, Self::budget(seconds), &cancelled),
        )
        .await
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Report what a pane has written since the last look.
    #[tool(
        description = "Read what a pane wrote since the previous call. The first call, with no \
                       cursor, starts watching and returns a cursor; later calls pass it back \
                       and receive only what is new. Use this to follow a pane over several \
                       turns without re-reading the whole screen. The answer says missed=true \
                       if the cursor no longer names retained output, including when the pane \
                       outran the buffer, its live tail was evicted, or the server restarted. \
                       Starting a tail owns a retained observer until the tail is evicted or \
                       the server stops.",
        title = "Read New Pane Output",
        meta = crate::capability_meta!(Inspect, None, [Observe], [TmuxMetadata, TerminalContent], true, true, {
            "pane" => [TmuxLookup],
            "cursor" => [None]
        })
    )]
    pub async fn capture_since(
        &self,
        Parameters(CaptureSinceArgs { pane, cursor }): Parameters<CaptureSinceArgs>,
    ) -> Result<Json<Since>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        let cursor = cursor
            .as_deref()
            .map(Cursor::decode)
            .transpose()
            .map_err(|text| bad_input(format!("{text} is not a cursor this server issued")))?;
        if let Some(cursor) = &cursor
            && cursor.pane() != target.id().to_string()
        {
            return Err(bad_input(format!(
                "that cursor belongs to pane {}, not {pane}",
                cursor.pane()
            )));
        }

        let first = cursor.is_none();
        let since = self
            .tails
            .read(&target, cursor.as_ref())
            .await
            .map_err(tail_error)?;

        Ok(Json(Since {
            pane: target.id().to_string(),
            text: since.text,
            cursor: since.cursor.encode(),
            missed: since.missed,
            closed: since.closed,
            // The first answer is the screen as it stands; every later one is
            // what the pane wrote since the cursor.
            first,
        }))
    }

    /// Wait for a `wait-for` channel to be signalled.
    #[tool(
        description = "Block until something signals a tmux wait-for channel. A pending \
                       signal is consumed. Pair this with a shell command that ends in \
                       `tmux wait-for -S <channel>` to synchronise with work this server \
                       did not start.",
        title = "Wait For Channel",
        meta = crate::capability_meta!(Manage, None, [Change], [TmuxMetadata], true, true, {
            "channel" => [TmuxState],
            "seconds" => [None]
        })
    )]
    pub async fn wait_for_channel(
        &self,
        Parameters(ChannelArgs { channel, seconds }): Parameters<ChannelArgs>,
    ) -> Result<Json<ChannelWait>, ErrorData> {
        // libtmux caps this at its own command timeout and reports running
        // out of time as an outcome rather than an error, which is the shape
        // this tool wants: the budget stays a request, and a deadline stays
        // distinct from a failure to reach tmux.
        let outcome = match self
            .server
            .wait_for_channel(channel.as_str(), Self::budget(seconds))
            .await
        {
            Ok(libtmux::ChannelWait::Signalled) => "signalled",
            Ok(libtmux::ChannelWait::TimedOut) => "deadline",
            // The schema promises one of those two words. `ChannelWait` may
            // grow a third, and answering with the nearest existing label
            // would report something that did not happen.
            Ok(_) => {
                return Err(ErrorData::internal_error(
                    "tmux reported a wait outcome this server does not know".to_owned(),
                    None,
                ));
            }
            Err(error) => return Err(tmux_error(&error)),
        };

        Ok(Json(ChannelWait {
            channel,
            outcome: outcome.to_owned(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use libtmux::test::TestServer;
    use tokio_util::sync::CancellationToken;

    use super::*;

    async fn client_count(server: &libtmux::Server) -> usize {
        server.clients().await.map_or(0, |clients| clients.len())
    }

    async fn configure_dead_transition(pane: &libtmux::Pane) {
        pane.set_option("remain-on-exit", "on")
            .await
            .expect("fixture retains a dead pane");
        pane.set_hook("pane-died", "wait-for -S mcp-final-pane-died")
            .await
            .expect("dead transition is observable");
    }

    async fn caller_peer_identity(
        pane: &libtmux::Pane,
        server: &libtmux::Server,
    ) -> crate::CallerIdentity {
        let peer = pane
            .split(libtmux::SplitOptions::new(libtmux::SplitDirection::Below))
            .await
            .expect("caller peer is created");
        crate::CallerIdentity::from_values(
            Some(format!("{},1,$0", server.socket_path().display()).into()),
            Some(peer.id().to_string().into()),
        )
        .expect("caller identity is complete")
    }

    async fn transition_then_preflight(
        transition: &str,
        pane: &mut libtmux::Pane,
        server: &libtmux::Server,
        tools: &TmuxTools,
        source: &str,
        foreground: &libtmux::TmuxText,
    ) -> Result<(), ErrorData> {
        if transition == "caller" {
            server
                .window_by_id(pane.window_id())
                .await
                .expect("window lookup")
                .expect("source window exists")
                .set_option("synchronize-panes", "on")
                .await
                .expect("caller peer enters the configured cohort");
        } else {
            let command = if transition == "dead" {
                "exit 0"
            } else {
                "exec sleep 30"
            };
            let pane_id = pane.id().clone();
            pane.respawn(Some(command), true)
                .await
                .expect("pane begins its final transition");
            if transition == "dead" {
                assert_eq!(
                    server
                        .wait_for_channel("mcp-final-pane-died", Duration::from_secs(2))
                        .await
                        .expect("pane-died notification answers"),
                    libtmux::ChannelWait::Signalled
                );
            } else {
                libtmux::test::retry_until(Duration::from_secs(2), async || {
                    server
                        .pane_by_id(&pane_id)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|pane| {
                            pane.current_command()
                                .is_some_and(|value| value.as_str() == Ok("sleep"))
                        })
                })
                .await
                .expect("selected socket reports the foreground transition");
            }
        }
        let final_plan = tools
            .preflight_pane_input(
                source,
                PaneInputReach::Synchronized,
                MissingSource::ObservedTransition,
            )
            .await?;
        if transition == "foreground" && final_plan.target.current_command() != Some(foreground) {
            return Err(bad_input(format!(
                "pane {source} changed foreground command between run checkpoints"
            )));
        }
        Ok(())
    }

    #[test]
    fn an_uncertain_start_names_safe_recovery_without_an_unreachable_handle() {
        let source = libtmux::Server::builder()
            .socket_name("conflicting")
            .socket_path("/tmp/libtmux-rs-test/conflicting.sock")
            .build()
            .expect_err("two socket selectors are refused");
        let error = run_error(run_request::RunError::DispatchUnknown(Box::new(source)));
        let data = error.data.as_ref().expect("the failure carries metadata");

        assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert_eq!(data["kind"], "dispatch_unknown");
        assert_eq!(data["retryable"], false);
        assert_eq!(data["stale"], false);
        assert!(data.get("job").is_none());
        assert!(!error.message.contains("job-7"));
        assert!(error.message.contains("inspect the pane"));
        assert!(error.message.contains("Do not retry automatically"));
        assert!(error.message.contains("send_keys"));
    }

    #[tokio::test]
    async fn real_tmux_compat_final_preflight_observes_transition_before_dispatch() {
        for (transition, expected_refusal, expected_kind) in [
            ("dead", "is dead", "invalid_input"),
            ("foreground", "changed foreground command", "invalid_input"),
            ("caller", "refusing to send input", "self_protection"),
        ] {
            let guard = TestServer::builder().start().await.expect("tmux starts");
            let server = guard.server();
            let session = server
                .new_session(format!("run-final-{transition}"))
                .await
                .expect("session starts");
            let pane = session.panes().await.expect("panes list").remove(0);
            if transition == "dead" {
                configure_dead_transition(&pane).await;
            }
            let caller = if transition == "caller" {
                Some(caller_peer_identity(&pane, server).await)
            } else {
                None
            };
            let tools = TmuxTools::builder(server.clone()).caller(caller).build();
            let source = pane.id().to_string();
            let initial = tools
                .preflight_pane_input(
                    &source,
                    PaneInputReach::Synchronized,
                    MissingSource::CallerInput,
                )
                .await
                .expect("initial preflight accepts the live pane");
            let foreground = initial
                .target
                .current_command()
                .cloned()
                .expect("initial pane reports its foreground command");
            server
                .set_hook(
                    "after-display-message",
                    "set-option -g @mcp-final-display-message seen",
                )
                .await
                .expect("display transport is observable");
            let executable = server
                .resolved_tmux_executable()
                .expect("fixture tmux resolves");
            let socket = server.socket_path().to_path_buf();
            let baseline_clients = client_count(server).await;
            let mut transition_pane = pane.clone();
            let final_check = async {
                assert_eq!(
                    client_count(server).await,
                    baseline_clients + 1,
                    "{transition}: watcher is attached before the final checkpoint"
                );
                transition_then_preflight(
                    transition,
                    &mut transition_pane,
                    server,
                    &tools,
                    &source,
                    &foreground,
                )
                .await
            };
            let cancelled = CancellationToken::new();

            let result = run_request::run(
                &initial.target,
                "printf should-not-run",
                Duration::from_secs(2),
                false,
                &cancelled,
                (executable.as_os_str(), &socket),
                final_check,
            )
            .await;

            let Err(run_request::RunError::Guard(error)) = result else {
                panic!("the final preflight must reject the {transition} transition");
            };
            assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(error.message.contains(expected_refusal), "{transition}");
            assert_eq!(
                error.data.expect("final refusal is classified")["kind"],
                expected_kind,
                "{transition}"
            );
            assert_eq!(
                server
                    .get_global_option("@mcp-final-display-message")
                    .await
                    .expect("display marker is read"),
                None,
                "{transition}: no completion-record display command ran before refusal"
            );
            assert_eq!(
                client_count(server).await,
                baseline_clients,
                "{transition}: final refusal closes the output watcher"
            );
            guard.shutdown().await.expect("tmux fixture shuts down");
        }
    }

    #[test]
    fn unavailable_cursor_identity_is_an_internal_failure() {
        let error = tail_error(TailError::OwnerUnavailable);
        let data = error.data.expect("the failure is classified");

        assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert_eq!(error.message, "capture cursor identity is unavailable");
        assert_eq!(data["kind"], "unreachable");
        assert_eq!(data["retryable"], true);
        assert_eq!(data["stale"], false);
        assert_eq!(data.as_object().map(serde_json::Map::len), Some(3));
    }

    #[test]
    fn a_busy_tail_opener_is_retryable_without_a_partial_effect() {
        let error = tail_error(TailError::OpeningAtCapacity { limit: 1 });
        let data = error.data.expect("the failure is classified");

        assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert_eq!(data["kind"], "capacity");
        assert_eq!(data["retryable"], true);
        assert_eq!(data["stale"], false);
        assert_eq!(data["resource"], "tail_opening");
        assert_eq!(data["capacity"], 1);
    }

    #[test]
    fn a_failed_baseline_after_opening_reports_a_partial_effect() {
        let configuration_error = || {
            libtmux::Server::builder()
                .socket_name("conflicting")
                .socket_path("/tmp/libtmux-rs-test/conflicting.sock")
                .build()
                .expect_err("two socket selectors are refused")
        };
        let existing = tail_error(TailError::Snapshot {
            error: configuration_error(),
            opened: false,
        });
        let existing_data = existing.data.expect("the failure is classified");
        assert_eq!(existing_data["kind"], "unreachable");

        let error = tail_error(TailError::Snapshot {
            error: configuration_error(),
            opened: true,
        });
        let data = error.data.expect("the failure is classified");

        assert_eq!(error.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert_eq!(data["kind"], "partial_effect");
        assert_eq!(data["retryable"], false);
        assert_eq!(data["stale"], false);
    }
}
