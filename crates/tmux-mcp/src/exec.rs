//! Running a command in a pane, and waiting for one to say something.
//!
//! Both read the pane's output stream rather than its screen. A screen is what
//! survived rendering; the stream is everything the program wrote, in the
//! order it wrote it, including what has already scrolled away. Nothing here
//! polls, and nothing here depends on tmux still holding a line in scrollback.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use libtmux::{CaptureOptions, Error, Pane};
use regex::bytes::Regex;
use serde::Serialize;

use crate::text::{TextFilter, readable_from};

/// The most output either primitive will hold for one call.
///
/// A pane can write faster than any consumer reads, so the ceiling belongs
/// here rather than in the caller's hands. Older bytes are dropped first: the
/// end of a command's output is what says how it went.
const OUTPUT_LIMIT: usize = 256 * 1024;

/// Distinguishes one run's sentinels from another's.
///
/// Only has to be unique among the runs this process makes; a sentinel is
/// already unmistakable in the stream because it carries a real escape byte.
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How a run finished.
///
/// Split from the wait outcomes rather than shared with them: a run cannot
/// match a pattern and a wait cannot report a missing shell, and a vocabulary
/// carrying both would have an agent checking for answers that never come.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// The command ran to completion and reported its status.
    Completed,
    /// The time the caller allowed ran out.
    ///
    /// This ends the waiting, not the command. The pane keeps working, so the
    /// next thing typed at it lands in the running command rather than at a
    /// prompt.
    Deadline,
    /// The pane stopped writing for good.
    PaneClosed,
    /// The client withdrew the request while the run was still going.
    Cancelled,
    /// The pane never acknowledged the command.
    ///
    /// The keys were sent but the opening sentinel never came back. That is
    /// what a pane looks like when it is not at a shell prompt: sitting in an
    /// editor or a REPL, or still running something an earlier call left
    /// behind. The text was typed into whatever is there.
    ///
    /// The evidence is absence, so a deadline too short for the pane's shell
    /// to have echoed anything yet looks the same. Read it as "nothing came
    /// back in the time allowed" and check the pane with `snapshot_pane`
    /// before concluding it is stuck.
    NoShell,
}

/// How a wait for text finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitOutcome {
    /// A pattern matched.
    Matched,
    /// A stop pattern matched, so the wait ended early.
    Stopped,
    /// The time the caller allowed ran out.
    Deadline,
    /// The pane stopped writing for good.
    PaneClosed,
    /// The client withdrew the request while the wait was still running.
    Cancelled,
}

/// How a wait for quiet finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdleOutcome {
    /// The pane went quiet for as long as the caller asked.
    Idle,
    /// The time the caller allowed ran out with the pane still writing.
    Deadline,
    /// The pane stopped writing for good.
    PaneClosed,
    /// The client withdrew the request while the wait was still running.
    Cancelled,
}

/// What a command did.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RunView {
    /// The pane the command ran in.
    pub pane: String,
    /// How the run finished.
    pub outcome: RunOutcome,
    /// The command's exit status, when it completed.
    ///
    /// Absent when the run did not complete, and when the command was killed
    /// by a signal rather than exiting.
    pub exit_status: Option<i32>,
    /// Everything the command wrote, stdout and stderr interleaved in the
    /// order the program wrote them.
    pub output: String,
    /// How many bytes that was, before any truncation.
    pub bytes: usize,
    /// Whether the output was truncated from the front.
    pub truncated: bool,
}

/// What a pane said while it was watched for a pattern.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct WaitView {
    /// The pane that was watched.
    pub pane: String,
    /// How the wait finished.
    pub outcome: WaitOutcome,
    /// Which pattern matched, indexed into the list it came from.
    pub matched_index: Option<usize>,
    /// The pattern that matched, as it was given.
    pub matched_pattern: Option<String>,
    /// Whether a success pattern was already on screen when the wait began.
    ///
    /// A wait only sees what a pane writes after it starts, so a pattern
    /// already present will not match. This says so, rather than leaving the
    /// caller to wait out the deadline wondering.
    pub present_at_entry: bool,
    /// What the pane wrote, with escape sequences removed.
    pub text: String,
    /// How many bytes arrived, before filtering or truncation.
    pub bytes: usize,
}

/// What a pane did while it was watched for quiet.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct IdleView {
    /// The pane that was watched.
    pub pane: String,
    /// How the wait finished.
    pub outcome: IdleOutcome,
    /// How long the pane was quiet for, in seconds.
    ///
    /// Equal to what was asked for when the outcome is `idle`, and how long
    /// the last gap was when the deadline arrived first.
    pub quiet_seconds: u64,
    /// What the pane wrote while waiting, with escape sequences removed.
    pub text: String,
    /// How many bytes arrived, before filtering or truncation.
    pub bytes: usize,
}

/// A set of patterns to look for in a pane's output.
pub(crate) struct Patterns {
    compiled: Vec<Regex>,
    sources: Vec<String>,
}

impl Patterns {
    /// Compile patterns, as literal text or as regular expressions.
    ///
    /// # Errors
    ///
    /// Returns the offending pattern and the reason when one will not compile.
    pub(crate) fn compile(
        sources: &[String],
        regex: bool,
        match_case: bool,
    ) -> Result<Self, (String, String)> {
        let mut compiled = Vec::with_capacity(sources.len());
        for source in sources {
            let body = if regex {
                source.clone()
            } else {
                regex::escape(source)
            };
            let expression = if match_case {
                body
            } else {
                format!("(?i){body}")
            };
            match Regex::new(&expression) {
                Ok(pattern) => compiled.push(pattern),
                Err(error) => return Err((source.clone(), error.to_string())),
            }
        }

        Ok(Self {
            compiled,
            sources: sources.to_vec(),
        })
    }

    /// Whether any pattern was given.
    fn is_empty(&self) -> bool {
        self.compiled.is_empty()
    }

    /// The first pattern that matches, with the index it was given at.
    pub(crate) fn first_match(&self, haystack: &[u8]) -> Option<(usize, &str)> {
        self.compiled
            .iter()
            .position(|pattern| pattern.is_match(haystack))
            .map(|index| (index, self.sources[index].as_str()))
    }
}

/// Run one command in a pane and report how it went.
///
/// The command is bracketed by two sentinels the pane's shell prints, so the
/// output is exactly what the command wrote and the exit status is the
/// command's own. The pane's shell has to cooperate for that, which is why
/// this reports a pane that is not at a prompt rather than waiting on one.
///
/// # Errors
///
/// Returns an error when the pane cannot be watched or the keys cannot be
/// sent.
pub(crate) async fn run_command(
    pane: &Pane,
    command: &str,
    timeout: Duration,
    suppress_history: bool,
    cancelled: &CancellationToken,
) -> Result<RunView, Error> {
    let mut run = start_run(pane, command, suppress_history).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut outcome = RunOutcome::Deadline;

    loop {
        let chunk = tokio::select! {
            biased;
            // Checked first so a request cancelled while output is already
            // waiting still stops, rather than reading one more chunk.
            () = cancelled.cancelled() => {
                outcome = RunOutcome::Cancelled;
                break;
            }
            chunk = tokio::time::timeout_at(deadline, run.output.next_chunk()) => chunk,
        };
        match chunk {
            Ok(Some(chunk)) => {
                if let Some(mut view) = run.scanner.push(&chunk) {
                    view.pane = run.output.pane().to_string();
                    // Shutting down is what distinguishes a connection that
                    // broke from one that closed, so its failure is this
                    // call's failure.
                    run.output.shutdown().await?;
                    return Ok(view);
                }
            }
            Ok(None) => {
                outcome = RunOutcome::PaneClosed;
                break;
            }
            Err(_) => break,
        }
    }

    let view = run.scanner.unfinished(outcome, run.pane.clone());
    run.output.shutdown().await?;

    Ok(view)
}

/// A pane stream and the sentinels for one command.
///
/// Held so a caller can decide how long to read for -- to a deadline, as
/// `run_command` does, or until it ends, as a background job does.
pub(crate) struct Run {
    output: libtmux::control::PaneOutput,
    scanner: Scanner,
    pane: String,
}

/// A watched run whose pane has not been changed yet.
pub(crate) struct PreparedRun {
    pane: Pane,
    payload: String,
    run: Run,
}

/// Whether tmux confirmed the sends that start a watched run.
#[must_use = "an unknown dispatch retains the watcher for a command that may be running"]
pub(crate) enum RunDispatch {
    /// Both the payload and Enter were acknowledged.
    Confirmed(Run),
    /// The first send was rejected before tmux could receive pane input.
    NotDispatched(Error),
    /// Delivery cannot be proved either way, so the watcher stays owned.
    Unknown { run: Run, error: Error },
}

/// Say whether an error proves that the subprocess dispatch never started.
///
/// Timeout and executor shutdown are deliberately absent: each can occur
/// before spawn or after tmux accepted the command, and the variants do not
/// retain which phase produced them.
fn definitely_not_dispatched(error: &Error) -> bool {
    matches!(
        error,
        Error::Overloaded { .. }
            | Error::InvalidCommandInput { .. }
            | Error::ExecutableNotFound { .. }
            | Error::Spawn { .. }
            | Error::DuplicateRequest { .. }
    )
}

impl PreparedRun {
    /// Send the prepared payload and Enter while retaining its watcher.
    pub(crate) async fn dispatch(self) -> RunDispatch {
        let Self { pane, payload, run } = self;
        if let Err(error) = pane.send_keys(payload).await {
            if definitely_not_dispatched(&error) {
                return RunDispatch::NotDispatched(error);
            }
            return RunDispatch::Unknown { run, error };
        }
        if let Err(error) = pane.send_key_names(["Enter"]).await {
            return RunDispatch::Unknown { run, error };
        }
        RunDispatch::Confirmed(run)
    }
}

/// Attach a watcher and construct a run without sending pane input.
///
/// # Errors
///
/// Returns an error when the pane cannot be watched.
pub(crate) async fn prepare_run(
    pane: &Pane,
    command: &str,
    suppress_history: bool,
) -> Result<PreparedRun, Error> {
    let nonce = format!(
        "{:x}{:x}",
        std::process::id(),
        RUN_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let opened = format!("\x1b_{nonce}s\x1b\\").into_bytes();
    let closed = format!("\x1b_{nonce}e;").into_bytes();

    // Attached before the keys are sent, so no output can arrive unseen.
    let output = pane.stream_output().await?;

    // The command runs in a subshell: a bare `exit` in it would otherwise end
    // the pane's own shell. A leading space is how `suppress_history` keeps
    // the line out of shell history, for shells configured to do that.
    let lead = if suppress_history { " " } else { "" };
    let payload = format!(
        "{lead}printf '\\033_{nonce}s\\033\\\\'; ( {command} ); __tmux_mcp=$?; \
         printf '\\033_{nonce}e;%d\\033\\\\' \"$__tmux_mcp\"; unset __tmux_mcp"
    );

    Ok(PreparedRun {
        pane: pane.clone(),
        payload,
        run: Run {
            output,
            scanner: Scanner::new(opened, closed),
            pane: pane.id().to_string(),
        },
    })
}

/// Send a command to a pane, bracketed by the sentinels that delimit it.
///
/// Returns once the keys are away and the connection is open. Nothing has been
/// read yet, so nothing has been missed: the stream was attached first.
///
/// # Errors
///
/// Returns an error when the pane cannot be watched or the keys cannot be
/// sent.
pub(crate) async fn start_run(
    pane: &Pane,
    command: &str,
    suppress_history: bool,
) -> Result<Run, Error> {
    match prepare_run(pane, command, suppress_history)
        .await?
        .dispatch()
        .await
    {
        RunDispatch::Confirmed(run) => Ok(run),
        RunDispatch::NotDispatched(error) | RunDispatch::Unknown { error, .. } => Err(error),
    }
}

impl Run {
    /// Read until the command ends or the pane closes, publishing as it goes.
    ///
    /// `publish` receives the command's output so far and how many bytes were
    /// dropped from the front of it, so a poller sees progress rather than
    /// only the answer.
    pub(crate) async fn collect(
        mut self,
        mut publish: impl FnMut(&[u8], u64, &TextFilter),
    ) -> RunView {
        while let Some(chunk) = self.output.next_chunk().await {
            let finished = self.scanner.push(&chunk);
            publish(
                self.scanner.body(),
                self.scanner.body_dropped(),
                self.scanner.body_checkpoint(),
            );

            if let Some(mut view) = finished {
                view.pane = self.pane.clone();
                let _ = self.output.shutdown().await;
                return view;
            }
        }

        let view = self
            .scanner
            .unfinished(RunOutcome::PaneClosed, self.pane.clone());
        let _ = self.output.shutdown().await;
        view
    }
}

/// Watch a pane until it stops writing for `quiet`, or time runs out.
///
/// The complement of [`wait_for_text`], for the case where a caller cannot
/// name what success looks like: a TUI finishing its redraw, an installer
/// settling, a prompt whose glyph nobody can predict. Reads the same stream,
/// so it measures what the program wrote rather than what the screen shows.
///
/// # Errors
///
/// Returns an error when the pane cannot be watched.
pub(crate) async fn wait_for_idle(
    pane: &Pane,
    quiet: Duration,
    timeout: Duration,
    cancelled: &CancellationToken,
) -> Result<IdleView, Error> {
    let mut output = pane.stream_output().await?;

    let mut filter = TextFilter::new();
    let mut text: Vec<u8> = Vec::new();
    let mut bytes = 0usize;
    let mut outcome = IdleOutcome::Deadline;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_wrote = tokio::time::Instant::now();

    loop {
        // Whichever comes first: the caller's deadline, or the pane having
        // been quiet long enough.
        let quiet_at = last_wrote + quiet;
        let until = quiet_at.min(deadline);

        let chunk = tokio::select! {
            biased;
            () = cancelled.cancelled() => {
                outcome = IdleOutcome::Cancelled;
                break;
            }
            chunk = tokio::time::timeout_at(until, output.next_chunk()) => chunk,
        };
        match chunk {
            Ok(Some(chunk)) => {
                bytes = bytes.saturating_add(chunk.len());
                filter.push(&chunk, &mut text);
                last_wrote = tokio::time::Instant::now();

                if text.len() > OUTPUT_LIMIT {
                    let excess = text.len() - OUTPUT_LIMIT;
                    text.drain(..excess);
                }
            }
            Ok(None) => {
                outcome = IdleOutcome::PaneClosed;
                break;
            }
            // Nothing arrived before `until`. Which deadline that was decides
            // whether the pane went quiet or the caller ran out of time.
            Err(_) => {
                if quiet_at <= deadline {
                    outcome = IdleOutcome::Idle;
                }
                break;
            }
        }
    }

    let view = IdleView {
        pane: pane.id().to_string(),
        outcome,
        quiet_seconds: last_wrote.elapsed().as_secs(),
        text: String::from_utf8_lossy(&text).into_owned(),
        bytes,
    };
    output.shutdown().await?;

    Ok(view)
}

/// Collects a pane's output and watches it for the sentinels bracketing a run.
///
/// Separate from the read loop so it can be driven with chunk boundaries in
/// awkward places. tmux decides where a chunk ends, and the sentinel arriving
/// split from the status digits that follow it is the case worth proving.
struct Scanner {
    opened: Vec<u8>,
    closed: Vec<u8>,
    collected: Vec<u8>,
    /// How far the search for the closing sentinel has looked.
    scanned: usize,
    /// Where the closing sentinel was found, once it has been.
    ///
    /// Held rather than searched for again: the status digits that complete
    /// the block can arrive in a later chunk, and by then the sentinel sits
    /// behind everything newly scanned.
    close_at: Option<usize>,
    /// Where the command's own output begins, once the opening sentinel has
    /// arrived. An index into `collected`, moved when the front is trimmed.
    body_at: Option<usize>,
    /// How many bytes of the command's output trimming has dropped.
    body_dropped: u64,
    /// Filter state at the first retained byte of the command's output.
    body_checkpoint: TextFilter,
    bytes: usize,
    truncated: bool,
}

impl Scanner {
    fn new(opened: Vec<u8>, closed: Vec<u8>) -> Self {
        Self {
            opened,
            closed,
            collected: Vec::new(),
            scanned: 0,
            close_at: None,
            body_at: None,
            body_dropped: 0,
            body_checkpoint: TextFilter::new(),
            bytes: 0,
            truncated: false,
        }
    }

    /// Take one chunk, and report the run if it completed it.
    fn push(&mut self, chunk: &[u8]) -> Option<RunView> {
        self.bytes = self.bytes.saturating_add(chunk.len());
        self.collected.extend_from_slice(chunk);

        if self.body_at.is_none() {
            self.body_at = find(&self.collected, &self.opened).map(|at| at + self.opened.len());
        }

        if self.close_at.is_none() {
            // Scanning forward only, with an overlap wide enough that a
            // sentinel split across two chunks is still seen.
            let from = self
                .scanned
                .saturating_sub(self.closed.len().saturating_sub(1));
            self.close_at = find(&self.collected[from..], &self.closed).map(|at| from + at);
            self.scanned = self.collected.len();

            // Trimming only while the end is still out of sight. Once the
            // closing sentinel has been found the run is bytes from done, and
            // not moving the buffer keeps the recorded position true.
            if self.close_at.is_none() && self.collected.len() > OUTPUT_LIMIT {
                let excess = self.collected.len() - OUTPUT_LIMIT;
                if let Some(body_at) = self.body_at {
                    // Trimming eats the command's output only once it has
                    // eaten everything before it.
                    let body_excess = excess.saturating_sub(body_at);
                    self.body_checkpoint
                        .advance(&self.collected[body_at..body_at + body_excess]);
                    self.body_dropped = self.body_dropped.saturating_add(body_excess as u64);
                    self.body_at = Some(body_at.saturating_sub(excess));
                }
                self.collected.drain(..excess);
                self.scanned = self.scanned.saturating_sub(excess);
                self.truncated = true;
            }
        }

        let at = self.close_at?;
        let mut view = finished(&self.collected, at, &self.opened, &self.closed)?;
        view.bytes = self.bytes;
        view.truncated = self.truncated;
        Some(view)
    }

    /// The command's own output so far, between the sentinels.
    ///
    /// Empty until the opening sentinel arrives, which is what separates the
    /// shell's echo of the typed line from what the command wrote.
    fn body(&self) -> &[u8] {
        let Some(from) = self.body_at else {
            return &[];
        };
        let to = self.close_at.unwrap_or(self.collected.len());
        self.collected.get(from..to).unwrap_or_default()
    }

    /// How many bytes of the command's output were dropped to bound memory.
    const fn body_dropped(&self) -> u64 {
        self.body_dropped
    }

    /// Filter state at the first byte returned by [`Self::body`].
    const fn body_checkpoint(&self) -> &TextFilter {
        &self.body_checkpoint
    }

    /// Report a run that stopped without completing.
    fn unfinished(&self, outcome: RunOutcome, pane: String) -> RunView {
        // Nothing came back at all: the keys went somewhere that is not a
        // shell prompt. Worth its own answer, because retrying will not help.
        let outcome =
            if outcome == RunOutcome::Deadline && find(&self.collected, &self.opened).is_none() {
                RunOutcome::NoShell
            } else {
                outcome
            };

        RunView {
            pane,
            outcome,
            exit_status: None,
            output: readable(&self.collected),
            bytes: self.bytes,
            truncated: self.truncated,
        }
    }
}

/// Assemble the answer once the closing sentinel is whole.
///
/// Returns `None` while the status digits are still arriving, so the caller
/// reads more rather than reporting a truncated number.
fn finished(collected: &[u8], at: usize, opened: &[u8], closed: &[u8]) -> Option<RunView> {
    let digits_from = at + closed.len();
    let terminator = find(&collected[digits_from..], b"\x1b\\")?;
    let status = std::str::from_utf8(&collected[digits_from..digits_from + terminator])
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok());

    // Everything between the sentinels is the command's own output. The echo
    // of the typed line sits before the opening sentinel: a shell echoes the
    // source text, in which the escape is the four characters `\033`, so it
    // can never be mistaken for the sentinel itself.
    //
    // Both sentinels are printed by one command line, so seeing the closing
    // one without the opening one means trimming dropped it. What is left is
    // still the command's output, minus its beginning, and reporting it beats
    // reporting nothing.
    let body = find(collected, opened).map_or(&collected[..at], |start| {
        &collected[start + opened.len()..at]
    });

    Some(RunView {
        // Filled in by the caller, which is what holds the connection.
        pane: String::new(),
        outcome: RunOutcome::Completed,
        exit_status: status,
        output: readable(body),
        bytes: 0,
        truncated: false,
    })
}

/// Watch a pane until a pattern matches, a stop pattern matches, or time runs
/// out.
///
/// # Errors
///
/// Returns an error when the pane cannot be watched or read.
pub(crate) async fn wait_for_text(
    pane: &Pane,
    patterns: &Patterns,
    stops: &Patterns,
    timeout: Duration,
    cancelled: &CancellationToken,
) -> Result<WaitView, Error> {
    // Attached first: a pattern that arrives while the screen is being read
    // must still be seen.
    let mut output = pane.stream_output().await?;

    // What is already on screen will never match, because a stream only
    // carries what comes next. Saying so is cheaper than a wasted deadline.
    let present_at_entry = match pane.capture_with(CaptureOptions::visible()).await {
        Ok(lines) => {
            let mut screen = Vec::new();
            for line in &lines {
                screen.extend_from_slice(line.as_bytes());
                screen.push(b'\n');
            }
            patterns.first_match(&screen).is_some()
        }
        // A screen that cannot be read is not a reason to refuse to wait.
        Err(_) => false,
    };

    let mut filter = TextFilter::new();
    let mut text: Vec<u8> = Vec::new();
    let mut bytes = 0usize;
    let mut outcome = WaitOutcome::Deadline;
    let mut matched_index = None;
    let mut matched_pattern = None;
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let chunk = tokio::select! {
            biased;
            // Checked first so a request cancelled while output is already
            // waiting still stops, rather than reading one more chunk.
            () = cancelled.cancelled() => {
                outcome = WaitOutcome::Cancelled;
                break;
            }
            chunk = tokio::time::timeout_at(deadline, output.next_chunk()) => chunk,
        };
        match chunk {
            Ok(Some(chunk)) => {
                bytes = bytes.saturating_add(chunk.len());
                filter.push(&chunk, &mut text);

                if let Some((index, source)) = stops.first_match(&text) {
                    outcome = WaitOutcome::Stopped;
                    matched_index = Some(index);
                    matched_pattern = Some(source.to_owned());
                    break;
                }
                // No patterns means "wait for anything at all", which any
                // output satisfies.
                if patterns.is_empty() {
                    if !text.is_empty() {
                        outcome = WaitOutcome::Matched;
                        break;
                    }
                } else if let Some((index, source)) = patterns.first_match(&text) {
                    outcome = WaitOutcome::Matched;
                    matched_index = Some(index);
                    matched_pattern = Some(source.to_owned());
                    break;
                }

                if text.len() > OUTPUT_LIMIT {
                    let excess = text.len() - OUTPUT_LIMIT;
                    text.drain(..excess);
                }
            }
            Ok(None) => {
                outcome = WaitOutcome::PaneClosed;
                break;
            }
            Err(_) => break,
        }
    }

    let pane_id = output.pane().to_string();
    output.shutdown().await?;

    Ok(WaitView {
        pane: pane_id,
        outcome,
        matched_index,
        matched_pattern,
        present_at_entry,
        text: String::from_utf8_lossy(&text).into_owned(),
        bytes,
    })
}

/// Render collected bytes as text, with escape sequences removed.
pub(crate) fn readable(bytes: &[u8]) -> String {
    readable_from(&TextFilter::new(), bytes, 0)
}

/// Find the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_a_needle_reports_where_it_starts() {
        assert_eq!(find(b"abcdef", b"cd"), Some(2));
        assert_eq!(find(b"abcdef", b"xy"), None);
        assert_eq!(find(b"ab", b"abcdef"), None);
        assert_eq!(find(b"abc", b""), None);
    }

    #[test]
    fn a_literal_pattern_is_not_a_regular_expression() {
        let patterns = Patterns::compile(&["a.c".to_owned()], false, true)
            .unwrap_or_else(|_| unreachable!("a literal always compiles"));

        assert!(patterns.first_match(b"a.c").is_some());
        assert!(
            patterns.first_match(b"abc").is_none(),
            "the dot must match itself when the caller asked for literal text"
        );
    }

    #[test]
    fn a_regular_expression_is_one_when_asked() {
        let patterns = Patterns::compile(&["a.c".to_owned()], true, true)
            .unwrap_or_else(|_| unreachable!("a valid expression compiles"));

        assert!(patterns.first_match(b"abc").is_some());
    }

    #[test]
    fn matching_ignores_case_unless_asked() {
        let insensitive = Patterns::compile(&["DONE".to_owned()], false, false)
            .unwrap_or_else(|_| unreachable!("a literal always compiles"));
        let sensitive = Patterns::compile(&["DONE".to_owned()], false, true)
            .unwrap_or_else(|_| unreachable!("a literal always compiles"));

        assert!(insensitive.first_match(b"done").is_some());
        assert!(sensitive.first_match(b"done").is_none());
    }

    #[test]
    fn the_first_pattern_given_is_the_one_reported() {
        let patterns = Patterns::compile(&["one".to_owned(), "two".to_owned()], false, true)
            .unwrap_or_else(|_| unreachable!("literals always compile"));

        assert_eq!(patterns.first_match(b"two one"), Some((0, "one")));
    }

    #[test]
    fn a_bad_expression_names_itself() {
        let error = Patterns::compile(&["a(".to_owned()], true, true);
        let (source, _reason) = error
            .err()
            .unwrap_or_else(|| unreachable!("`a(` is invalid"));

        assert_eq!(source, "a(");
    }

    #[test]
    fn an_invalid_literal_is_still_a_literal() {
        // `a(` is not a valid expression, but as text it is ordinary.
        let patterns = Patterns::compile(&["a(".to_owned()], false, true)
            .unwrap_or_else(|_| unreachable!("escaping makes any text valid"));

        assert!(patterns.first_match(b"a(").is_some());
    }

    /// Feed a scanner one run's stream, split at the given byte offsets.
    fn scan(stream: &[u8], splits: &[usize]) -> Option<RunView> {
        let mut scanner = Scanner::new(b"\x1b_Ns\x1b\\".to_vec(), b"\x1b_Ne;".to_vec());
        let mut at = 0;
        let mut finished = None;
        for &next in splits.iter().chain(std::iter::once(&stream.len())) {
            let chunk = &stream[at..next];
            at = next;
            finished = finished.or_else(|| scanner.push(chunk));
        }
        finished
    }

    fn one_run() -> Vec<u8> {
        let mut stream = Vec::new();
        stream.extend_from_slice(br"printf '\033_Ns\033\\'; ( echo hi ); ");
        stream.extend_from_slice(b"\r\n\x1b_Ns\x1b\\hi\r\n\x1b_Ne;42\x1b\\");
        stream
    }

    #[test]
    fn a_run_arriving_whole_is_read() {
        let view = scan(&one_run(), &[]).unwrap_or_else(|| unreachable!("the run completed"));

        assert_eq!(view.exit_status, Some(42));
        assert_eq!(view.output, "hi\n");
    }

    #[test]
    fn scanner_publishes_state_at_a_trimmed_body_start() {
        let opened = b"\x1b_Ns\x1b\\".to_vec();
        let mut scanner = Scanner::new(opened.clone(), b"\x1b_Ne;".to_vec());
        let mut body = b"\x1b[31mred".to_vec();
        body.resize(OUTPUT_LIMIT + 4, b'x');
        let mut stream = opened;
        stream.extend_from_slice(&body);

        assert!(scanner.push(&stream).is_none());
        assert_eq!(scanner.body_dropped(), 4);

        let text = readable_from(scanner.body_checkpoint(), scanner.body(), 0);

        assert!(text.starts_with("red"));
        assert_eq!(text.len(), OUTPUT_LIMIT - 1);
    }

    #[test]
    fn a_run_split_between_its_sentinel_and_its_status_is_still_read() {
        // tmux decides where a chunk ends. Splitting immediately after the
        // closing sentinel leaves the status digits for a later chunk, by
        // which time the sentinel is behind everything newly scanned.
        let stream = one_run();
        let after_sentinel = stream.len() - "42\x1b\\".len();

        let view = scan(&stream, &[after_sentinel])
            .unwrap_or_else(|| unreachable!("a split chunk must not lose the run"));

        assert_eq!(view.exit_status, Some(42));
        assert_eq!(view.output, "hi\n");
    }

    #[test]
    fn a_run_split_at_every_byte_is_still_read() {
        let stream = one_run();
        let splits: Vec<usize> = (1..stream.len()).collect();

        let view = scan(&stream, &splits)
            .unwrap_or_else(|| unreachable!("no chunk boundary may lose the run"));

        assert_eq!(view.exit_status, Some(42));
        assert_eq!(view.output, "hi\n");
    }

    #[test]
    fn a_run_that_never_answered_is_reported_as_no_shell() {
        let mut scanner = Scanner::new(b"\x1b_Ns\x1b\\".to_vec(), b"\x1b_Ne;".to_vec());
        assert!(scanner.push(b"some editor drew a screen").is_none());

        let view = scanner.unfinished(RunOutcome::Deadline, "%0".to_owned());

        assert_eq!(view.outcome, RunOutcome::NoShell);
        assert!(view.exit_status.is_none());
    }

    #[test]
    fn a_run_still_going_at_its_deadline_keeps_that_outcome() {
        let mut scanner = Scanner::new(b"\x1b_Ns\x1b\\".to_vec(), b"\x1b_Ne;".to_vec());
        assert!(scanner.push(b"\x1b_Ns\x1b\\working").is_none());

        let view = scanner.unfinished(RunOutcome::Deadline, "%0".to_owned());

        assert_eq!(
            view.outcome,
            RunOutcome::Deadline,
            "the shell answered, so the command is merely slow"
        );
    }

    #[test]
    fn a_status_is_read_from_between_the_sentinels() {
        let opened = b"\x1b_1s\x1b\\";
        let closed = b"\x1b_1e;";
        let mut stream = Vec::new();
        stream.extend_from_slice(b"echo hi\r\n");
        stream.extend_from_slice(opened);
        stream.extend_from_slice(b"hi\r\n");
        stream.extend_from_slice(closed);
        stream.extend_from_slice(b"7\x1b\\");

        let at = find(&stream, closed).unwrap_or_else(|| unreachable!("the sentinel is present"));
        let view = finished(&stream, at, opened, closed)
            .unwrap_or_else(|| unreachable!("the block is whole"));

        assert_eq!(view.exit_status, Some(7));
        assert_eq!(view.output, "hi\n");
    }

    #[test]
    fn a_half_arrived_status_is_not_reported() {
        let opened = b"\x1b_1s\x1b\\";
        let closed = b"\x1b_1e;";
        let mut stream = Vec::new();
        stream.extend_from_slice(opened);
        stream.extend_from_slice(b"out");
        stream.extend_from_slice(closed);
        stream.extend_from_slice(b"12");

        let at = find(&stream, closed).unwrap_or_else(|| unreachable!("the sentinel is present"));

        assert!(
            finished(&stream, at, opened, closed).is_none(),
            "reading 1 from a status of 12 would be worse than waiting"
        );
    }

    #[test]
    fn the_echoed_command_is_not_mistaken_for_a_sentinel() {
        let opened = b"\x1b_ab1s\x1b\\";
        let closed = b"\x1b_ab1e;";
        let mut stream = Vec::new();
        // What a shell echoes: the source text, where the escape is four
        // ordinary characters.
        stream.extend_from_slice(br"printf '\033_ab1s\033\\'; ( echo hi ); ");
        stream.extend_from_slice(br"printf '\033_ab1e;%d\033\\' $s");
        stream.extend_from_slice(b"\r\n");
        stream.extend_from_slice(opened);
        stream.extend_from_slice(b"hi\r\n");
        stream.extend_from_slice(closed);
        stream.extend_from_slice(b"0\x1b\\");

        let at = find(&stream, closed).unwrap_or_else(|| unreachable!("the sentinel is present"));
        let view = finished(&stream, at, opened, closed)
            .unwrap_or_else(|| unreachable!("the block is whole"));

        assert_eq!(
            view.output, "hi\n",
            "the echo sits before the opening sentinel and is discarded whole"
        );
        assert_eq!(view.exit_status, Some(0));
    }
}
