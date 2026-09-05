//! Sentinel-bracketed command dispatch and stream scanning.

use std::ffi::{OsStr, OsString};
use std::ops::Range;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use libtmux::{Error, Pane};

use crate::retained::RetainedBytes;
use crate::text::{TextFilter, readable_from};

use super::{OUTPUT_LIMIT, RunOutcome, RunView};

/// Distinguishes one run's sentinels from another's.
///
/// Only has to be unique among the runs this process makes; a sentinel is
/// already unmistakable in the stream because it carries a real escape byte.
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
static PREPARED_SHUTDOWN_COMPLETIONS: AtomicU64 = AtomicU64::new(0);

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
    pub(super) fn publication_bytes(&self) -> usize {
        self.appended.len()
    }
}

/// A watched run whose pane has not been changed yet.
pub(crate) struct PreparedRun {
    pane: Pane,
    payload: OsString,
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

    /// Close the watcher without sending the prepared pane input.
    pub(crate) async fn shutdown(self) -> Result<(), Error> {
        let Self { run, .. } = self;
        let Run { output, .. } = run;
        let result = output.shutdown().await;
        #[cfg(test)]
        PREPARED_SHUTDOWN_COMPLETIONS.fetch_add(1, Ordering::Relaxed);
        result
    }
}

#[cfg(test)]
pub(crate) fn prepared_shutdown_completions() -> u64 {
    PREPARED_SHUTDOWN_COMPLETIONS.load(Ordering::Relaxed)
}

pub(super) fn quote_shell_word(value: &OsStr) -> OsString {
    let mut quoted = Vec::with_capacity(value.as_bytes().len() + 2);
    quoted.push(b'\'');
    for byte in value.as_bytes() {
        if *byte == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(*byte);
        }
    }
    quoted.push(b'\'');
    OsString::from_vec(quoted)
}

fn marker_client(executable: &OsStr, socket: &Path, nonce: &str, closing: bool) -> Vec<u8> {
    let mut client = Vec::new();
    client.extend_from_slice(br"( \exec ");
    client.extend_from_slice(quote_shell_word(executable).as_bytes());
    client.extend_from_slice(br" -S ");
    client.extend_from_slice(quote_shell_word(socket.as_os_str()).as_bytes());
    client.extend_from_slice(br#" run-shell "printf '\\033_"#);
    client.extend_from_slice(nonce.as_bytes());
    if closing {
        client.extend_from_slice(br#"e;$1\\033\\\\'" )"#);
    } else {
        client.extend_from_slice(br#"s\\033\\\\'" )"#);
    }
    client
}

fn append_command_branch(
    payload: &mut Vec<u8>,
    command: &OsStr,
    inherited_xtrace: bool,
    inherited_errexit: bool,
    opening: &[u8],
    closing: &[u8],
) {
    payload.extend_from_slice(if inherited_errexit {
        b"*e*)\n\\set +e\nif "
    } else {
        b"*)\n\\set +e\nif "
    });
    payload.extend_from_slice(opening);
    payload.extend_from_slice(b"; then\n( ");
    payload.extend_from_slice(if inherited_errexit {
        b"\\set -e; \\eval "
    } else {
        b"\\set +e; \\eval "
    });
    let operand = if inherited_xtrace {
        let mut operand = OsString::from("\\set -x\n");
        operand.push(command);
        operand
    } else {
        command.to_os_string()
    };
    payload.extend_from_slice(quote_shell_word(&operand).as_bytes());
    payload.extend_from_slice(b" )\n\\set -- \"$?\"\n");
    payload.extend_from_slice(closing);
    payload.extend_from_slice(b"\nfi\n;;\n");
}

pub(super) fn render_payload(
    executable: &OsStr,
    socket: &Path,
    nonce: &str,
    command: &OsStr,
    suppress_history: bool,
) -> OsString {
    let opening = marker_client(executable, socket, nonce, false);
    let closing = marker_client(executable, socket, nonce, true);
    let mut payload = Vec::new();
    if suppress_history {
        payload.push(b' ');
    }
    payload.extend_from_slice(b"(\ncase $- in\n*x*)\n\\set +x\ncase $- in\n");
    append_command_branch(&mut payload, command, true, true, &opening, &closing);
    append_command_branch(&mut payload, command, true, false, &opening, &closing);
    payload.extend_from_slice(b"esac\n;;\n*)\ncase $- in\n");
    append_command_branch(&mut payload, command, false, true, &opening, &closing);
    append_command_branch(&mut payload, command, false, false, &opening, &closing);
    payload.extend_from_slice(b"esac\n;;\nesac\n)");
    OsString::from_vec(payload)
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
    executable: &OsStr,
    socket: &Path,
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

    let payload = render_payload(
        executable,
        socket,
        &nonce,
        OsStr::new(command),
        suppress_history,
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

/// Collects a pane's output and watches it for the sentinels bracketing a run.
///
/// Separate from the read loop so it can be driven with chunk boundaries in
/// awkward places. tmux decides where a chunk ends, and the sentinel arriving
/// split from the status digits that follow it is the case worth proving.
pub(super) struct Scanner {
    opened: Vec<u8>,
    closed: Vec<u8>,
    collected: RetainedBytes,
    /// How far the search for the opening sentinel has looked.
    open_scanned: usize,
    /// The byte immediately after a found opening sentinel.
    open_at: Option<usize>,
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
    pub(super) fn new(opened: Vec<u8>, closed: Vec<u8>) -> Self {
        Self {
            opened,
            closed,
            collected: RetainedBytes::new(),
            open_scanned: 0,
            open_at: None,
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
    pub(super) fn push(&mut self, chunk: &[u8]) -> Option<RunView> {
        self.bytes = self.bytes.saturating_add(chunk.len());
        let previously_retained = self.collected.len();
        self.collected.append(chunk);
        self.publish_drop = 0;
        self.publish_append = chunk.len();

        if self.body_at.is_none() {
            if self.open_at.is_none() {
                let collected = self.collected.as_slice();
                let from = self
                    .open_scanned
                    .saturating_sub(self.opened.len().saturating_sub(1));
                self.open_at =
                    find(&collected[from..], &self.opened).map(|at| from + at + self.opened.len());
                self.open_scanned = collected.len();
            }
            if let Some(open_at) = self.open_at {
                self.body_at = opening_separator_end(self.collected.as_slice(), open_at);
            }
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
            if let Some(open_at) = self.open_at {
                self.open_at = (excess <= open_at).then_some(open_at.saturating_sub(excess));
            }
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
        })
    }

    /// Borrow the state an owner needs to publish this run's progress.
    pub(super) fn progress(&self) -> RunProgress<'_> {
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
    pub(super) fn physical_bytes(&self) -> usize {
        self.collected.physical_len()
    }

    #[cfg(test)]
    pub(super) fn physical_capacity(&self) -> usize {
        self.collected.physical_capacity()
    }

    #[cfg(test)]
    pub(super) fn retained(&self) -> &[u8] {
        self.collected.as_slice()
    }

    /// Report a run that stopped without completing.
    pub(super) fn unfinished(&self, outcome: RunOutcome, pane: String) -> RunView {
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
        }
    }
}

fn opening_separator_end(collected: &[u8], at: usize) -> Option<usize> {
    match collected.get(at..) {
        Some([b'\n', ..]) => Some(at + 1),
        Some([b'\r', b'\n', ..]) => Some(at + 2),
        _ => None,
    }
}

/// Assemble the answer once the closing sentinel is whole.
///
/// Returns `None` while the status digits are still arriving, so the caller
/// reads more rather than reporting a truncated number.
#[cfg(test)]
pub(super) fn finished(
    collected: &[u8],
    at: usize,
    opened: &[u8],
    closed: &[u8],
) -> Option<RunView> {
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
    let body = find(collected, opened)
        .and_then(|start| opening_separator_end(collected, start + opened.len()))
        .map_or(&collected[..at], |start| &collected[start..at]);

    Some(RunView {
        // Filled in by the caller, which is what holds the connection.
        pane: String::new(),
        outcome: RunOutcome::Completed,
        exit_status: completion.exit_status,
        output: readable(body),
        bytes: 0,
        truncated: false,
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

/// Render collected bytes as text, with escape sequences removed.
pub(crate) fn readable(bytes: &[u8]) -> String {
    readable_from(&TextFilter::new(), bytes, 0)
}

/// Find the first occurrence of `needle` in `haystack`.
pub(super) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
