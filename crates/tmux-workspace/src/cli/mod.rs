//! The `tmux-workspace` command: a drop-in for tmuxp.
//!
//! This reads a larger document language than the library beside it, with
//! different defaults, and shares no parser or builder with it. The README
//! names every place the two disagree on the same file.

mod args;
mod bridge;
mod discovery;
mod document;
mod execution;
mod generate;
mod logging;
mod normalize;
mod output;
mod process;
mod progress;
mod search;

use std::io::{self, Write};
use std::process::ExitCode;

use output::{Mode, Reporter};
use serde_json::json;

type Result<T> = std::result::Result<T, CliError>;

/// The `code` on a terminal error record — a load, freeze, convert, import or
/// search failure, reported once and never retried — is drawn from a fixed
/// vocabulary shared across every port of this tool, or `interrupted` for a
/// mutation an actual `SIGINT`/`SIGTERM` cut off mid-flight (its own
/// contract: `outcome_unknown` plus `retained_state`, not a refusal about
/// what was asked):
///
/// - `workspace_not_found` — the named file or session was not found.
/// - `invalid_workspace` — the document, or something in it, is not valid.
/// - `unsupported_key` — the document names a key this port refuses.
/// - `session_not_found` — no session answers the name or selector given.
/// - `session_mismatch` — the named session exists and does not satisfy the
///   document: found, not missing, so `session_not_found` here would send a
///   consumer looking for a session that is right there.
/// - `tmux_unavailable` — no tmux executable, or its server is unreachable.
/// - `tmux_failed` — tmux rejected a command this port sent it.
/// - `script_failed` — a `before_script` is missing, not executable, or
///   exited nonzero.
/// - `destination_exists` — a write target is already there and nothing
///   said to replace it.
/// - `usage` — how the command was invoked is wrong, not what it names;
///   exit 2 rather than 1.
///
/// That agreement is scoped to this one field on a terminal error record.
/// Outside it, by design, and not required to collapse into the above:
/// - Warning codes (`unsupported_builder_option`, `start_directory_absent`)
///   ride the `warning` event, never a failure; a load that only warns still
///   completes.
/// - `python_runtime` names a checked Python runtime that will not run — a
///   fact about this machine, not about the document.
/// - The `child_*` family (`child_spawn`, `child_failed`, `child_identity`,
///   `child_stream`, `child_wait`) names which step of running a child
///   process under `shell`/`edit` failed, not why tmux did.
/// - `io`, `encoding`, `log_file` are this tool's own plumbing: a file it
///   could not read or write, JSON it could not decode, or a log sink that
///   would not open. A signal is `interrupted`, exit 130, the only spelling
///   this crate uses for it; a `cancelled` code once covered the confirm
///   prompt below declining, and that second spelling is retired.
///   Declining is not represented as a code at all now: it is a normal
///   outcome the caller reports as itself, exit 0, the same as answering no
///   to load's "already running, attach?".
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct CliError {
    code: &'static str,
    message: String,
    status: u8,
    retained_state: Option<serde_json::Value>,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            status: 1,
            retained_state: None,
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid_workspace", message)
    }
    fn unsupported_key(message: impl Into<String>) -> Self {
        Self::new("unsupported_key", message)
    }
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: "usage",
            message: message.into(),
            status: 2,
            retained_state: None,
        }
    }
}

impl From<io::Error> for CliError {
    fn from(error: io::Error) -> Self {
        Self::new("io", error.to_string())
    }
}
impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("encoding", error.to_string())
    }
}
impl From<libtmux::Error> for CliError {
    fn from(error: libtmux::Error) -> Self {
        // libtmux::ErrorKind is already the coarse category the crate
        // classifies every variant into "for reporting and routing"; reuse
        // it rather than re-deriving the same split from Error's variants.
        let code = match error.kind() {
            libtmux::ErrorKind::Unreachable | libtmux::ErrorKind::ServerGone => "tmux_unavailable",
            // Everything rejected before a command is sent was rejected for
            // what the document said: a layout name that is not one, an
            // option tmux keeps somewhere else. That is a defect in the
            // workspace, not a failure of tmux.
            libtmux::ErrorKind::InvalidInput => "invalid_workspace",
            _ => "tmux_failed",
        };
        Self::new(code, error.to_string())
    }
}

pub(super) fn main() -> ExitCode {
    let argv: Vec<_> = std::env::args_os().collect();
    let machine = argv
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json" || arg == "--ndjson");
    let matches = match args::command().try_get_matches_from(argv) {
        Ok(matches) => matches,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                return if write!(io::stdout(), "{error}").is_ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                };
            }
            diagnostic(machine, &CliError::usage(error.to_string()));
            return ExitCode::from(2);
        }
    };
    let mut report = Reporter::new(
        &matches,
        matches.subcommand_name().unwrap_or("tmux-workspace"),
    );
    let mut load_state = execution::LoadState::default();
    let mut result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::from)
        .and_then(|runtime| {
            runtime.block_on(interruptible(execute(
                &matches,
                &mut report,
                &mut load_state,
            )))
        });
    if let Err(error) = &mut result {
        if error.code == "interrupted" {
            load_state.interrupted(error, &mut report);
        }
    }
    let _ = report.clear_progress();
    match result {
        Ok(()) => {
            report.log_warning();
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = report.failed(&error);
            report.log_error(&error);
            diagnostic(machine, &error);
            report.log_warning();
            ExitCode::from(error.status)
        }
    }
}

async fn interruptible(future: impl Future<Output = Result<()>>) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = future => return result,
        _ = interrupt.recv() => {},
        _ = terminate.recv() => {},
    }
    Err(CliError {
        code: "interrupted",
        message: "operation interrupted".into(),
        status: 130,
        retained_state: None,
    })
}

async fn execute(
    matches: &clap::ArgMatches,
    report: &mut Reporter,
    load_state: &mut execution::LoadState,
) -> Result<()> {
    if let Some(format) = matches.get_one::<String>("generate") {
        if matches.subcommand().is_some() {
            return Err(CliError::usage(
                "--generate cannot be combined with a workspace command",
            ));
        }
        return generate::write(format, report);
    }
    let Some((name, options)) = matches.subcommand() else {
        return Err(CliError::usage("select a workspace command; use --help"));
    };
    match name {
        "ls" => {
            let records = discovery::records(options.get_flag("full"))?;
            let directories = discovery::global_metadata()?;
            report.records(&records, true, &directories, options.get_flag("tree"))
        }
        "search" => {
            let query = search::Query::new(options)?;
            let records = query.run(discovery::records(true)?)?;
            report.records(&records, false, &[], false)
        }
        "convert" => convert(options, None, report),
        "load" => execution::load(options, report, load_state).await,
        "freeze" => execution::freeze(options, report).await,
        "shell" => process::shell(options, report).await,
        "edit" => process::edit(options, report).await,
        "debug-info" => process::diagnostics(report).await,
        "import" => {
            let (kind, options) = options
                .subcommand()
                .ok_or_else(|| CliError::usage("choose an importer"))?;
            convert(options, Some(kind), report)
        }
        _ => Err(CliError::usage("unknown workspace command")),
    }
}

fn convert(options: &clap::ArgMatches, importer: Option<&str>, report: &Reporter) -> Result<()> {
    let source = options
        .get_one::<String>("workspace_file")
        .ok_or_else(|| CliError::usage("workspace source is required"))?;
    let source = discovery::resolve(source, importer)?;
    let value = document::read(&source)?;
    let value = match importer {
        Some(kind) => {
            let value = document::import(kind, &value, &source)?;
            normalize::workspace(&value, &source)?;
            value
        }
        None => value,
    };
    let format = options.get_one::<String>("workspace-format").map_or_else(
        || {
            if importer.is_none() && source.extension().is_some_and(|ext| ext != "json") {
                "json"
            } else {
                "yaml"
            }
        },
        String::as_str,
    );
    let destination = options
        .get_one::<String>("save-to")
        .map(std::path::PathBuf::from)
        .or_else(|| (!report.machine()).then(|| source.with_extension(format)));
    if let Some(path) = destination {
        // Declining is not cancellation: it is a normal outcome, exit 0,
        // that changed nothing, the same as answering no to load's "already
        // running, attach?" prompt.
        if !report.machine()
            && !options.get_flag("yes")
            && !confirm(&format!("Save {}?", path.display()))?
        {
            return report.line("warning", "Not saved", &discovery::masked(&path));
        }
        document::save(&path, &value, format, options.get_flag("force"))?;
        if report.machine() {
            report.document(&json!({"schema_version":1,"command":importer.map_or("convert".into(), |kind| format!("import {kind}")),"status":"ok","destination":discovery::masked(&path),"format":format}))
        } else {
            report.line("success", "Saved", &discovery::masked(&path))
        }
    } else if report.mode == Mode::Ndjson {
        report.document(&json!({"schema_version":1,"command":importer.map_or("convert".into(), |kind| format!("import {kind}")),"status":"ok","workspace":value}))
    } else {
        report.document(&value)
    }
}

/// Asks a yes/no question and reports the answer. A terminal is required to
/// ask at all; without one there is nothing to answer, so that is `usage`,
/// not a cancellation. Declining itself is not an error the caller reports:
/// it is a normal answer this returns as `Ok(false)` for the caller to act
/// on.
fn confirm(question: &str) -> Result<bool> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() {
        return Err(CliError::usage(
            "confirmation requires a terminal; supply --yes or explicit machine arguments",
        ));
    }
    write!(io::stderr(), "{question} [y/N] ")?;
    io::stderr().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    Ok(matches!(
        response.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn diagnostic(machine: bool, error: &CliError) {
    if machine {
        let mut value = json!({"schema_version":1,"code":error.code,"message":error.message});
        if let Some(state) = &error.retained_state {
            value["retained_state"] = state.clone();
        }
        let _ = writeln!(io::stderr(), "{value}");
    } else {
        let _ = writeln!(io::stderr(), "{}", error.message);
        // Interruption is the one case a human needs the machine record for:
        // it is how a caller recovers what an unfinished mutation touched.
        // An ordinary failure's message already says what happened; a second,
        // JSON-shaped copy of it is not for a human to read.
        if error.code == "interrupted" {
            if let Some(state) = &error.retained_state {
                let _ = writeln!(io::stderr(), "Retained state: {state}");
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::{process::Stdio, time::Duration};

    #[test]
    fn signal_handlers_precede_the_first_execution_poll() {
        for signal in ["SIGINT", "SIGTERM"] {
            for _ in 0..8 {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "cli::tests::first_poll_signal_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("LIBTMUX_TEST_STARTUP_SIGNAL", signal)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(3);
                while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if child.try_wait().unwrap().is_none() {
                    child.kill().unwrap();
                }
                let output = child.wait_with_output().unwrap();
                assert!(output.status.success(), "{signal}: {output:?}");
            }
        }
    }

    #[test]
    #[ignore = "subprocess fixture raises native signals at the first execution poll"]
    fn first_poll_signal_child() {
        let signal = match std::env::var("LIBTMUX_TEST_STARTUP_SIGNAL")
            .unwrap()
            .as_str()
        {
            "SIGINT" => Some(rustix::process::Signal::INT),
            "SIGTERM" => Some(rustix::process::Signal::TERM),
            _ => None,
        }
        .expect("signal selected by the parent fixture");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(super::interruptible(async {
            rustix::process::kill_process(rustix::process::getpid(), signal).unwrap();
            std::future::pending::<super::Result<()>>().await
        }));
        let error = result.unwrap_err();
        assert_eq!(error.code, "interrupted");
        assert_eq!(error.status, 130);
    }
}
