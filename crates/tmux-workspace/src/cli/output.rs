use std::io::{self, IsTerminal, Write};

use clap::ArgMatches;
use serde_json::{Value, json};

use super::Result;

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
        let policy = matches
            .get_one::<String>("color")
            .map_or("auto", String::as_str);
        let nonempty = |name| std::env::var(name).is_ok_and(|value| !value.is_empty());
        let color = mode == Mode::Human
            && !nonempty("NO_COLOR")
            && policy != "never"
            && (policy == "always"
                || nonempty("FORCE_COLOR")
                || std::env::var("CLICOLOR_FORCE").is_ok_and(|v| !v.is_empty() && v != "0")
                || (std::env::var("CLICOLOR").as_deref() != Ok("0") && io::stdout().is_terminal()));
        Self {
            mode,
            color,
            sequence: 0,
            terminal: false,
            command: command.into(),
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
