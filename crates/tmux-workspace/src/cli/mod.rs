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
        Self::new("invalid_config", message)
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
        Self::new("tmux", error.to_string())
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
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::from)
        .and_then(|runtime| runtime.block_on(async {
            tokio::select! {
                result = execute(&matches, &mut report) => result,
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    Err(CliError { code: "interrupted", message: "operation interrupted".into(), status: 130, retained_state: None })
                }
            }
        }));
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

async fn execute(matches: &clap::ArgMatches, report: &mut Reporter) -> Result<()> {
    if let Some(format) = matches.get_one::<String>("generate") {
        if matches.subcommand().is_some() {
            return Err(CliError::usage(
                "--generate cannot be combined with a workspace command",
            ));
        }
        return generate::write(format);
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
        "load" => execution::load(options, report).await,
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
        Some(kind) => document::import(kind, &value)?,
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
        if !report.machine() && !options.get_flag("yes") {
            confirm(&format!("Save {}?", path.display()))?;
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

fn confirm(question: &str) -> Result<()> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() {
        return Err(CliError::new(
            "confirmation_required",
            "confirmation requires a terminal; supply --yes or explicit machine arguments",
        ));
    }
    write!(io::stderr(), "{question} [y/N] ")?;
    io::stderr().flush()?;
    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if matches!(response.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::new("cancelled", "operation declined"))
    }
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
        if let Some(state) = &error.retained_state {
            let _ = writeln!(io::stderr(), "Retained state: {state}");
        }
    }
}
