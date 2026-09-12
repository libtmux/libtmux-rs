use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use clap::ArgMatches;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use super::{
    CliError, Result, discovery,
    output::{Mode, Reporter},
};

const CAPTURE_LIMIT: usize = 1024 * 1024;

struct ChildGroup(Option<rustix::process::Pid>);

impl ChildGroup {
    fn terminate(&mut self) {
        if let Some(pid) = self.0.take() {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub(super) struct ChildOutput {
    pub(super) status: i32,
    stdout: String,
    stderr: String,
    truncated: bool,
    terminal: bool,
}

impl ChildOutput {
    pub(super) fn value(&self) -> Value {
        json!({"child_status":self.status,"stdout":self.stdout,"stderr":self.stderr,"truncated":self.truncated,"encoding":"utf-8-with-replacement","terminal":self.terminal})
    }
    pub(super) fn success(&self) -> Result<()> {
        if self.status == 0 {
            Ok(())
        } else {
            Err(CliError {
                code: "child_failed",
                message: format!("child process exited with status {}", self.status),
                status: u8::try_from(self.status).unwrap_or(1),
            })
        }
    }
}

pub(super) fn split(command: &str) -> Result<Vec<OsString>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for ch in command.chars() {
        if escaped {
            if quote == Some('"') && ch != '"' && ch != '\\' {
                current.push('\\');
            }
            current.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
            continue;
        }
        if let Some(end) = quote {
            if ch == end {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started {
                args.push(OsString::from(std::mem::take(&mut current)));
                started = false;
            }
        } else {
            current.push(ch);
            started = true;
        }
    }
    if quote.is_some() || escaped {
        return Err(CliError::usage("unclosed quote or escape in child command"));
    }
    if started {
        args.push(current.into());
    }
    if args.is_empty() {
        return Err(CliError::usage("child command is empty"));
    }
    Ok(args)
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
    if retained.len() + text.len() <= CAPTURE_LIMIT {
        retained.push_str(text);
    } else {
        *truncated = true;
    }
    if report.machine() {
        report.event(
            "script-output",
            json!({"stream":stream,"text":text,"encoding":"utf-8-with-replacement"}),
        )?;
    } else if stream == "stderr" {
        write!(io::stderr(), "{text}")?;
        io::stderr().flush()?;
    } else {
        write!(io::stdout(), "{text}")?;
        io::stdout().flush()?;
    }
    Ok(())
}

pub(super) async fn run(
    argv: &[OsString],
    directory: &Path,
    report: &mut Reporter,
) -> Result<ChildOutput> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| CliError::usage("child executable is missing"))?;
    let grouped = report.machine() || !io::stdin().is_terminal();
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
        .kill_on_drop(true);
    if grouped {
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let mut group = ChildGroup(if grouped {
        child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
    } else {
        None
    });
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
            result = child.wait(), if status.is_none() => {
                status = Some(result?);
                drain_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
            }
            () = tokio::time::sleep_until(drain_deadline), if status.is_some() && (!out_done || !err_done) => {
                group.terminate();
                output.truncated = true;
                break;
            }
        }
    }
    output.status = status.map_or(1, exit_status);
    if output.status == 0 {
        group.0 = None;
    }
    Ok(output)
}

fn exit_status(status: std::process::ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
}

fn terminal() -> Result<Option<std::fs::File>> {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(terminal) => Ok(Some(terminal)),
        Err(error) if matches!(error.raw_os_error(), Some(2 | 6 | 25)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn run_terminal(argv: &[OsString], terminal: std::fs::File) -> Result<ChildOutput> {
    let status = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(terminal.try_clone()?)
        .stdout(terminal.try_clone()?)
        .stderr(terminal)
        .kill_on_drop(true)
        .status()
        .await?;
    Ok(ChildOutput {
        status: exit_status(status),
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        terminal: true,
    })
}

pub(super) async fn python() -> Result<OsString> {
    let python = std::env::var_os("TMUX_WORKSPACE_PYTHON").unwrap_or_else(|| "python3".into());
    let output = tokio::process::Command::new(&python)
        .args([
            "-c",
            "import importlib.metadata; print(importlib.metadata.version('tmuxp'))",
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| {
            CliError::new(
                "python_runtime",
                format!(
                    "tmuxp 1.74.0 Python bridge is unavailable: {e}; set TMUX_WORKSPACE_PYTHON"
                ),
            )
        })?;
    if !output.status.success() || output.stdout.as_slice().trim_ascii() != b"1.74.0" {
        return Err(CliError::new(
            "python_runtime",
            "Python bridge requires tmuxp 1.74.0; install that version and set TMUX_WORKSPACE_PYTHON to its Python executable",
        ));
    }
    Ok(python)
}

pub(super) async fn shell(options: &ArgMatches, report: &mut Reporter) -> Result<()> {
    let code = options.get_one::<String>("python-code");
    let terminal = if code.is_none() {
        Some(terminal()?.ok_or_else(|| CliError::usage("interactive Python shell requires a controlling terminal; use -c for captured execution"))?)
    } else {
        None
    };
    let python = python().await?;
    let mut argv: Vec<OsString> = vec![
        python,
        "-c".into(),
        "from tmuxp.cli import cli; cli()".into(),
        "--color".into(),
        "never".into(),
        "shell".into(),
    ];
    for (name, flag) in [
        ("socket-path", "-S"),
        ("socket-name", "-L"),
        ("python-code", "-c"),
    ] {
        if let Some(value) = options.get_one::<String>(name) {
            argv.extend([flag.into(), value.into()]);
        }
    }
    for name in [
        "best",
        "pdb",
        "code",
        "ptipython",
        "ptpython",
        "ipython",
        "bpython",
        "use-pythonrc",
        "no-startup",
        "use-vi-mode",
        "no-vi-mode",
    ] {
        if options.get_flag(name) {
            argv.push(format!("--{name}").into());
        }
    }
    for name in ["session_name", "window_name"] {
        if let Some(value) = options.get_one::<String>(name) {
            argv.push(value.into());
        }
    }
    let output = if let Some(terminal) = terminal {
        run_terminal(&argv, terminal).await?
    } else {
        run(&argv, &std::env::current_dir()?, report).await?
    };
    if report.machine() {
        let mut value = output.value();
        value["schema_version"] = json!(1);
        value["command"] = json!("shell");
        value["status"] = json!(if output.status == 0 { "ok" } else { "error" });
        if report.mode == Mode::Ndjson {
            report.event(
                if output.status == 0 {
                    "completed"
                } else {
                    "failed"
                },
                value,
            )?;
        } else {
            report.document(&value)?;
        }
    }
    output.success()
}

pub(super) async fn edit(options: &ArgMatches, report: &mut Reporter) -> Result<()> {
    let source = options
        .get_one::<String>("workspace_file")
        .ok_or_else(|| CliError::usage("workspace file is required"))?;
    let source = discovery::resolve(source, None)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let mut argv = split(&editor)?;
    argv.push(source.clone().into_os_string());
    let output = if let Some(terminal) = terminal()? {
        run_terminal(&argv, terminal).await?
    } else {
        run(&argv, &std::env::current_dir()?, report).await?
    };
    if !report.machine() {
        return output.success();
    }
    let mut value = output.value();
    value["schema_version"] = json!(1);
    value["command"] = json!("edit");
    value["path"] = json!(discovery::masked(&source));
    value["status"] = json!(if output.status == 0 { "ok" } else { "error" });
    report.document(&value)?;
    output.success()
}

pub(super) async fn diagnostics(report: &Reporter) -> Result<()> {
    let tmux = std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
    let version = tokio::process::Command::new(tmux)
        .arg("-V")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()
        .filter(|o| o.status.success());
    let value = json!({"port":"rust","workspace_version":env!("CARGO_PKG_VERSION"),"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
        "tmux_available":version.is_some(),"tmux_version":version.map(|v| String::from_utf8_lossy(&v.stdout).trim().to_owned()),
        "cwd":discovery::masked(&std::env::current_dir()?),"home":"~","shell":std::env::var("SHELL").ok().map(|v| discovery::masked(&PathBuf::from(v))),
        "workspace_dirs":discovery::global_dirs().iter().map(|p| discovery::masked(p)).collect::<Vec<_>>(),"python_bridge_version":"1.74.0"});
    if report.machine() {
        report.document(&value)
    } else {
        report.line(
            "heading",
            "Workspace diagnostics",
            env!("CARGO_PKG_VERSION"),
        )?;
        for (name, value) in value.as_object().into_iter().flatten() {
            report.line("information", name, &super::document::scalar(value))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{decode, split};

    #[test]
    fn decoding_keeps_split_utf8_after_invalid_bytes() {
        let mut pending = Vec::new();
        let first = decode(&mut pending, &[0xff, 0xe9], false);
        let second = decode(&mut pending, &[0x9b, 0xaa], false);
        assert_eq!(first + &second, "�雪");
        assert_eq!(decode(&mut pending, &[0xf0, 0x9f], false), "");
        assert_eq!(decode(&mut pending, &[], true), "�");
    }

    #[test]
    fn child_words_preserve_backslashes_inside_double_quotes() -> Result<(), super::CliError> {
        let words = split(r#"printf "a\nb" '' 'literal\q'"#)?;
        assert_eq!(
            words,
            ["printf", "a\\nb", "", "literal\\q"].map(std::ffi::OsString::from)
        );
        assert!(split("printf '").is_err());
        Ok(())
    }
}
