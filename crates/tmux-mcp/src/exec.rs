//! Running a command in a pane, and waiting for one to say something.
//!
//! Both read the pane's output stream rather than its screen. A screen is what
//! survived rendering; the stream is everything the program wrote, in the
//! order it wrote it, including what has already scrolled away. Nothing here
//! polls, and nothing here depends on tmux still holding a line in scrollback.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use libtmux::{CaptureOptions, ControlLimits, ControlModeErrorKind, Error, Pane};
use regex::bytes::Regex;
use serde::Serialize;

use crate::retained::MAX_BYTES as OUTPUT_LIMIT;
#[cfg(test)]
use crate::retained::{COMPACT_AFTER, RetainedBytes};
use crate::text::TextFilter;
#[cfg(test)]
use crate::text::readable_from;

const MAX_PATTERNS: usize = 32;
const MAX_PATTERN_BYTES: usize = 4_096;
const MAX_TOTAL_PATTERN_BYTES: usize = 16_384;

mod run;

#[cfg(test)]
pub(crate) use run::observing_prepared_shutdowns;
#[cfg(test)]
use run::{
    FrameError, Scanner, TRAP_DECLARATION_LIMIT, find, frame_path, frame_with_random,
    inherited_trap_capture, quote_shell_word, render_payload, stage_frame, staged_line,
};
pub(crate) use run::{
    PrepareRunError, RunDispatch, RunProgress, prepare_run, readable, route_is_terminal_safe,
    route_path_is_terminal_safe,
};

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
    /// A wanted pattern was already in the pane's output before this began
    /// watching, on a row above the one still being typed into.
    ///
    /// A wait only sees what a pane writes after it starts, so this is never
    /// folded into [`Self::Matched`]: the same pattern printed moments
    /// earlier -- an earlier command's own output, or the shell's echo of a
    /// line that has already been submitted -- can already be sitting there,
    /// and a caller that treated that as a fresh match would act on
    /// something that happened before this call, not because of it.
    PresentAtEntry,
    /// A wanted pattern's only occurrence is the row still being typed
    /// into: text this server (or a person sharing the pane) sent and has
    /// not submitted, not anything that has run.
    ///
    /// Submit it, then wait again: the next wait sees the command's own
    /// output on a row above a new one still being typed into, which is
    /// [`Self::PresentAtEntry`] or [`Self::Matched`] depending on when it
    /// arrived, never this.
    Pending,
    /// A pattern matched, in output that arrived after the wait attached.
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
    /// What the pane wrote, with escape sequences removed.
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
        if sources.len() > MAX_PATTERNS {
            return Err((
                "set".to_owned(),
                format!("contains more than {MAX_PATTERNS} patterns"),
            ));
        }
        let mut compiled = Vec::with_capacity(sources.len());
        let mut total_bytes = 0_usize;
        for (index, source) in sources.iter().enumerate() {
            if source.len() > MAX_PATTERN_BYTES {
                return Err((
                    format!("{}", index + 1),
                    format!("exceeds {MAX_PATTERN_BYTES} bytes"),
                ));
            }
            total_bytes = total_bytes.saturating_add(source.len());
            if total_bytes > MAX_TOTAL_PATTERN_BYTES {
                return Err((
                    "set".to_owned(),
                    format!("exceeds {MAX_TOTAL_PATTERN_BYTES} bytes in total"),
                ));
            }
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
    wait_for_text_with_limits(
        pane,
        patterns,
        stops,
        timeout,
        cancelled,
        ControlLimits::default(),
    )
    .await
}

/// Like [`wait_for_text`], with explicit control-mode frame budgets.
///
/// Every real caller wants [`wait_for_text`]'s default: this exists so a test
/// can shrink the budget enough to force the frame-too-large shutdown error
/// [`wait_for_text`] propagates instead of tolerating -- a branch no MCP
/// tool argument can reach, since exposing a protocol-tuning knob to a
/// caller of the tool would leak an implementation detail into its surface.
///
/// Split from [`wait_on_output`] at the attach point so a test driving a
/// tiny budget can send its adversarial output only once attaching has
/// provably finished, rather than racing a fixed delay against it.
pub(crate) async fn wait_for_text_with_limits(
    pane: &Pane,
    patterns: &Patterns,
    stops: &Patterns,
    timeout: Duration,
    cancelled: &CancellationToken,
    limits: ControlLimits,
) -> Result<WaitView, Error> {
    // Attached first: a pattern that arrives while the screen is being read
    // must still be seen. Reading first would lose one that landed between
    // the capture and the attach, and wait out the deadline over output that
    // did arrive. One landing in that gap is reported as present at entry
    // instead, which is still true of the screen.
    let output = pane.stream_output_with_limits(limits).await?;
    if let Some(view) = read_present_at_entry(pane, patterns).await? {
        // The answer is already in hand; a failure closing a stream nothing
        // read does not change it.
        let _ = output.shutdown().await;
        return Ok(view);
    }
    wait_on_output(pane, output, patterns, stops, timeout, cancelled).await
}

/// One pane's screen, split at the row still being typed into.
///
/// Two tmux round trips read this, not one: the cursor row first, then the
/// screen. They are not atomic, so a line arriving between the two can only
/// move the cursor down and make `pending` cover a later row -- excluding
/// more from `above`, never less -- which is the safe direction to be wrong
/// in.
struct Screen {
    /// Every visible row above the one still being typed into, each
    /// terminated with a newline: completed output, never text this server
    /// or a person sent and has not submitted.
    above: Vec<u8>,
    /// The row still being typed into, terminated with a newline to match
    /// `above`'s rows.
    pending: Vec<u8>,
}

impl Screen {
    /// Capture `pane`'s current screen, split at its cursor row.
    ///
    /// `None` when the pane cannot be read; the same failure surfaces again
    /// from whatever the caller does next.
    async fn capture(pane: &Pane) -> Option<Self> {
        let cursor_row: usize = pane
            .format("#{cursor_y}")
            .await
            .ok()?
            .to_string_lossy()
            .trim()
            .parse()
            .ok()?;
        let lines = pane.capture_with(CaptureOptions::visible()).await.ok()?;
        // A cursor row past the last captured line is conservative rather
        // than a decode failure: treat every visible row as still pending.
        let pending_row = cursor_row.min(lines.len().saturating_sub(1));

        let mut above = Vec::new();
        for line in lines.iter().take(pending_row) {
            above.extend_from_slice(line.as_bytes());
            above.push(b'\n');
        }
        let mut pending = lines
            .get(pending_row)
            .map_or_else(Vec::new, |line| line.as_bytes().to_vec());
        pending.push(b'\n');

        Some(Self { above, pending })
    }

    /// Both halves, in screen order, for a view that reports the whole
    /// thing rather than only whichever half matched.
    fn whole(&self) -> Vec<u8> {
        let mut all = self.above.clone();
        all.extend_from_slice(&self.pending);
        all
    }
}

/// Report a wanted pattern already in the pane's output, or still only on
/// the row being typed into, before any stream attaches to watch for one
/// arriving.
///
/// See [`WaitOutcome::PresentAtEntry`] and [`WaitOutcome::Pending`] for why
/// these are distinct outcomes from [`WaitOutcome::Matched`] rather than a
/// flag alongside it.
async fn read_present_at_entry(
    pane: &Pane,
    patterns: &Patterns,
) -> Result<Option<WaitView>, Error> {
    // No patterns means "wait for anything at all", which nothing already on
    // screen can pre-empt: there is nothing yet to call present.
    if patterns.is_empty() {
        return Ok(None);
    }

    // A screen that cannot be read is not a reason to refuse to wait; the
    // same failure surfaces from the attach right after this.
    let Some(screen) = Screen::capture(pane).await else {
        return Ok(None);
    };

    let outcome = patterns
        .first_match(&screen.above)
        .map(|found| (WaitOutcome::PresentAtEntry, found))
        .or_else(|| {
            patterns
                .first_match(&screen.pending)
                .map(|found| (WaitOutcome::Pending, found))
        });
    let Some((outcome, (index, source))) = outcome else {
        return Ok(None);
    };
    let whole = screen.whole();

    Ok(Some(WaitView {
        pane: pane.id().to_string(),
        outcome,
        matched_index: Some(index),
        matched_pattern: Some(source.to_owned()),
        text: String::from_utf8_lossy(&whole).into_owned(),
        bytes: whole.len(),
    }))
}

/// The read loop [`wait_for_text_with_limits`] runs once attached.
async fn wait_on_output(
    pane: &Pane,
    mut output: libtmux::control::PaneOutput,
    patterns: &Patterns,
    stops: &Patterns,
    timeout: Duration,
    cancelled: &CancellationToken,
) -> Result<WaitView, Error> {
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
                    // Fresh bytes on this connection are not necessarily a
                    // submitted line: the kernel echoes what was typed at
                    // once, and a shell's line editor re-prints the buffer
                    // when it starts reading, both genuinely new output that
                    // can still be sitting on the row being typed into.
                    // Confirmed only once the *current* screen shows the
                    // pattern above that row.
                    let confirmed = Screen::capture(pane)
                        .await
                        .is_some_and(|screen| patterns.first_match(&screen.above).is_some());
                    if confirmed {
                        outcome = WaitOutcome::Matched;
                        matched_index = Some(index);
                        matched_pattern = Some(source.to_owned());
                        break;
                    }
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
    // Ordinary EOF (`Closed`) is tolerated: the pane stopped being read, so
    // that alone is not a failure. Any other shutdown error -- frame budget,
    // timeout, executor shutdown -- is real and discards the view above.
    if let Err(error) = output.shutdown().await
        && !matches!(
            error,
            Error::ControlMode {
                kind: ControlModeErrorKind::Closed,
                ..
            }
        )
    {
        return Err(error);
    }

    // A chunk can arrive in the same instant the deadline elapses; without
    // this, that race would report `Deadline` while `text` already holds a
    // match, the same shape the .NET port hit.
    let (mut outcome, mut matched_index, mut matched_pattern) =
        reconcile_deadline(outcome, matched_index, matched_pattern, patterns, &text);
    // `reconcile_deadline` reads the same accumulated buffer the main loop
    // does, and is subject to the same trap: the promotion it just made can
    // still be the pane's own not-yet-submitted line racing the deadline,
    // not a genuine match. Confirmed the same way, against the row still
    // being typed into, or the promotion is undone.
    if matches!(outcome, WaitOutcome::Matched)
        && Screen::capture(pane)
            .await
            .is_none_or(|screen| patterns.first_match(&screen.above).is_none())
    {
        outcome = WaitOutcome::Deadline;
        matched_index = None;
        matched_pattern = None;
    }

    Ok(WaitView {
        pane: pane_id,
        outcome,
        matched_index,
        matched_pattern,
        text: String::from_utf8_lossy(&text).into_owned(),
        bytes,
    })
}

/// Reclassify a timed-out wait as matched when the buffer it is about to
/// report already contains a pattern.
///
/// Only `Deadline` is reconsidered: `Stopped`, `PaneClosed`, and `Cancelled`
/// already carry their own reason and are returned unchanged.
fn reconcile_deadline(
    outcome: WaitOutcome,
    matched_index: Option<usize>,
    matched_pattern: Option<String>,
    patterns: &Patterns,
    text: &[u8],
) -> (WaitOutcome, Option<usize>, Option<String>) {
    if !matches!(outcome, WaitOutcome::Deadline) {
        return (outcome, matched_index, matched_pattern);
    }
    match patterns.first_match(text) {
        Some((index, source)) => (WaitOutcome::Matched, Some(index), Some(source.to_owned())),
        None => (outcome, matched_index, matched_pattern),
    }
}

#[cfg(test)]
mod tests;
