use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    path::Path,
    process::Stdio,
};

use serde_json::json;
use tokio::io::AsyncReadExt;

use super::{CAPTURE_LIMIT, ChildOutput, CliError, Reporter, Result};

#[path = "terminal.rs"]
mod terminal;

#[allow(
    clippy::unnecessary_wraps,
    reason = "same capability check on targets without a safe observer"
)]
pub(in crate::cli) fn require_support() -> Result<()> {
    Ok(())
}

struct ChildGroup {
    pid: Option<rustix::process::Pid>,
    terminal: Option<terminal::Terminal>,
}

impl ChildGroup {
    fn terminate(&mut self) {
        if let Some(pid) = self.pid.take() {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }

    fn finish(&mut self, success: bool) -> Result<()> {
        if !success {
            self.terminate();
        }
        if let Some(terminal) = &mut self.terminal {
            terminal.restore()?;
        }
        self.pid = None;
        Ok(())
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn decode(pending: &mut Vec<u8>, bytes: &[u8], end: bool) -> String {
    pending.extend_from_slice(bytes);
    let mut decoded = String::new();
    let mut offset = 0;
    while offset < pending.len() {
        match std::str::from_utf8(&pending[offset..]) {
            Ok(text) => {
                decoded.push_str(text);
                offset = pending.len();
            }
            Err(error) => {
                let valid = offset + error.valid_up_to();
                decoded.push_str(&String::from_utf8_lossy(&pending[offset..valid]));
                offset = valid;
                if let Some(length) = error.error_len() {
                    decoded.push('�');
                    offset += length;
                } else if end {
                    decoded.push('�');
                    offset = pending.len();
                } else {
                    break;
                }
            }
        }
    }
    pending.drain(..offset);
    decoded
}

/// Append to a capture until the limit, then stop for good.
///
/// Resuming after a dropped chunk would join two runs of output that were
/// never adjacent, and one boolean cannot say where the gap is.
fn retain(retained: &mut String, text: &str, truncated: &mut bool) {
    if *truncated || retained.len() + text.len() > CAPTURE_LIMIT {
        *truncated = true;
        return;
    }
    retained.push_str(text);
}

fn chunk(
    report: &mut Reporter,
    stream: &str,
    text: &str,
    retained: &mut String,
    truncated: &mut bool,
) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    report.log_chunk(stream, text);
    retain(retained, text, truncated);
    if report.machine() {
        report.event(
            "script-output",
            json!({"stream":stream,"text":text,"encoding":"utf-8-with-replacement"}),
        )?;
    } else if report.progress_output(stream, text)? {
        return Ok(());
    } else if stream == "stderr" {
        write!(io::stderr(), "{text}")?;
        io::stderr().flush()?;
    } else {
        write!(io::stdout(), "{text}")?;
        io::stdout().flush()?;
    }
    Ok(())
}

pub(in crate::cli) async fn run(
    argv: &[OsString],
    directory: &Path,
    report: &mut Reporter,
) -> Result<ChildOutput> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| CliError::usage("child executable is missing"))?;
    report.log.begin_child();
    let terminal = if !report.machine() && io::stdin().is_terminal() {
        terminal::Terminal::stdin()?
    } else {
        None
    };
    let mut child_signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(if report.machine() {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command.spawn()?;
    let pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| CliError::new("child_identity", "child process identity is unavailable"))?;
    let mut group = ChildGroup {
        pid: Some(pid),
        terminal,
    };
    if let Some(terminal) = &mut group.terminal {
        terminal.handoff(pid)?;
    }
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::new("child_stream", "child stdout is unavailable"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliError::new("child_stream", "child stderr is unavailable"))?;
    let mut output = ChildOutput {
        status: 0,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        terminal: false,
    };
    let mut out_buffer = vec![0; 8192];
    let mut err_buffer = vec![0; 8192];
    let (mut out_pending, mut err_pending) = (Vec::new(), Vec::new());
    let (mut out_done, mut err_done, mut status) = (false, false, None);
    let mut drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(86_400);
    while !out_done || !err_done || status.is_none() {
        tokio::select! {
            read = stdout.read(&mut out_buffer), if !out_done => {
                let size = read?; out_done = size == 0;
                let text = decode(&mut out_pending, &out_buffer[..size], out_done);
                chunk(report, "stdout", &text, &mut output.stdout, &mut output.truncated)?;
            }
            read = stderr.read(&mut err_buffer), if !err_done => {
                let size = read?; err_done = size == 0;
                let text = decode(&mut err_pending, &err_buffer[..size], err_done);
                chunk(report, "stderr", &text, &mut output.stderr, &mut output.truncated)?;
            }
            result = child_event(pid, &mut child_signal, group.terminal.is_some()), if status.is_none() => {
                match result? {
                    ChildEvent::Exited(code) => {
                        status = Some(code);
                        drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
                    }
                    ChildEvent::Stopped(signal) => {
                        if let Some(terminal) = &mut group.terminal {
                            terminal.stopped(signal)?;
                        }
                    }
                }
            }
            () = tokio::time::sleep_until(drain_deadline), if status.is_some() && (!out_done || !err_done) => {
                group.terminate();
                output.truncated = true;
                break;
            }
        }
    }
    output.status = status.unwrap_or(1);
    group.finish(output.status == 0)?;
    child.wait().await?;
    Ok(output)
}

enum ChildEvent {
    Exited(i32),
    Stopped(i32),
}

async fn child_event(
    pid: rustix::process::Pid,
    signal: &mut tokio::signal::unix::Signal,
    terminal: bool,
) -> Result<ChildEvent> {
    use rustix::process::{WaitId, WaitIdOptions, waitid};
    let mut options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    if terminal {
        options |= WaitIdOptions::STOPPED;
    }
    loop {
        match waitid(WaitId::Pid(pid), options) {
            Ok(Some(status)) => {
                if let Some(stopped) = status.stopping_signal() {
                    return Ok(ChildEvent::Stopped(stopped));
                }
                return Ok(ChildEvent::Exited(status.exit_status().unwrap_or_else(
                    || 128 + status.terminating_signal().unwrap_or(1),
                )));
            }
            Ok(None) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(io::Error::from(error).into()),
        }
        signal
            .recv()
            .await
            .ok_or_else(|| CliError::new("child_wait", "child exit notification stream closed"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::{CAPTURE_LIMIT, decode, retain};

    #[test]
    fn a_truncated_capture_stops_appending_rather_than_splicing() {
        let mut retained = "a".repeat(CAPTURE_LIMIT - 10);
        let mut truncated = false;
        retain(&mut retained, &"b".repeat(100), &mut truncated);
        assert!(truncated);
        retain(&mut retained, "tail", &mut truncated);
        assert_eq!(retained.len(), CAPTURE_LIMIT - 10);
        assert!(retained.ends_with('a'));
    }

    #[test]
    fn decoding_keeps_split_utf8_after_invalid_bytes() {
        let mut pending = Vec::new();
        let first = decode(&mut pending, &[0xff, 0xe9], false);
        let second = decode(&mut pending, &[0x9b, 0xaa], false);
        assert_eq!(first + &second, "�雪");
        assert_eq!(decode(&mut pending, &[0xf0, 0x9f], false), "");
        assert_eq!(decode(&mut pending, &[], true), "�");
    }
}
