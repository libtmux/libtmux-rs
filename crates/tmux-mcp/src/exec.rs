//! Running a command in a pane, and waiting for one to say something.
//!
//! Both read the pane's output stream rather than its screen. A screen is what
//! survived rendering; the stream is everything the program wrote, in the
//! order it wrote it, including what has already scrolled away. Nothing here
//! polls, and nothing here depends on tmux still holding a line in scrollback.

use std::ops::Range;
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

/// How much dead prefix a command buffer holds before moving its live bytes.
const COMPACT_AFTER: usize = 64 * 1024;

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
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
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
    /// The background job retaining this run after waiting stopped.
    ///
    /// Absent once the command has a terminal outcome. Pass this to
    /// `job_status` or `forget_job` instead of retrying the command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
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

/// A pane stream and the sentinels for one command.
pub(crate) struct Run {
    output: libtmux::control::PaneOutput,
    scanner: Scanner,
    pane: String,
}

/// One immutable delta from a run's collector.
pub(crate) struct RunProgress<'a> {
    pub(crate) appended: &'a [u8],
    pub(crate) discarded: usize,
    pub(crate) body: Option<Range<usize>>,
    pub(crate) body_dropped: u64,
    pub(crate) body_checkpoint: &'a TextFilter,
    pub(crate) bytes: usize,
    pub(crate) truncated: bool,
}

#[cfg(test)]
impl RunProgress<'_> {
    fn publication_bytes(&self) -> usize {
        self.appended.len()
    }
}

/// A contiguous retained window with a logical front.
#[derive(Debug, Default)]
pub(crate) struct RetainedBytes {
    bytes: Vec<u8>,
    head: usize,
}

impl RetainedBytes {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn from_slice(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            head: 0,
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[self.head..]
    }

    fn len(&self) -> usize {
        self.bytes.len() - self.head
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    pub(crate) fn discard(&mut self, bytes: usize) {
        debug_assert!(bytes <= self.len());
        self.head += bytes.min(self.len());
        self.settle();
    }

    pub(crate) fn settle(&mut self) {
        if self.head >= COMPACT_AFTER {
            let retained = self.len();
            self.bytes.copy_within(self.head.., 0);
            self.bytes.truncate(retained);
            self.head = 0;
        }
        if self.bytes.capacity() > OUTPUT_LIMIT + COMPACT_AFTER {
            let retained = self.as_slice();
            debug_assert!(retained.len() <= OUTPUT_LIMIT);
            let mut bounded = Vec::with_capacity(retained.len() + COMPACT_AFTER);
            bounded.extend_from_slice(retained);
            self.bytes = bounded;
            self.head = 0;
        }
    }

    #[cfg(test)]
    fn physical_len(&self) -> usize {
        self.bytes.len()
    }
}

/// A watched run whose pane has not been changed yet.
pub(crate) struct PreparedRun {
    pane: Pane,
    payload: String,
    run: Run,
}

/// Whether tmux confirmed the line dispatch that starts a watched run.
#[must_use = "an unknown dispatch retains the watcher for a command that may be running"]
pub(crate) enum RunDispatch {
    /// The payload and its terminating Enter were acknowledged.
    Confirmed(Run),
    /// The line dispatch was rejected before tmux could receive pane input.
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
        if let Err(error) = pane.send_line(payload).await {
            if definitely_not_dispatched(&error) {
                return RunDispatch::NotDispatched(error);
            }
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

impl Run {
    /// Read until the command ends or the pane closes, publishing as it goes.
    ///
    /// `publish` receives only bytes added to the retained window and how much
    /// of its preceding front to discard. A poller therefore sees progress
    /// without copying the whole window for every pane-stream chunk.
    pub(crate) async fn collect(mut self, mut publish: impl FnMut(RunProgress<'_>)) -> RunView {
        while let Some(chunk) = self.output.next_chunk().await {
            let finished = self.scanner.push(&chunk);
            publish(self.scanner.progress());

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
    collected: RetainedBytes,
    /// How far the search for the opening sentinel has looked.
    open_scanned: usize,
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
    /// Bytes the last update discards from the preceding publication.
    publish_drop: usize,
    /// Bytes from the last chunk that remain in the retained window.
    publish_append: usize,
    bytes: usize,
    truncated: bool,
}

impl Scanner {
    fn new(opened: Vec<u8>, closed: Vec<u8>) -> Self {
        Self {
            opened,
            closed,
            collected: RetainedBytes::new(),
            open_scanned: 0,
            scanned: 0,
            close_at: None,
            body_at: None,
            body_dropped: 0,
            body_checkpoint: TextFilter::new(),
            publish_drop: 0,
            publish_append: 0,
            bytes: 0,
            truncated: false,
        }
    }

    /// Take one chunk, and report the run if it completed it.
    fn push(&mut self, chunk: &[u8]) -> Option<RunView> {
        self.bytes = self.bytes.saturating_add(chunk.len());
        let previously_retained = self.collected.len();
        self.collected.append(chunk);
        self.publish_drop = 0;
        self.publish_append = chunk.len();

        if self.body_at.is_none() {
            let collected = self.collected.as_slice();
            let from = self
                .open_scanned
                .saturating_sub(self.opened.len().saturating_sub(1));
            self.body_at =
                find(&collected[from..], &self.opened).map(|at| from + at + self.opened.len());
            self.open_scanned = collected.len();
        }

        if self.close_at.is_none() {
            // Scanning forward only, with an overlap wide enough that a
            // sentinel split across two chunks is still seen.
            let from = self
                .scanned
                .saturating_sub(self.closed.len().saturating_sub(1));
            let collected = self.collected.as_slice();
            self.close_at = find(&collected[from..], &self.closed).map(|at| from + at);
            self.scanned = collected.len();
        }

        if self.collected.len() > OUTPUT_LIMIT {
            let excess = self.collected.len() - OUTPUT_LIMIT;
            self.publish_drop = excess.min(previously_retained);
            self.publish_append = chunk
                .len()
                .saturating_sub(excess.saturating_sub(previously_retained));
            if let Some(body_at) = self.body_at {
                // Trimming eats the command's output only once it has eaten
                // everything before it.
                let body_excess = excess.saturating_sub(body_at);
                self.body_checkpoint
                    .advance(&self.collected.as_slice()[body_at..body_at + body_excess]);
                self.body_dropped = self.body_dropped.saturating_add(body_excess as u64);
                self.body_at = Some(body_at.saturating_sub(excess));
            }
            self.open_scanned = self.open_scanned.saturating_sub(excess);
            if let Some(close_at) = self.close_at {
                if excess <= close_at {
                    self.close_at = Some(close_at - excess);
                    self.scanned = self.scanned.saturating_sub(excess);
                } else {
                    // A closing marker whose terminator falls more than one
                    // retained window later cannot be completed from bounded
                    // state. Resume looking for the wrapper's final marker.
                    self.close_at = None;
                    self.scanned = 0;
                }
            } else {
                self.scanned = self.scanned.saturating_sub(excess);
            }
            self.collected.discard(excess);
            self.truncated = true;
        }
        self.collected.settle();

        let at = self.close_at?;
        let collected = self.collected.as_slice();
        let completion = completion(collected, at, &self.closed)?;
        let output = self.body_at.filter(|&from| from <= at).map_or_else(
            || readable(&collected[..at]),
            |from| readable_from(&self.body_checkpoint, &collected[from..at], 0),
        );
        Some(RunView {
            pane: String::new(),
            outcome: RunOutcome::Completed,
            exit_status: completion.exit_status,
            output,
            bytes: self.bytes,
            truncated: self.truncated,
            job: None,
        })
    }

    /// Borrow the state an owner needs to publish this run's progress.
    fn progress(&self) -> RunProgress<'_> {
        let retained = self.collected.as_slice();
        let appended_at = retained.len().saturating_sub(self.publish_append);
        RunProgress {
            appended: &retained[appended_at..],
            discarded: self.publish_drop,
            body: self
                .body_at
                .map(|from| from..self.close_at.unwrap_or(retained.len())),
            body_dropped: self.body_dropped,
            body_checkpoint: &self.body_checkpoint,
            bytes: self.bytes,
            truncated: self.truncated,
        }
    }

    #[cfg(test)]
    fn physical_bytes(&self) -> usize {
        self.collected.physical_len()
    }

    #[cfg(test)]
    fn physical_capacity(&self) -> usize {
        self.collected.bytes.capacity()
    }

    #[cfg(test)]
    fn retained(&self) -> &[u8] {
        self.collected.as_slice()
    }

    /// Report a run that stopped without completing.
    fn unfinished(&self, outcome: RunOutcome, pane: String) -> RunView {
        // Nothing came back at all: the keys went somewhere that is not a
        // shell prompt. Worth its own answer, because retrying will not help.
        let collected = self.collected.as_slice();
        let outcome = if outcome == RunOutcome::Deadline && self.body_at.is_none() {
            RunOutcome::NoShell
        } else {
            outcome
        };
        let output = self.body_at.map_or_else(
            || readable(collected),
            |from| readable_from(&self.body_checkpoint, &collected[from..], 0),
        );

        RunView {
            pane,
            outcome,
            exit_status: None,
            output,
            bytes: self.bytes,
            truncated: self.truncated,
            job: None,
        }
    }
}

/// Assemble the answer once the closing sentinel is whole.
///
/// Returns `None` while the status digits are still arriving, so the caller
/// reads more rather than reporting a truncated number.
#[cfg(test)]
fn finished(collected: &[u8], at: usize, opened: &[u8], closed: &[u8]) -> Option<RunView> {
    let completion = completion(collected, at, closed)?;

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
        exit_status: completion.exit_status,
        output: readable(body),
        bytes: 0,
        truncated: false,
        job: None,
    })
}

struct Completion {
    exit_status: Option<i32>,
}

fn completion(collected: &[u8], at: usize, closed: &[u8]) -> Option<Completion> {
    let digits_from = at + closed.len();
    let terminator = find(&collected[digits_from..], b"\x1b\\")?;
    let status = std::str::from_utf8(&collected[digits_from..digits_from + terminator])
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok());
    Some(Completion {
        exit_status: status,
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
mod tests;
