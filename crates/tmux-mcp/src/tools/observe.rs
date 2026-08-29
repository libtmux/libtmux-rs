use super::super::{
    CancelJobArgs, CaptureOptions, CaptureSinceArgs, ChannelArgs, ChannelWait, Cursor, Duration,
    ErrorData, IdleView, JobCancelled, JobList, JobStatusArgs, Json, Parameters, Patterns,
    Reporter, RunCommandArgs, RunView, Since, StartCommandArgs, TmuxTools, WATCH_BYTES,
    WaitForIdleArgs, WaitForTextArgs, WaitView, Watch, WatchPaneArgs, bad_input, exec, jobs,
    tmux_error, tool, tool_router, unknown_job,
};
use crate::policy::reporting;

#[tool_router(router = observe_router, vis = "pub(super)")]
impl TmuxTools {
    /// Watch a pane produce output, without polling.
    #[tool(
        description = "Watch a pane and report everything it writes for a bounded time. \
                       Unlike capture_pane this misses nothing, including output that \
                       scrolls past, but it blocks for the requested duration",
        title = "Watch Pane Bytes",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn watch_pane(
        &self,
        Parameters(WatchPaneArgs {
            pane,
            seconds,
            max_bytes,
        }): Parameters<WatchPaneArgs>,
    ) -> Result<Json<Watch>, ErrorData> {
        // An agent that asks for an hour gets a minute: this call holds a
        // connection open and blocks its own response until it returns.
        let window = Duration::from_secs(seconds.clamp(1, 60));
        let budget = max_bytes.unwrap_or(WATCH_BYTES).clamp(1, WATCH_BYTES);

        let pane = self.find_pane(&pane).await?;
        let mut output = pane.stream_output().await.map_err(|e| tmux_error(&e))?;

        let mut collected = Vec::new();
        let mut stopped = "deadline";
        let deadline = tokio::time::Instant::now() + window;

        while collected.len() < budget {
            match tokio::time::timeout_at(deadline, output.next_chunk()).await {
                Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
                // The pane stopped writing for good, which is worth saying:
                // it is the difference between a busy pane and a dead one.
                Ok(None) => {
                    stopped = "pane closed";
                    break;
                }
                Err(_) => break,
            }
        }
        if collected.len() >= budget {
            collected.truncate(budget);
            stopped = "byte limit";
        }

        let view = Watch {
            pane: output.pane().to_string(),
            bytes: collected.len(),
            // A pane emits whatever bytes it likes, and JSON carries text.
            output: String::from_utf8_lossy(&collected).into_owned(),
            stopped: stopped.to_owned(),
        };

        output.shutdown().await.map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Run a command in a pane and report how it went.
    #[tool(
        description = "Run a shell command in a pane, wait for it to finish, and report its \
                       exit status with everything it wrote. This is the tool for \"run this \
                       and tell me if it worked\". Output is read from the pane's live stream, \
                       so nothing is missed and the shell prompt is not included. The command \
                       runs in a subshell, so cd and export do not persist. \
                       Reaching the deadline stops the waiting, not the command: the pane is \
                       still busy afterwards, and another run there reports no_shell until it \
                       finishes. Send C-c with send_keys to stop it.",
        title = "Run Command In Pane",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
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
        // A pane in copy mode does not pass keys to the shell, so the command
        // would be read as navigation and the wait would run to its deadline
        // with nothing to show for it.
        if target.is_in_mode() {
            return Err(bad_input(format!(
                "pane {pane} is in copy mode, where keys move the cursor rather than \
                     reaching the shell. Leave it first."
            )));
        }
        let view = reporting(
            reporter,
            "still running",
            exec::run_command(
                &target,
                &command,
                Self::budget(seconds),
                suppress_history,
                &cancelled,
            ),
        )
        .await
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Start a command without waiting for it.
    #[tool(
        description = "Start a shell command in a pane and return at once with a job id, \
                       instead of holding this call until it finishes. Use this for anything \
                       slow -- a build, a test suite, a deploy -- and for running several at \
                       once: the answer is collected whether or not you are waiting for it. \
                       Poll with job_status, which returns only what is new. Prefer \
                       run_command when the command is quick and you want its answer now.",
        title = "Start Command In Background",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    pub async fn start_command(
        &self,
        Parameters(StartCommandArgs {
            pane,
            command,
            suppress_history,
        }): Parameters<StartCommandArgs>,
    ) -> Result<Json<jobs::JobView>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        // A pane in copy mode does not pass keys to the shell, so the command
        // would be read as navigation and the job would never start.
        if target.is_in_mode() {
            return Err(bad_input(format!(
                "pane {pane} is in copy mode, where keys move the cursor rather than \
                     reaching the shell. Leave it first."
            )));
        }

        let view = self
            .jobs
            .start(&target, &command, suppress_history)
            .await
            .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Report how a background command is getting on.
    #[tool(
        description = "Report whether a job started with start_command is still running, its \
                       exit status once it is not, and what it has written since the cursor \
                       you were given last. Pass that cursor back to read only what is new. \
                       Give seconds to wait for it to finish, which returns as soon as it \
                       does rather than at the deadline.",
        title = "Check Background Command",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn job_status(
        &self,
        Parameters(JobStatusArgs {
            job,
            cursor,
            seconds,
        }): Parameters<JobStatusArgs>,
    ) -> Result<Json<jobs::JobProgress>, ErrorData> {
        if let Some(seconds) = seconds.filter(|seconds| *seconds > 0) {
            self.jobs.wait(&job, Self::budget(Some(seconds))).await;
        }

        self.jobs
            .read(&job, cursor)
            .map(Json)
            .ok_or_else(|| unknown_job(&job))
    }

    /// List the background commands this server is holding.
    #[tool(
        description = "List every job started with start_command, running and finished, \
                       newest first. A finished job is kept so its answer can still be \
                       collected, and the oldest is forgotten once too many pile up.",
        title = "List Background Commands",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn list_jobs(&self) -> Result<Json<JobList>, ErrorData> {
        Ok(Json(JobList {
            jobs: self.jobs.list(),
        }))
    }

    /// Stop a background command and forget it.
    #[tool(
        description = "Interrupt a running job with C-c and forget it. A job that has already \
                       finished is forgotten without touching its pane. This sends the \
                       interrupt to the pane the job runs in, so anything else that pane is \
                       doing is interrupted too.",
        title = "Cancel Background Command",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn cancel_job(
        &self,
        Parameters(CancelJobArgs { job }): Parameters<CancelJobArgs>,
    ) -> Result<Json<JobCancelled>, ErrorData> {
        let (pane, running) = self
            .jobs
            .running_in(&job)
            .ok_or_else(|| unknown_job(&job))?;

        if running {
            let target = self.find_pane(&pane).await?;
            target
                .send_key_names(["C-c"])
                .await
                .map_err(|e| tmux_error(&e))?;
        }
        self.jobs.forget(&job);

        Ok(Json(JobCancelled {
            job,
            pane,
            interrupted: running,
        }))
    }

    /// Wait until a pane stops writing.
    #[tool(
        description = "Wait until a pane has written nothing for a few seconds. Use this when \
                       you cannot name what success looks like: a TUI settling, an installer \
                       finishing, a prompt whose glyph you cannot predict. Prefer run_command \
                       for a command you sent yourself, and wait_for_text when you know the \
                       text to look for -- both are exact, and this one infers.",
        title = "Wait For Pane To Go Quiet",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn wait_for_idle(
        &self,
        Parameters(WaitForIdleArgs {
            pane,
            quiet_seconds,
            seconds,
        }): Parameters<WaitForIdleArgs>,
        cancelled: tokio_util::sync::CancellationToken,
        reporter: Reporter,
    ) -> Result<Json<IdleView>, ErrorData> {
        let target = self.find_pane(&pane).await?;
        // Clamped against the total, because quiet longer than the deadline
        // could never be observed and would always answer `deadline`.
        let budget = Self::budget(seconds);
        let quiet = Duration::from_secs(quiet_seconds.unwrap_or(2).max(1)).min(budget);

        let view = reporting(
            reporter,
            "still waiting for the pane to go quiet",
            exec::wait_for_idle(&target, quiet, budget, &cancelled),
        )
        .await
        .map_err(|e| tmux_error(&e))?;

        Ok(Json(view))
    }

    /// Wait until a pane writes something a caller is looking for.
    #[tool(
        description = "Wait until a pane writes matching text. Reads the pane's live output \
                       stream, so text that scrolls past between checks is still seen. Prefer \
                       run_command for commands you are sending yourself: it reports an exit \
                       status instead of guessing from output. Use this for output you did \
                       not author, such as a server logging that it is ready.",
        title = "Wait For Pane Text",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
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
                       if output was dropped, which only happens when a pane outruns the \
                       buffer.",
        title = "Read New Pane Output",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
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
            .map_err(|e| tmux_error(&e))?;

        // A tail can only report what it saw, and on the first call it has
        // seen nothing. Answering with the visible screen makes the tool
        // usable on its own rather than requiring a wasted first round trip.
        let text = if first {
            let lines = target
                .capture_with(CaptureOptions::visible())
                .await
                .map_err(|e| tmux_error(&e))?;
            lines
                .iter()
                .map(|line| line.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            since.text
        };

        Ok(Json(Since {
            pane: target.id().to_string(),
            text,
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
        description = "Block until something signals a tmux wait-for channel. Pair this with \
                       a shell command that ends in `tmux wait-for -S <channel>` to \
                       synchronise with work this server did not start.",
        title = "Wait For Channel",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
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
