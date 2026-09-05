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
                       runs in a subshell, so cd and export do not persist. \
                       Reaching the deadline or cancelling this request stops the waiting, not \
                       the command; inspect the pane before sending more input.",
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
        let target = self.find_pane(&pane).await?;
        // A pane mode routes input to tmux bindings instead of the workload.
        // The attached client owns the transition back to ordinary input.
        if target.is_in_mode() {
            return Err(bad_input(format!(
                "pane {pane} is in a tmux mode, where input invokes mode bindings rather \
                     than reaching the shell. Read with capture_pane or snapshot_pane and \
                     wait for the attached person to leave the mode before sending input."
            )));
        }
        let view = reporting(
            reporter,
            "still running",
            run_request::run(
                &target,
                &command,
                Self::budget(seconds),
                suppress_history,
                &cancelled,
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
    use super::*;

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
