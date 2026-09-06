//! Bounded MCP initialize handshake for a prospective server definition.

#[cfg(target_os = "linux")]
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::Value;

use crate::config::ServerSpec;
use crate::fs::FsError;

/// Per-stream preflight output ceiling.
pub const PREFLIGHT_MAX_BYTES: usize = 1024 * 1024;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(5);
#[cfg(target_os = "linux")]
const OWNER_ENV: &str = "LIBTMUX_MCP_PREFLIGHT_OWNER";
#[cfg(target_os = "linux")]
static OWNER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const INITIALIZE: &str = concat!(
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"mcp-swap-preflight","version":"1"}}}"#,
    "\n"
);

/// Launch a server and require one MCP initialize result within a deadline.
///
/// The child starts in its own process group, and timeout kills the complete
/// group before waiting for output readers.
///
/// # Errors
///
/// Returns [`FsError`] for launch, input, timeout, oversize output, or a
/// process that exits without a matching JSON-RPC result.
pub fn preflight(spec: &ServerSpec, timeout: Duration) -> Result<(), FsError> {
    let containment = ProcessContainment::new();
    let mut command = Command::new(&spec.command);
    command
        .args(&spec.args)
        .envs(&spec.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    containment.configure(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| FsError::new(format!("could not launch {}: {error}", spec.command)))?;
    let Ok(raw_pid) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(FsError::new("server returned an out-of-range process ID"));
    };
    let Some(pid) = Pid::from_raw(raw_pid) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(FsError::new("server returned an invalid process ID"));
    };
    let Some(mut stdin) = child.stdin.take() else {
        return Err(clean_after_error(
            FsError::new("server stdin was not piped"),
            &mut child,
            pid,
            &containment,
        ));
    };
    if let Err(error) = stdin.write_all(INITIALIZE.as_bytes()) {
        return Err(clean_after_error(
            FsError::new(format!("write MCP initialize: {error}")),
            &mut child,
            pid,
            &containment,
        ));
    }
    drop(stdin);
    let Some(stdout) = child.stdout.take() else {
        return Err(clean_after_error(
            FsError::new("server stdout was not piped"),
            &mut child,
            pid,
            &containment,
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        return Err(clean_after_error(
            FsError::new("server stderr was not piped"),
            &mut child,
            pid,
            &containment,
        ));
    };
    if let Err(error) = set_nonblocking(&stdout, Stream::Stdout)
        .and_then(|()| set_nonblocking(&stderr, Stream::Stderr))
    {
        return Err(clean_after_error(error, &mut child, pid, &containment));
    }
    let (sender, receiver) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let stdout_reader = spawn_reader(
        stdout,
        Stream::Stdout,
        sender.clone(),
        Arc::clone(&cancelled),
    );
    let stderr_reader = spawn_reader(stderr, Stream::Stderr, sender, Arc::clone(&cancelled));
    let result = monitor(&mut child, &receiver, timeout);
    cancelled.store(true, Ordering::Release);
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    let cleanup = stop_process_tree(&mut child, pid, &containment, deadline);
    let readers = finish_readers([stdout_reader, stderr_reader], deadline);
    with_cleanup(with_cleanup(result, cleanup), readers)
}

fn clean_after_error(
    error: FsError,
    child: &mut Child,
    pid: Pid,
    containment: &ProcessContainment,
) -> FsError {
    match stop_process_tree(child, pid, containment, Instant::now() + CLEANUP_TIMEOUT) {
        Ok(()) => error,
        Err(cleanup) => FsError::new(format!("{error}; cleanup failed: {cleanup}")),
    }
}

fn with_cleanup(result: Result<(), FsError>, cleanup: Result<(), FsError>) -> Result<(), FsError> {
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup)) => {
            Err(FsError::new(format!("{error}; cleanup failed: {cleanup}")))
        }
    }
}

fn stop_process_tree(
    child: &mut Child,
    pid: Pid,
    containment: &ProcessContainment,
    deadline: Instant,
) -> Result<(), FsError> {
    let mut failures = Vec::new();
    if let Err(error) = kill_process_group(pid, Signal::KILL) {
        if error != Errno::SRCH {
            failures.push(format!("kill MCP process group: {error}"));
        }
    }
    let _ = child.kill();
    if let Err(error) = containment.terminate_all(deadline) {
        failures.push(error.to_string());
    }
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(CLEANUP_POLL_INTERVAL),
            Ok(None) => {
                failures.push("MCP direct child did not exit before cleanup deadline".into());
                break;
            }
            Err(error) => {
                failures.push(format!("reap MCP direct child: {error}"));
                break;
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(FsError::new(failures.join("; ")))
    }
}

struct ProcessContainment {
    #[cfg(target_os = "linux")]
    marker: OsString,
}

impl ProcessContainment {
    fn new() -> Self {
        #[cfg(target_os = "linux")]
        {
            let sequence = OWNER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            Self {
                marker: format!("{}-{sequence}", std::process::id()).into(),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self {}
        }
    }

    fn configure(&self, command: &mut Command) {
        #[cfg(target_os = "linux")]
        command.env(OWNER_ENV, &self.marker);
        #[cfg(not(target_os = "linux"))]
        let _ = command;
    }

    fn terminate_all(&self, deadline: Instant) -> Result<(), FsError> {
        #[cfg(target_os = "linux")]
        {
            linux::terminate_all(&self.marker, deadline)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = deadline;
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

enum StreamEvent {
    Chunk(Stream, Vec<u8>),
    Eof(Stream),
    Error(Stream, String),
    Oversize(Stream),
}

fn spawn_reader(
    mut reader: impl Read + Send + 'static,
    stream: Stream,
    sender: Sender<StreamEvent>,
    cancelled: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut total = 0;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let mut buffer = [0; 8192];
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(StreamEvent::Eof(stream));
                    return;
                }
                Ok(count) => {
                    total += count;
                    if total > PREFLIGHT_MAX_BYTES {
                        let _ = sender.send(StreamEvent::Oversize(stream));
                        return;
                    }
                    if sender
                        .send(StreamEvent::Chunk(stream, buffer[..count].to_vec()))
                        .is_err()
                    {
                        return;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(CLEANUP_POLL_INTERVAL);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    let _ = sender.send(StreamEvent::Error(stream, error.to_string()));
                    return;
                }
            }
        }
    })
}

fn set_nonblocking(reader: &impl AsFd, stream: Stream) -> Result<(), FsError> {
    let flags = fcntl_getfl(reader).map_err(|error| {
        FsError::new(format!("inspect MCP {} pipe: {error}", stream_name(stream)))
    })?;
    fcntl_setfl(reader, flags | OFlags::NONBLOCK).map_err(|error| {
        FsError::new(format!(
            "configure MCP {} pipe: {error}",
            stream_name(stream)
        ))
    })
}

fn finish_readers(readers: [thread::JoinHandle<()>; 2], deadline: Instant) -> Result<(), FsError> {
    let mut failures = Vec::new();
    for reader in readers {
        while !reader.is_finished() && Instant::now() < deadline {
            thread::sleep(CLEANUP_POLL_INTERVAL);
        }
        if !reader.is_finished() {
            failures.push("MCP output reader did not stop before cleanup deadline");
            continue;
        }
        if reader.join().is_err() {
            failures.push("MCP output reader panicked");
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(FsError::new(failures.join("; ")))
    }
}

fn monitor(
    child: &mut Child,
    receiver: &Receiver<StreamEvent>,
    timeout: Duration,
) -> Result<(), FsError> {
    let started = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut scanned = 0;
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut status: Option<ExitStatus> = None;
    loop {
        if started.elapsed() >= timeout {
            return Err(FsError::new(format!(
                "no MCP response within {}s",
                timeout.as_secs_f64()
            )));
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|error| FsError::new(format!("wait for MCP preflight: {error}")))?;
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(StreamEvent::Chunk(Stream::Stdout, chunk)) => {
                stdout.extend_from_slice(&chunk);
                while let Some(end) = stdout[scanned..].iter().position(|byte| *byte == b'\n') {
                    let end = scanned + end;
                    if parse_initialize_result(&stdout[scanned..end])? {
                        return Ok(());
                    }
                    scanned = end + 1;
                }
            }
            Ok(StreamEvent::Chunk(Stream::Stderr, chunk)) => stderr.extend_from_slice(&chunk),
            Ok(StreamEvent::Eof(Stream::Stdout)) => {
                stdout_eof = true;
                if scanned < stdout.len() && parse_initialize_result(&stdout[scanned..])? {
                    return Ok(());
                }
            }
            Ok(StreamEvent::Eof(Stream::Stderr)) => stderr_eof = true,
            Ok(StreamEvent::Error(stream, error)) => {
                return Err(FsError::new(format!(
                    "read MCP {}: {error}",
                    stream_name(stream)
                )));
            }
            Ok(StreamEvent::Oversize(stream)) => {
                return Err(FsError::new(format!(
                    "MCP {} exceeds {PREFLIGHT_MAX_BYTES} bytes",
                    stream_name(stream)
                )));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                stdout_eof = true;
                stderr_eof = true;
            }
        }
        if let Some(status) = status.filter(|_| stdout_eof && stderr_eof) {
            return no_response(status, &stderr);
        }
    }
}

fn parse_initialize_result(bytes: &[u8]) -> Result<bool, FsError> {
    let line = std::str::from_utf8(bytes)
        .map_err(|error| FsError::new(format!("MCP stdout is not UTF-8: {error}")))?;
    Ok(is_initialize_result(line))
}

fn no_response(status: ExitStatus, stderr: &[u8]) -> Result<(), FsError> {
    let stderr = String::from_utf8_lossy(stderr);
    let detail = stderr
        .lines()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if !detail.is_empty() {
        return Err(FsError::new(detail));
    }
    Err(FsError::new(format!(
        "server exited with {status} without answering initialize"
    )))
}

const fn stream_name(stream: Stream) -> &'static str {
    match stream {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
    }
}

fn is_initialize_result(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|message| {
        message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && message.get("id").and_then(Value::as_u64) == Some(1)
            && message
                .get("result")
                .and_then(|result| result.get("protocolVersion"))
                .and_then(Value::as_str)
                .is_some_and(|version| !version.is_empty())
    })
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Instant;

    use rustix::io::Errno;
    use rustix::process::{Pid, PidfdFlags, Signal, getuid, pidfd_open, pidfd_send_signal};

    use super::{CLEANUP_POLL_INTERVAL, OWNER_ENV};
    use crate::fs::FsError;

    #[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
    struct ProcessIdentity {
        pid: u32,
        uid: u32,
        start_time: u64,
    }

    #[derive(Clone, Copy)]
    struct ProcessSnapshot {
        identity: ProcessIdentity,
        state: u8,
    }

    struct MarkedProcess {
        identity: ProcessIdentity,
        pidfd: OwnedFd,
    }

    #[derive(Default)]
    struct FrozenProcesses(BTreeMap<ProcessIdentity, MarkedProcess>);

    impl Drop for FrozenProcesses {
        fn drop(&mut self) {
            for process in self.0.values() {
                let _ = pidfd_send_signal(&process.pidfd, Signal::KILL);
            }
        }
    }

    pub(super) fn terminate_all(marker: &OsStr, deadline: Instant) -> Result<(), FsError> {
        let mut frozen = FrozenProcesses::default();
        loop {
            let mut discovered = false;
            for process in scan(marker)? {
                if frozen.0.contains_key(&process.identity) {
                    continue;
                }
                if !signal_process(&process, Signal::STOP) {
                    return Err(FsError::new("could not freeze a marked MCP descendant"));
                }
                frozen.0.insert(process.identity, process);
                discovered = true;
            }
            if !discovered {
                break;
            }
            require_time(deadline)?;
            thread::sleep(CLEANUP_POLL_INTERVAL);
        }

        if !frozen
            .0
            .values()
            .all(|process| signal_process(process, Signal::KILL))
        {
            return Err(FsError::new("could not kill a marked MCP descendant"));
        }

        loop {
            let mut terminal = Vec::new();
            for (identity, process) in &frozen.0 {
                if process_is_terminal(process)? {
                    terminal.push(*identity);
                }
            }
            for identity in terminal {
                frozen.0.remove(&identity);
            }
            for process in scan(marker)? {
                if frozen.0.contains_key(&process.identity) {
                    continue;
                }
                if !signal_process(&process, Signal::STOP)
                    || !signal_process(&process, Signal::KILL)
                {
                    return Err(FsError::new("could not kill a late MCP descendant"));
                }
                frozen.0.insert(process.identity, process);
            }
            if frozen.0.is_empty() {
                return Ok(());
            }
            require_time(deadline)?;
            thread::sleep(CLEANUP_POLL_INTERVAL);
        }
    }

    fn scan(marker: &OsStr) -> Result<Vec<MarkedProcess>, FsError> {
        let expected = environment_entry(marker);
        let mut processes = Vec::new();
        for (pid, path) in process_paths()? {
            let Some(snapshot) = read_process(pid, &path) else {
                continue;
            };
            if snapshot.identity.uid != getuid().as_raw() || snapshot.state == b'Z' {
                continue;
            }
            if !environment_matches(&path, &expected).unwrap_or_default() {
                continue;
            }
            let Ok(numeric_pid) = i32::try_from(pid) else {
                continue;
            };
            let Some(rustix_pid) = Pid::from_raw(numeric_pid) else {
                continue;
            };
            let pidfd = match pidfd_open(rustix_pid, PidfdFlags::empty()) {
                Ok(pidfd) => pidfd,
                Err(Errno::SRCH) => continue,
                Err(error) => {
                    return Err(FsError::new(format!(
                        "open marked MCP descendant pidfd: {error}"
                    )));
                }
            };
            let Some(current) = read_process(pid, &path) else {
                continue;
            };
            if !same_live_process(snapshot.identity, current) {
                continue;
            }
            let Ok(marker_matches) = environment_matches(&path, &expected) else {
                let Some(current) = read_process(pid, &path) else {
                    continue;
                };
                if !same_live_process(snapshot.identity, current) {
                    continue;
                }
                return Err(FsError::new(
                    "marked MCP descendant became opaque during cleanup",
                ));
            };
            let Some(current) = read_process(pid, &path) else {
                continue;
            };
            if marker_matches && same_live_process(snapshot.identity, current) {
                processes.push(MarkedProcess {
                    identity: snapshot.identity,
                    pidfd,
                });
            }
        }
        Ok(processes)
    }

    fn process_paths() -> Result<Vec<(u32, PathBuf)>, FsError> {
        let mut paths = Vec::new();
        let entries = fs::read_dir("/proc")
            .map_err(|error| FsError::new(format!("scan Linux processes: {error}")))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| FsError::new(format!("scan Linux process entry: {error}")))?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            paths.push((pid, entry.path()));
        }
        Ok(paths)
    }

    fn environment_entry(marker: &OsStr) -> Vec<u8> {
        let mut expected = OWNER_ENV.as_bytes().to_vec();
        expected.push(b'=');
        expected.extend_from_slice(marker.as_bytes());
        expected
    }

    fn environment_matches(path: &Path, expected: &[u8]) -> std::io::Result<bool> {
        fs::read(path.join("environ")).map(|environment| {
            environment
                .split(|byte| *byte == 0)
                .any(|entry| entry == expected)
        })
    }

    fn read_process(pid: u32, path: &Path) -> Option<ProcessSnapshot> {
        let uid = read_uid(&path.join("status"))?;
        let (state, start_time) = read_stat(&path.join("stat"))?;
        Some(ProcessSnapshot {
            identity: ProcessIdentity {
                pid,
                uid,
                start_time,
            },
            state,
        })
    }

    fn read_uid(path: &Path) -> Option<u32> {
        let status = fs::read_to_string(path).ok()?;
        let values = status.lines().find_map(|line| line.strip_prefix("Uid:"))?;
        values.split_ascii_whitespace().next()?.parse().ok()
    }

    fn read_stat(path: &Path) -> Option<(u8, u64)> {
        let stat = fs::read(path).ok()?;
        let command_end = stat.iter().rposition(|byte| *byte == b')')?;
        let mut fields = stat
            .get(command_end + 1..)?
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty());
        let state = *fields.next()?.first()?;
        let start_time = fields.nth(18)?;
        Some((state, std::str::from_utf8(start_time).ok()?.parse().ok()?))
    }

    fn same_live_process(expected: ProcessIdentity, current: ProcessSnapshot) -> bool {
        current.identity == expected && current.state != b'Z'
    }

    fn signal_process(process: &MarkedProcess, signal: Signal) -> bool {
        matches!(
            pidfd_send_signal(&process.pidfd, signal),
            Ok(()) | Err(Errno::SRCH)
        )
    }

    fn process_is_terminal(process: &MarkedProcess) -> Result<bool, FsError> {
        let path = PathBuf::from(format!("/proc/{}", process.identity.pid));
        if read_process(process.identity.pid, &path)
            .is_some_and(|current| current.identity == process.identity && current.state == b'Z')
        {
            return Ok(true);
        }
        match pidfd_send_signal(&process.pidfd, Signal::KILL) {
            Ok(()) => Ok(false),
            Err(Errno::SRCH) => Ok(true),
            Err(error) => Err(FsError::new(format!(
                "signal marked MCP descendant: {error}"
            ))),
        }
    }

    fn require_time(deadline: Instant) -> Result<(), FsError> {
        if Instant::now() < deadline {
            Ok(())
        } else {
            Err(FsError::new(
                "marked MCP descendants outlived the cleanup deadline",
            ))
        }
    }
}
