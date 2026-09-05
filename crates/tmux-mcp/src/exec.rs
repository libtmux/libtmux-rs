//! Running a command in a pane, and waiting for one to say something.
//!
//! Both read the pane's output stream rather than its screen. A screen is what
//! survived rendering; the stream is everything the program wrote, in the
//! order it wrote it, including what has already scrolled away. Nothing here
//! polls, and nothing here depends on tmux still holding a line in scrollback.

use std::time::Duration;

use tokio_util::sync::CancellationToken;

use libtmux::{CaptureOptions, Error, Pane};
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
pub(crate) use run::prepared_shutdown_completions;
#[cfg(test)]
use run::{FrameError, Scanner, find, frame_with_random, quote_shell_word, render_payload};
pub(crate) use run::{PrepareRunError, RunDispatch, RunProgress, prepare_run, readable};

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

#[cfg(test)]
mod tests;
