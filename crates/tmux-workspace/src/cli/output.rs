use std::io::{self, IsTerminal, Write};

use clap::ArgMatches;
use serde_json::{Value, json};

use super::{Result, logging::Logger, progress::Progress};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Human,
    Json,
    Ndjson,
}

pub(super) struct Reporter {
    pub(super) mode: Mode,
    color: bool,
    sequence: u64,
    terminal: bool,
    command: String,
    pub(super) log: Logger,
    pub(super) progress: Option<Progress>,
}

pub(super) fn color_enabled(matches: &ArgMatches, terminal: bool) -> bool {
    let policy = matches
        .get_one::<String>("color")
        .map_or("auto", String::as_str);
    let nonempty = |name| std::env::var(name).is_ok_and(|value| !value.is_empty());
    !nonempty("NO_COLOR")
        && policy != "never"
        && (policy == "always"
            || nonempty("FORCE_COLOR")
            || std::env::var("CLICOLOR_FORCE").is_ok_and(|v| !v.is_empty() && v != "0")
            || (std::env::var("CLICOLOR").as_deref() != Ok("0") && terminal))
}

impl Reporter {
    pub(super) fn new(matches: &ArgMatches, command: &str) -> Self {
        let mode = if matches.get_flag("ndjson") {
            Mode::Ndjson
        } else if matches.get_flag("json") {
            Mode::Json
        } else {
            Mode::Human
        };
        let color = mode == Mode::Human && color_enabled(matches, io::stdout().is_terminal());
        Self {
            mode,
            color,
            sequence: 0,
            terminal: false,
            command: command.into(),
            progress: None,
            log: Logger::new(
                matches
                    .get_one::<String>("log-level")
                    .map_or("warning", String::as_str),
            ),
        }
    }

    pub(super) fn machine(&self) -> bool {
        self.mode != Mode::Human
    }

    pub(super) fn document(&self, value: &Value) -> Result<()> {
        let mut output = io::stdout().lock();
        if self.mode == Mode::Human {
            serde_json::to_writer_pretty(&mut output, value)?;
        } else {
            serde_json::to_writer(&mut output, value)?;
        }
        writeln!(output)?;
        output.flush()?;
        Ok(())
    }

    pub(super) fn event(&mut self, name: &str, data: Value) -> Result<()> {
        self.log.event(&self.command, name, &data);
        self.publish_event(name, data)
    }

    fn publish_event(&mut self, name: &str, data: Value) -> Result<()> {
        self.terminal |= matches!(name, "completed" | "failed");
        if self.mode == Mode::Ndjson {
            self.sequence += 1;
            let mut event = json!({"schema_version":1,"command":self.command,"event":name,"sequence":self.sequence});
            if let (Some(target), Value::Object(source)) = (event.as_object_mut(), data) {
                target.extend(source);
            }
            self.document(&event)?;
        }
        Ok(())
    }

    pub(super) fn summary(&mut self, event: &str, summary: &Value) -> Result<()> {
        self.clear_progress()?;
        self.log.event(&self.command, event, summary);
        if self.mode == Mode::Json {
            self.document(summary)?;
        }
        self.publish_event(
            event,
            if self.mode == Mode::Ndjson {
                summary.clone()
            } else {
                Value::Null
            },
        )
    }

    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    pub(super) fn log_chunk(&mut self, stream: &str, text: &str) {
        self.log.chunk(&self.command, stream, text);
    }

    pub(super) fn clear_progress(&mut self) -> Result<()> {
        if let Some(progress) = &mut self.progress {
            progress.clear()?;
        }
        Ok(())
    }

    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    pub(super) fn progress_output(&mut self, stream: &str, text: &str) -> Result<bool> {
        self.progress
            .as_mut()
            .map_or(Ok(false), |progress| progress.output(stream, text))
    }

    pub(super) fn log_error(&mut self, error: &super::CliError) {
        self.log.error(&self.command, error);
    }

    pub(super) fn log_warning(&mut self) {
        if let Some(message) = self.log.warning() {
            let mut errors = io::stderr().lock();
            if self.machine() {
                let _ = writeln!(
                    errors,
                    "{}",
                    json!({"schema_version":1,"code":"log_file_failed","severity":"warning","message":message})
                );
            } else {
                let _ = writeln!(errors, "Warning: {message}");
            }
            let _ = errors.flush();
        }
    }

    pub(super) fn failed(&mut self, error: &super::CliError) -> Result<()> {
        if !self.terminal && (self.sequence > 0 || error.status == 130) {
            self.event(
                "failed",
                json!({"status":"error", "errors":[{"code":error.code,"message":error.message}]}),
            )?;
        }
        Ok(())
    }

    pub(super) fn records(
        &self,
        records: &[Value],
        listing: bool,
        directories: &[Value],
        tree: bool,
    ) -> Result<()> {
        match self.mode {
            Mode::Json => self.document(&if listing {
                json!({"workspaces":records,"global_workspace_dirs":directories})
            } else {
                json!(records)
            }),
            Mode::Ndjson => {
                for record in records {
                    self.document(record)?;
                }
                Ok(())
            }
            Mode::Human => {
                if tree {
                    self.line(
                        "heading",
                        "Workspaces",
                        &format!("{} configured directories", directories.len()),
                    )?;
                }
                for record in records {
                    self.line(
                        "subject",
                        record["name"].as_str().unwrap_or("workspace"),
                        record["path"].as_str().unwrap_or(""),
                    )?;
                    if let Some(config) = record.get("config") {
                        write!(io::stdout(), "{}", super::document::encode(config, "yaml")?)?;
                    }
                }
                Ok(())
            }
        }
    }

    pub(super) fn loaded(&self, results: &Value) -> Result<()> {
        for result in results.as_array().into_iter().flatten() {
            self.line(
                "success",
                if result["reused"] == true {
                    "Reused"
                } else {
                    "Loaded"
                },
                result["session_name"].as_str().unwrap_or(""),
            )?;
        }
        Ok(())
    }

    pub(super) fn line(&self, role: &str, subject: &str, detail: &str) -> Result<()> {
        let code = match role {
            "heading" => "1;96",
            "subject" => "1;35",
            "success" => "32",
            "warning" => "33",
            "error" => "31",
            _ => "36",
        };
        if self.color {
            writeln!(
                io::stdout(),
                "\x1b[{code}m{subject}\x1b[0m  \x1b[36m{detail}\x1b[0m"
            )?;
        } else {
            writeln!(io::stdout(), "{subject}  {detail}")?;
        }
        Ok(())
    }
}
