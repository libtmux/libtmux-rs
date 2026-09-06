//! Bounded MCP initialize handshake for a prospective server definition.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::Value;

use crate::config::ServerSpec;
use crate::fs::FsError;

/// Per-stream preflight output ceiling.
pub const PREFLIGHT_MAX_BYTES: usize = 1024 * 1024;

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
    let mut command = Command::new(&spec.command);
    command
        .args(&spec.args)
        .envs(&spec.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
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
        stop_process_group(&mut child, pid);
        return Err(FsError::new("server stdin was not piped"));
    };
    if let Err(error) = stdin.write_all(INITIALIZE.as_bytes()) {
        stop_process_group(&mut child, pid);
        return Err(FsError::new(format!("write MCP initialize: {error}")));
    }
    drop(stdin);
    let Some(stdout) = child.stdout.take() else {
        stop_process_group(&mut child, pid);
        return Err(FsError::new("server stdout was not piped"));
    };
    let Some(stderr) = child.stderr.take() else {
        stop_process_group(&mut child, pid);
        return Err(FsError::new("server stderr was not piped"));
    };
    let (sender, receiver) = mpsc::channel();
    let stdout_reader = spawn_reader(stdout, Stream::Stdout, sender.clone());
    let stderr_reader = spawn_reader(stderr, Stream::Stderr, sender);
    let result = monitor(&mut child, &receiver, timeout);
    stop_process_group(&mut child, pid);
    let stdout_join = stdout_reader.join();
    let stderr_join = stderr_reader.join();
    if stdout_join.is_err() || stderr_join.is_err() {
        return Err(FsError::new("MCP output reader panicked"));
    }
    result
}

fn stop_process_group(child: &mut Child, pid: Pid) {
    let _ = kill_process_group(pid, Signal::KILL);
    let _ = child.kill();
    let _ = child.wait();
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
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut total = 0;
        loop {
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
                Err(error) => {
                    let _ = sender.send(StreamEvent::Error(stream, error.to_string()));
                    return;
                }
            }
        }
    })
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
