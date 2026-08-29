//! Pane output streaming, capture, and bounded polling waits.

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use crate::Error;
use crate::formats::TmuxText;

use super::{CaptureOptions, CapturedLine, POLL_INTERVAL, Pane, PaneWait, contains};

impl Pane {
    /// Watch what this pane writes, as it writes it.
    ///
    /// [`Pane::capture`] reads what is on screen now; this reports every byte
    /// the pane produces from here on, including what scrolls away. It opens a
    /// control-mode connection to the session holding the pane and keeps it,
    /// so there is no polling and no sampling interval to get wrong. Where the
    /// pane lives is resolved here rather than taken from this handle.
    ///
    /// The bytes are the pane's own, terminal escapes included. tmux reports
    /// them in whatever sized chunks it has, so a caller wanting lines has to
    /// buffer.
    ///
    /// tmux discards what a pane has buffered when the pane exits, so a
    /// command that writes and returns immediately may be reported as nothing
    /// at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the pane no longer exists, or an
    /// error when the control-mode connection cannot be opened.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn watch(pane: &libtmux::Pane) -> Result<(), libtmux::Error> {
    /// let mut output = pane.stream_output().await?;
    ///
    /// while let Some(chunk) = output.next_chunk().await {
    ///     println!("{} bytes", chunk.len());
    /// }
    ///
    /// output.shutdown().await
    /// # }
    /// ```
    #[cfg(feature = "control-mode")]
    pub async fn stream_output(&self) -> Result<crate::control::PaneOutput, Error> {
        // tmux reports a pane only to a client attached to a session that
        // links its window, so attaching through this handle's cached session
        // would deliver silence after the pane was joined elsewhere.
        let window = self.window().await?.ok_or_else(|| Error::ObjectGone {
            kind: crate::ObjectKind::Pane,
            id: self.id().to_string(),
        })?;
        let server = crate::Server::from_core(Arc::clone(&self.core));
        let (sender, events) = crate::control::ControlMode::attach(&server, window.session_id())
            .await?
            .split();

        // One session can hold many panes, and the connection carries all of
        // them, so narrowing happens before the caller reads.
        sender.watch_only(std::slice::from_ref(self.id())).await?;

        Ok(crate::control::PaneOutput::new(
            self.id().clone(),
            events,
            sender,
        ))
    }

    /// Capture the pane's visible contents, one entry per line.
    ///
    /// Lines are [`TmuxText`] because a terminal's contents are arbitrary
    /// bytes, not guaranteed UTF-8.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn capture(&self) -> Result<Vec<TmuxText>, Error> {
        self.capture_with(CaptureOptions::visible()).await
    }

    /// Read the pane's contents, choosing how much and in what form.
    ///
    /// [`Pane::capture`] reads the visible screen. This reaches into
    /// scrollback, which is where the output a caller is looking for has
    /// usually gone by the time anyone asks.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the capture, which includes a pane
    /// that has been closed.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::CaptureOptions;
    ///
    /// let session = server.new_session("captured").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// let visible = pane.capture_with(CaptureOptions::visible()).await?;
    /// let everything = pane.capture_with(CaptureOptions::history()).await?;
    /// assert!(everything.len() >= visible.len());
    ///
    /// let last_ten = pane.capture_with(CaptureOptions::visible().start(-10)).await?;
    /// assert!(last_ten.len() >= visible.len());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capture_with(&self, options: CaptureOptions) -> Result<Vec<TmuxText>, Error> {
        // `-T` arrived in 3.4; every other flag this lowers is present at the
        // supported floor. Refusing here beats dispatching a flag tmux will
        // reject with a usage error that names the whole command.
        if options.trims_blank_cells() {
            crate::Server::from_core(Arc::clone(&self.core))
                .require(
                    "capture-pane -T",
                    crate::version::since::CAPTURE_TRIM_BLANK_CELLS,
                )
                .await?;
        }

        let command = options.into_command(self.id().as_ref());
        let target = command.target().map(OsStr::to_os_string);
        let result = self.core.execute(command).await?;
        if !result.success() {
            return Err(Error::from_refused_result(
                "capture-pane",
                &result,
                target.as_deref(),
            ));
        }

        // tmux terminates every line, including the last, so a trailing empty
        // element after the final newline is framing rather than content.
        let stdout = result.stdout();
        let stdout = stdout.strip_suffix(b"\n").unwrap_or(stdout);
        if stdout.is_empty() {
            return Ok(Vec::new());
        }

        Ok(stdout
            .split(|byte| *byte == b'\n')
            .map(|line| TmuxText::from(line.to_vec()))
            .collect())
    }

    /// Capture with the per-line flags tmux records, marking shell prompts.
    ///
    /// tmux records where a prompt and its output begin from the OSC 133
    /// sequences a shell emits, and keeps those marks as lines scroll into
    /// history. That is what answers "show me the last command's output"
    /// exactly, rather than by guessing at a prompt's shape.
    ///
    /// Every [`CapturedLine::starts_prompt`] is `false` when the pane's shell
    /// does not emit OSC 133, which is the common case: fish does, bash and
    /// zsh do not without shell integration installed. An empty result is a
    /// real answer about the shell, not a failure.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.7, which is where
    /// `capture-pane -F` arrived, and an error when tmux refuses the capture.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::CaptureOptions;
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    /// let session = server.new_session("prompts").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// if server.capabilities().await?.tmux_version().meets(&libtmux::since::CAPTURE_LINE_FLAGS) {
    ///     let lines = pane.capture_lines(CaptureOptions::history()).await?;
    ///     // Without shell integration nothing is marked, which is an answer.
    ///     let prompts = lines.iter().filter(|line| line.starts_prompt).count();
    ///     assert!(prompts <= lines.len());
    /// }
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capture_lines(&self, options: CaptureOptions) -> Result<Vec<CapturedLine>, Error> {
        crate::Server::from_core(Arc::clone(&self.core))
            .require("capture-pane -F", crate::version::since::CAPTURE_LINE_FLAGS)
            .await?;

        Ok(self
            .capture_with(options.line_flags())
            .await?
            .into_iter()
            .map(|row| CapturedLine::parse(&row))
            .collect())
    }

    /// Wait until this pane's output contains `needle`.
    ///
    /// Polls rather than streams, so it needs no feature: a caller who
    /// dispatches a command needs to know when it finished, and
    /// [`Pane::send_keys`] without that is half an operation. A control-mode
    /// subscription would answer sooner and is not available in a default
    /// build.
    ///
    /// Each look reads the scrollback rather than the visible screen, and
    /// joins lines tmux wrapped. Both matter for correctness rather than
    /// completeness: text that scrolled off before the look would otherwise
    /// be missed and reported as absent, and a line wider than the pane
    /// arrives split, so a needle spanning the wrap point would never match.
    ///
    /// A pane whose process ends answers [`PaneWait::Dead`] rather than
    /// running to the deadline, because waiting longer cannot change it.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses a capture.
    /// Running out of time is [`PaneWait::TimedOut`], not an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::PaneWait;
    /// use std::time::Duration;
    ///
    /// # let guard = libtmux::test::TestServer::builder().start().await?;
    /// # let session = guard.server().new_session("waiting").await?;
    /// # let pane = session.panes().await?.remove(0);
    /// pane.send_line("printf 'ready\\n'").await?;
    ///
    /// let seen = pane.wait_for_text("ready", Duration::from_secs(10)).await?;
    /// assert_eq!(seen, PaneWait::Arrived);
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_text(
        &self,
        needle: impl AsRef<[u8]>,
        within: Duration,
    ) -> Result<PaneWait, Error> {
        let needle = needle.as_ref();
        self.wait_until(within, |text, _| contains(text, needle))
            .await
    }

    /// Wait until this pane stops producing output for `quiet_for`.
    ///
    /// For work that prints nothing recognisable at its end. Quiet is measured
    /// from the last change this saw, so it cannot be shorter than the polling
    /// interval; a caller wanting a specific string should say so with
    /// [`Pane::wait_for_text`], which answers as soon as it appears rather
    /// than after a silence.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses a capture.
    /// Running out of time is [`PaneWait::TimedOut`], not an error.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::PaneWait;
    /// use std::time::Duration;
    ///
    /// # let guard = libtmux::test::TestServer::builder().start().await?;
    /// # let session = guard.server().new_session("settling").await?;
    /// # let pane = session.panes().await?.remove(0);
    /// let settled = pane
    ///     .wait_for_quiet(Duration::from_millis(300), Duration::from_secs(10))
    ///     .await?;
    /// assert_eq!(settled, PaneWait::Arrived);
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn wait_for_quiet(
        &self,
        quiet_for: Duration,
        within: Duration,
    ) -> Result<PaneWait, Error> {
        let mut last_change = tokio::time::Instant::now();
        let mut previous: Option<Vec<u8>> = None;
        self.wait_until(within, move |text, now| {
            if previous.as_deref() == Some(text) {
                return now.duration_since(last_change) >= quiet_for;
            }
            previous = Some(text.to_vec());
            last_change = now;
            false
        })
        .await
    }

    /// The shared loop: look, decide, sleep, repeat until the deadline.
    async fn wait_until(
        &self,
        within: Duration,
        mut settled: impl FnMut(&[u8], tokio::time::Instant) -> bool,
    ) -> Result<PaneWait, Error> {
        // Scrollback rather than the visible screen, and wrapped lines joined:
        // output that scrolled away would read as absent, and a needle
        // spanning a wrap point would never match.
        let options = CaptureOptions::history().join_wrapped();
        let deadline = tokio::time::Instant::now() + within;

        loop {
            let text = self
                .capture_with(options)
                .await?
                .into_iter()
                .flat_map(|line| {
                    let mut bytes = line.as_bytes().to_vec();
                    bytes.push(b'\n');
                    bytes
                })
                .collect::<Vec<u8>>();

            let now = tokio::time::Instant::now();
            // Asked before the deadline is checked, so output already there
            // when the wait began is an answer rather than a timeout.
            if settled(&text, now) {
                return Ok(PaneWait::Arrived);
            }

            // A pane whose process ended will not produce more, so holding it
            // to the deadline reports the wrong thing slowly.
            if self.refreshed().await.is_ok_and(|pane| pane.is_dead()) {
                return Ok(PaneWait::Dead);
            }

            if tokio::time::Instant::now() >= deadline {
                return Ok(PaneWait::TimedOut);
            }
            tokio::time::sleep(POLL_INTERVAL.min(within)).await;
        }
    }
}
