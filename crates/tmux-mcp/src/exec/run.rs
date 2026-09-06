//! Sentinel-bracketed command dispatch and stream scanning.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::ops::Range;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::Path;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use libtmux::{CaptureOptions, Error, Pane, Server, ServerGeneration};

use crate::retained::RetainedBytes;
use crate::text::{TextFilter, readable_from};

use super::{OUTPUT_LIMIT, RunOutcome, RunView};

const MARKER_PREFIX: &str = "__LIBTMUX_MCP_DONE_";
const NONCE_ATTEMPTS: usize = 32;
const PROOF_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const PROOF_RETRY_DELAY: Duration = Duration::from_millis(50);
const PROOF_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);
pub(super) const TRAP_DECLARATION_LIMIT: usize = 64 * 1024;

#[cfg(test)]
tokio::task_local! {
    /// Prepared shutdowns completed inside one [`observing_prepared_shutdowns`]
    /// scope. Task-local rather than global: several tests drive the guard
    /// refusal path concurrently, and a process-wide counter made every
    /// observation depend on which of them happened to be running.
    static PREPARED_SHUTDOWNS: std::sync::Arc<AtomicU64>;
}

/// A pane stream and the sentinels for one command.
pub(crate) struct Run {
    output: libtmux::control::PaneOutput,
    scanner: Scanner,
    pane: Pane,
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

/// A collected view and any proof still needed after its watcher closed.
pub(crate) struct RunCollection {
    view: RunView,
    proof: Option<RunProof>,
}

impl RunCollection {
    pub(crate) fn into_parts(self) -> (RunView, Option<RunProof>) {
        (self.view, self.proof)
    }

    pub(crate) async fn finish_proof(self) {
        if let Some(proof) = self.proof {
            proof.wait().await;
        }
    }
}

/// Evidence retained after a watcher transport closes without a marker.
pub(crate) struct RunProof {
    pane: Pane,
    server: Server,
    generation: ServerGeneration,
    closing: Vec<u8>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum GenerationState {
    Current,
    Ended,
    Ambiguous,
}

impl RunProof {
    async fn generation_state(&self) -> GenerationState {
        match tokio::time::timeout(
            PROOF_PROBE_TIMEOUT,
            self.server.require_generation(self.generation),
        )
        .await
        {
            Ok(Ok(())) => GenerationState::Current,
            Ok(Err(Error::ServerGenerationChanged { .. } | Error::ServerGone { .. })) => {
                GenerationState::Ended
            }
            Ok(Err(_)) | Err(_) => GenerationState::Ambiguous,
        }
    }

    async fn settled(&self) -> bool {
        match self.generation_state().await {
            GenerationState::Ended => return true,
            GenerationState::Ambiguous => return false,
            GenerationState::Current => {}
        }

        let (capture, refreshed) = tokio::join!(
            tokio::time::timeout(
                PROOF_PROBE_TIMEOUT,
                self.pane
                    .capture_with(CaptureOptions::history().join_wrapped()),
            ),
            tokio::time::timeout(PROOF_PROBE_TIMEOUT, self.pane.refreshed()),
        );

        match self.generation_state().await {
            GenerationState::Ended => return true,
            GenerationState::Ambiguous => return false,
            GenerationState::Current => {}
        }

        let marker = matches!(&capture, Ok(Ok(lines)) if lines.iter().any(|line| {
            completion_status(line.as_bytes(), &self.closing).is_some()
        }));
        let pane_ended = matches!(&refreshed, Ok(Ok(pane)) if pane.is_dead())
            || matches!(&refreshed, Ok(Err(Error::ObjectGone { .. })))
            || matches!(&capture, Ok(Err(Error::ObjectGone { .. })));
        marker || pane_ended
    }

    pub(crate) async fn wait(self) {
        let mut delay = PROOF_RETRY_DELAY;
        loop {
            if self.settled().await {
                return;
            }
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(PROOF_RETRY_MAX_DELAY);
        }
    }
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

/// Why a run could not be prepared before any pane input was sent.
#[derive(Debug)]
pub(crate) enum PrepareRunError {
    Tmux(Error),
    Frame,
}

impl From<Error> for PrepareRunError {
    fn from(error: Error) -> Self {
        Self::Tmux(error)
    }
}

/// Why a collision-free completion frame could not be constructed.
#[derive(Debug)]
pub(crate) enum FrameError {
    TerminalControl,
    Entropy,
    Collisions,
}

pub(super) struct Frame {
    pub(super) payload: OsString,
    pub(super) opened: Vec<u8>,
    pub(super) closed: Vec<u8>,
}

pub(super) struct TrapCapture {
    pub(super) setup: Vec<u8>,
    declarations: String,
    errexit: String,
    pub(super) status: String,
    xtrace: String,
}

fn shell_basename(shell: &[u8]) -> &[u8] {
    let basename = shell
        .rsplit(|byte| *byte == b'/')
        .next()
        .unwrap_or_default();
    basename.strip_prefix(b"-").unwrap_or(basename)
}

pub(super) fn inherited_trap_capture(shell: &[u8], nonce: &str) -> Option<TrapCapture> {
    let shell = shell_basename(shell);
    if shell != b"bash" && shell != b"zsh" {
        return None;
    }

    let declarations = format!("__libtmux_mcp_traps_{nonce}");
    let errexit = format!("__libtmux_mcp_trap_errexit_{nonce}");
    let status = format!("__libtmux_mcp_trap_status_{nonce}");
    let xtrace = format!("__libtmux_mcp_trap_xtrace_{nonce}");
    let file = format!("__libtmux_mcp_trap_file_{nonce}");
    let read_owned = format!("__libtmux_mcp_trap_read_owned_{nonce}");
    let write_owned = format!("__libtmux_mcp_trap_write_owned_{nonce}");
    let prefix = format!("/tmp/libtmux-mcp-traps-{nonce}");
    let template = format!("{prefix}.XXXXXX");
    let template = quote_shell_word(OsStr::new(&template));
    let prefix = quote_shell_word(OsStr::new(&prefix));

    // Every statement gets its own line. A terminal in canonical mode caps
    // one input line at `MAX_CANON`, which is 1024 bytes on macOS against
    // 4096 on Linux, and this frame is typed into a pane rather than piped.
    // Joined with `; ` the bash setup reached 1715 bytes, so the line could
    // never be delivered there and the run reported `no_shell` forever.
    let mut setup = format!(
        "{declarations}=\n{errexit}=0\n{status}=125\n{xtrace}=0\n\
         case $- in *e*) {errexit}=1;; esac\n\
         case $- in *x*) {xtrace}=1; \\set +x;; esac\n\
         {file}=\n{read_owned}=0\n{write_owned}=0\n"
    );
    if shell == b"bash" {
        let files = format!("__libtmux_mcp_trap_files_{nonce}");
        let _ = write!(
            setup,
            "{files}=()\nif /usr/bin/mktemp {} >/dev/null; then\n",
            template.to_string_lossy()
        );
        let discover = format!("{files}=({}.??????)", prefix.to_string_lossy());
        setup.push_str("case $- in *f*) \\set +f; ");
        setup.push_str(&discover);
        setup.push_str("; \\set -f ;; *) ");
        setup.push_str(&discover);
        setup.push_str(" ;; esac\n");
        setup.push_str("if [ \"${#");
        setup.push_str(&files);
        setup.push_str("[@]}\" -eq 1 ]; then\n");
        setup.push_str(&file);
        setup.push_str("=\"${");
        setup.push_str(&files);
        setup.push_str("[0]}\"\nelif ! /bin/rm -f \"${");
        setup.push_str(&files);
        setup.push_str("[@]}\"; then\n");
        setup.push_str(&status);
        setup.push_str("=125\nfi\nfi\n");
    } else {
        let _ = writeln!(
            setup,
            "if /usr/bin/mktemp {} | IFS= \\read -r {file}; then :; else {file}=; fi",
            template.to_string_lossy()
        );
    }
    let _ = write!(
        setup,
        "if [ -n \"${file}\" ] && [ -f \"${file}\" ] && [ -O \"${file}\" ] \
         && ! ( : >&8 ) 2>/dev/null && ! ( : <&8 ) 2>/dev/null \\
         && ! ( : >&9 ) 2>/dev/null && ! ( : <&9 ) 2>/dev/null \\
         && \\exec 8<> \"${file}\" && {write_owned}=1 \\
         && \\exec 9< \"${file}\" && {read_owned}=1 \\
         && /bin/rm -f \"${file}\" && {file}=; then\n"
    );
    if shell == b"bash" {
        setup.push_str("if \\trap -p ERR DEBUG >&8; then\n");
    } else {
        setup.push_str("if \\trap - $signals[1,-3]; then\nif \\trap >&8; then\n");
    }
    let capture_close = if shell == b"zsh" { "fi\n" } else { "" };
    let _ = write!(
        setup,
        "{status}=0\nfi\n{capture_close}fi\n\\
         if ! \\trap - ERR DEBUG; then {status}=125; fi\n\\
         if [ \"${write_owned}\" -eq 1 ]; then\n\\
         if ! \\exec 8>&-; then {status}=125; fi\n{write_owned}=0\nfi\n\\
         if [ \"${status}\" -eq 0 ] && [ \"${read_owned}\" -eq 1 ]; then\n\\
         if {declarations}=$(LC_ALL=C /usr/bin/head -c {TRAP_DECLARATION_LIMIT} <&9) \\
         && [ \"$(LC_ALL=C /usr/bin/head -c 1 <&9 | /usr/bin/wc -c)\" -eq 0 ]; then :\n\\
         else {declarations}=\n{status}=125\nfi\nfi\n\\
         if [ \"${read_owned}\" -eq 1 ]; then\n\\
         if ! \\exec 9<&-; then {status}=125; fi\n{read_owned}=0\nfi\n\\
         if [ -n \"${file}\" ] && ! /bin/rm -f \"${file}\"; then {status}=125; fi\n"
    );

    Some(TrapCapture {
        setup: setup.into_bytes(),
        declarations,
        errexit,
        status,
        xtrace,
    })
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
        let _ = PREPARED_SHUTDOWNS.try_with(|count| count.fetch_add(1, Ordering::Relaxed));
        result
    }
}

/// Run `future`, reporting how many prepared shutdowns completed inside it.
///
/// The count covers this task alone, so a concurrent test driving the same
/// refusal path cannot inflate it.
#[cfg(test)]
pub(crate) async fn observing_prepared_shutdowns<F: Future>(future: F) -> (F::Output, u64) {
    let count = std::sync::Arc::new(AtomicU64::new(0));
    let output = PREPARED_SHUTDOWNS
        .scope(std::sync::Arc::clone(&count), future)
        .await;
    (output, count.load(Ordering::Relaxed))
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

pub(crate) fn route_path_is_terminal_safe(value: &OsStr) -> bool {
    !value.as_bytes().iter().any(u8::is_ascii_control)
}

pub(crate) fn route_is_terminal_safe(executable: &OsStr, socket: &Path) -> bool {
    route_path_is_terminal_safe(executable) && route_path_is_terminal_safe(socket.as_os_str())
}

fn display_client(executable: &OsStr, socket: &Path, message: &[u8]) -> Vec<u8> {
    let mut client = Vec::new();
    client.extend_from_slice(br"( \exec ");
    client.extend_from_slice(quote_shell_word(executable).as_bytes());
    client.extend_from_slice(br" -N -S ");
    client.extend_from_slice(quote_shell_word(socket.as_os_str()).as_bytes());
    client.extend_from_slice(b" display-message -p ");
    client.extend_from_slice(message);
    client.extend_from_slice(b" )");
    client
}

fn marker_message(nonce: &str, closing: bool) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(b"'__LIBTMUX_MCP_DONE_''");
    message.extend_from_slice(nonce.as_bytes());
    if closing {
        message.extend_from_slice(b"''__:'\"$1\"");
    } else {
        message.extend_from_slice(b"''__:BEGIN'");
    }
    message
}

fn append_command_branch(
    payload: &mut Vec<u8>,
    command: &OsStr,
    inherited_xtrace: bool,
    inherited_errexit: bool,
    separator: &[u8],
    opening: &[u8],
    closing: &[u8],
) {
    payload.extend_from_slice(if inherited_errexit {
        b"*e*)\n\\set +e\nif "
    } else {
        b"*)\n\\set +e\nif "
    });
    payload.extend_from_slice(separator);
    payload.extend_from_slice(b" && ");
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
    payload.extend_from_slice(separator);
    payload.push(b'\n');
    payload.extend_from_slice(closing);
    payload.extend_from_slice(b"\nfi\n;;\n");
}

fn render_trapped_payload(
    command: &OsStr,
    suppress_history: bool,
    capture: &TrapCapture,
    separator: &[u8],
    opening: &[u8],
    closing: &[u8],
) -> OsString {
    let mut payload = Vec::new();
    if suppress_history {
        payload.push(b' ');
    }
    payload.extend_from_slice(b"(\n");
    payload.extend_from_slice(&capture.setup);
    payload.extend_from_slice(b"\\set +e\nif ");
    payload.extend_from_slice(separator);
    payload.extend_from_slice(b" && ");
    payload.extend_from_slice(opening);
    payload.extend_from_slice(b"; then\nif [ \"$");
    payload.extend_from_slice(capture.status.as_bytes());
    payload.extend_from_slice(b"\" -eq 0 ]; then\n( case \"$");
    payload.extend_from_slice(capture.errexit.as_bytes());
    payload.extend_from_slice(b"\" in 1) \\set -e;; *) \\set +e;; esac\n\\eval \"");
    payload.push(b'$');
    payload.extend_from_slice(capture.declarations.as_bytes());
    payload.extend_from_slice(b"\n\"");
    let mut operand = OsString::from("case \"$");
    operand.push(&capture.xtrace);
    operand.push("\" in 1) \\set -x;; esac\n");
    operand.push(command);
    payload.extend_from_slice(quote_shell_word(&operand).as_bytes());
    payload.extend_from_slice(b" )\n\\set -- \"$?\"\nelse\n\\set -- 125\nfi\n");
    payload.extend_from_slice(separator);
    payload.push(b'\n');
    payload.extend_from_slice(closing);
    payload.extend_from_slice(b"\nfi\n)");
    OsString::from_vec(payload)
}

pub(super) fn render_payload(
    executable: &OsStr,
    socket: &Path,
    nonce: &str,
    shell: &[u8],
    command: &OsStr,
    suppress_history: bool,
) -> OsString {
    let separator = display_client(executable, socket, b"''");
    let opening = display_client(executable, socket, &marker_message(nonce, false));
    let closing = display_client(executable, socket, &marker_message(nonce, true));
    let trap_capture = inherited_trap_capture(shell, nonce);
    if let Some(capture) = trap_capture.as_ref() {
        return render_trapped_payload(
            command,
            suppress_history,
            capture,
            &separator,
            &opening,
            &closing,
        );
    }
    let mut payload = Vec::new();
    if suppress_history {
        payload.push(b' ');
    }
    payload.extend_from_slice(b"(\ncase $- in\n*x*)\n\\set +x\ncase $- in\n");
    append_command_branch(
        &mut payload,
        command,
        true,
        true,
        &separator,
        &opening,
        &closing,
    );
    append_command_branch(
        &mut payload,
        command,
        true,
        false,
        &separator,
        &opening,
        &closing,
    );
    payload.extend_from_slice(b"esac\n;;\n*)\ncase $- in\n");
    append_command_branch(
        &mut payload,
        command,
        false,
        true,
        &separator,
        &opening,
        &closing,
    );
    append_command_branch(
        &mut payload,
        command,
        false,
        false,
        &separator,
        &opening,
        &closing,
    );
    payload.extend_from_slice(b"esac\n;;\nesac\n)");
    OsString::from_vec(payload)
}

fn frame_with_nonce(
    executable: &OsStr,
    socket: &Path,
    nonce: &str,
    shell: &[u8],
    command: &OsStr,
    suppress_history: bool,
) -> Option<Frame> {
    let marker = format!("{MARKER_PREFIX}{nonce}__").into_bytes();
    let payload = render_payload(executable, socket, nonce, shell, command, suppress_history);
    if find(payload.as_bytes(), &marker).is_some() {
        return None;
    }
    let mut opened = marker.clone();
    opened.extend_from_slice(b":BEGIN");
    let mut closed = marker;
    closed.push(b':');
    Some(Frame {
        payload,
        opened,
        closed,
    })
}

pub(super) fn frame_with_random(
    executable: &OsStr,
    socket: &Path,
    shell: &[u8],
    command: &OsStr,
    suppress_history: bool,
    mut fill: impl FnMut(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<Frame, FrameError> {
    if !route_is_terminal_safe(executable, socket) {
        return Err(FrameError::TerminalControl);
    }
    for _ in 0..NONCE_ATTEMPTS {
        let mut random = [0_u8; 16];
        fill(&mut random).map_err(|_| FrameError::Entropy)?;
        let nonce = format!("{:032x}", u128::from_be_bytes(random));
        if let Some(frame) =
            frame_with_nonce(executable, socket, &nonce, shell, command, suppress_history)
        {
            return Ok(frame);
        }
    }
    Err(FrameError::Collisions)
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
    shell: &[u8],
) -> Result<PreparedRun, PrepareRunError> {
    let Frame {
        payload,
        opened,
        closed,
    } = frame_with_random(
        executable,
        socket,
        shell,
        OsStr::new(command),
        suppress_history,
        getrandom::fill,
    )
    .map_err(|_| PrepareRunError::Frame)?;

    // Attached before the keys are sent, so no output can arrive unseen.
    let output = pane.stream_output().await?;

    Ok(PreparedRun {
        pane: pane.clone(),
        payload,
        run: Run {
            output,
            scanner: Scanner::new(opened, closed),
            pane: pane.clone(),
        },
    })
}

impl Run {
    pub(crate) fn proof(&self, server: Server, generation: ServerGeneration) -> RunProof {
        RunProof {
            pane: self.pane.clone(),
            server,
            generation,
            closing: self.scanner.closed.clone(),
        }
    }

    /// Read until the command ends or the pane closes, publishing as it goes.
    ///
    /// `publish` receives only bytes added to the retained window and how much
    /// of its preceding front to discard. A poller therefore sees progress
    /// without copying the whole window for every pane-stream chunk.
    pub(crate) async fn collect(
        mut self,
        server: Server,
        generation: ServerGeneration,
        mut publish: impl FnMut(RunProgress<'_>),
    ) -> RunCollection {
        while let Some(chunk) = self.output.next_chunk().await {
            let finished = self.scanner.push(&chunk);
            publish(self.scanner.progress());

            if let Some(mut view) = finished {
                view.pane = self.pane.id().to_string();
                let _ = self.output.shutdown().await;
                return RunCollection { view, proof: None };
            }
        }

        let view = self
            .scanner
            .unfinished(RunOutcome::PaneClosed, self.pane.id().to_string());
        let proof = RunProof {
            pane: self.pane,
            server,
            generation,
            closing: self.scanner.closed,
        };
        let _ = self.output.shutdown().await;
        RunCollection {
            view,
            proof: Some(proof),
        }
    }
}

/// Collects a pane's output and watches for exact completion-record lines.
///
/// Separate from the read loop so it can be driven with chunk boundaries in
/// awkward places. tmux decides where a chunk ends, including inside a marker.
pub(super) struct Scanner {
    opened: Vec<u8>,
    closed: Vec<u8>,
    max_line: usize,
    line: Vec<u8>,
    line_too_long: bool,
    line_last_was_cr: bool,
    line_start: usize,
    /// Start of the line ending immediately before the current line.
    line_boundary: Option<usize>,
    collected: RetainedBytes,
    /// Absolute offset of the first command-output byte.
    body_start: Option<usize>,
    completion: Option<Completion>,
    /// Absolute offset of the first retained byte.
    retained_from: usize,
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
        let max_line = opened.len().max(closed.len() + 3);
        Self {
            opened,
            closed,
            max_line,
            line: Vec::with_capacity(max_line + 1),
            line_too_long: false,
            line_last_was_cr: false,
            line_start: 0,
            line_boundary: None,
            collected: RetainedBytes::new(),
            body_start: None,
            completion: None,
            retained_from: 0,
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
        let chunk_at = self.bytes;
        self.bytes = self.bytes.saturating_add(chunk.len());
        self.scan_lines(chunk, chunk_at);
        let previously_retained = self.collected.len();
        self.collected.append(chunk);
        self.publish_drop = 0;
        self.publish_append = chunk.len();

        if self.collected.len() > OUTPUT_LIMIT {
            let excess = self.collected.len() - OUTPUT_LIMIT;
            self.publish_drop = excess.min(previously_retained);
            self.publish_append = chunk
                .len()
                .saturating_sub(excess.saturating_sub(previously_retained));
            let retained_to = self.retained_from.saturating_add(excess);
            if let Some(body_start) = self.body_start {
                let dropped_from = self.retained_from.max(body_start);
                let dropped_to = retained_to.min(
                    self.completion
                        .map_or(retained_to, |completion| completion.body_end),
                );
                if dropped_from < dropped_to {
                    let local_from = dropped_from - self.retained_from;
                    let local_to = dropped_to - self.retained_from;
                    self.body_checkpoint
                        .advance(&self.collected.as_slice()[local_from..local_to]);
                    self.body_dropped = self
                        .body_dropped
                        .saturating_add((dropped_to - dropped_from) as u64);
                }
            }
            self.collected.discard(excess);
            self.retained_from = retained_to;
            self.truncated = true;
        }
        self.collected.settle();

        let completion = self.completion?;
        let body = self.body_range()?;
        let output = readable_from(&self.body_checkpoint, &self.collected.as_slice()[body], 0);
        Some(RunView {
            pane: String::new(),
            outcome: RunOutcome::Completed,
            exit_status: Some(completion.exit_status),
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
            body: self.body_range(),
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

    #[cfg(test)]
    pub(super) fn frame_line_capacity(&self) -> usize {
        self.line.capacity()
    }

    /// Report a run that stopped without completing.
    pub(super) fn unfinished(&self, outcome: RunOutcome, pane: String) -> RunView {
        // Nothing came back at all: the keys went somewhere that is not a
        // shell prompt. Worth its own answer, because retrying will not help.
        let collected = self.collected.as_slice();
        let outcome = if outcome == RunOutcome::Deadline && self.body_start.is_none() {
            RunOutcome::NoShell
        } else {
            outcome
        };
        let output = self.body_range().map_or_else(
            || readable(collected),
            |body| readable_from(&self.body_checkpoint, &collected[body], 0),
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

    fn scan_lines(&mut self, chunk: &[u8], chunk_at: usize) {
        for (offset, byte) in chunk.iter().copied().enumerate() {
            let at = chunk_at.saturating_add(offset);
            if byte == b'\n' {
                let content_len = self
                    .line
                    .len()
                    .saturating_sub(usize::from(self.line_last_was_cr));
                if !self.line_too_long && content_len <= self.max_line {
                    let content = &self.line[..content_len];
                    if self.body_start.is_none() && content == self.opened {
                        self.body_start = Some(at.saturating_add(1));
                    } else if self.body_start.is_some()
                        && self.completion.is_none()
                        && let Some(exit_status) = completion_status(content, &self.closed)
                    {
                        self.completion = Some(Completion {
                            body_end: self.line_boundary.unwrap_or(self.line_start),
                            exit_status,
                        });
                    }
                }
                let boundary = at.saturating_sub(usize::from(self.line_last_was_cr));
                self.line.clear();
                self.line_too_long = false;
                self.line_last_was_cr = false;
                self.line_start = at.saturating_add(1);
                self.line_boundary = Some(boundary);
            } else {
                self.line_last_was_cr = byte == b'\r';
                if !self.line_too_long {
                    if self.line.len() <= self.max_line {
                        self.line.push(byte);
                    } else {
                        self.line.clear();
                        self.line_too_long = true;
                    }
                }
            }
        }
    }

    fn body_range(&self) -> Option<Range<usize>> {
        let body_start = self.body_start?;
        let retained = self.collected.len();
        let body_end = self.completion.map_or_else(
            || self.retained_from.saturating_add(retained),
            |completion| completion.body_end,
        );
        let from = body_start.saturating_sub(self.retained_from).min(retained);
        let to = body_end.saturating_sub(self.retained_from).min(retained);
        Some(from.min(to)..to)
    }
}

#[derive(Clone, Copy)]
struct Completion {
    body_end: usize,
    exit_status: i32,
}

fn completion_status(line: &[u8], closed: &[u8]) -> Option<i32> {
    let digits = line.strip_prefix(closed)?;
    if digits.is_empty() || digits.len() > 3 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let status = std::str::from_utf8(digits).ok()?.parse::<u8>().ok()?;
    (status.to_string().as_bytes() == digits).then_some(i32::from(status))
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
