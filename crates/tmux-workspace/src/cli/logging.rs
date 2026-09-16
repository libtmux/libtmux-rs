use std::{fs::File, io::Write, path::Path};

use rustix::fs::{FileType, Mode, OFlags};
use serde_json::{Value, json};

use super::{CliError, Result};

pub(super) struct Logger {
    level: u8,
    file: Option<File>,
    failure: Option<String>,
    sequence: u64,
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
    script_bytes: [usize; 2],
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
    script_truncated: [bool; 2],
}

fn rank(level: &str) -> u8 {
    match level {
        "debug" => 0,
        "info" => 1,
        "error" => 3,
        "critical" => 4,
        _ => 2,
    }
}

fn metadata(value: &Value) -> Value {
    match value {
        Value::Object(fields) => fields
            .iter()
            .filter(|(name, _)| *name != "script_output")
            .map(|(name, value)| (name.clone(), metadata(value)))
            .collect(),
        Value::Array(values) => values.iter().map(metadata).collect(),
        _ => value.clone(),
    }
}

impl Logger {
    pub(super) fn new(level: &str) -> Self {
        Self {
            level: rank(level),
            file: None,
            failure: None,
            sequence: 0,
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
            script_bytes: [0; 2],
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
            script_truncated: [false; 2],
        }
    }

    pub(super) fn open(&mut self, path: &Path) -> Result<()> {
        let unavailable = |error: std::io::Error| CliError::new("log_file", error.to_string());
        match std::fs::symlink_metadata(path) {
            Ok(info) if info.file_type().is_file() => {}
            Ok(_) => {
                return Err(CliError::new(
                    "log_file",
                    "log destination must be a regular file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(unavailable(error)),
        }
        let file = rustix::fs::open(
            path,
            OFlags::WRONLY
                | OFlags::APPEND
                | OFlags::CREATE
                | OFlags::CLOEXEC
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::NOCTTY,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| unavailable(error.into()))?;
        let stat = rustix::fs::fstat(&file).map_err(|error| unavailable(error.into()))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(CliError::new(
                "log_file",
                "opened log destination is not a regular file",
            ));
        }
        self.file = Some(file.into());
        Ok(())
    }

    fn enabled(&self, severity: &str) -> bool {
        self.file.is_some() && rank(severity) >= self.level
    }

    fn write(&mut self, command: &str, event: &str, severity: &str, data: Value) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        self.sequence += 1;
        let mut record = json!({"schema_version":1,"command":command,"event":event,"sequence":self.sequence,"severity":severity});
        record["data"] = data;
        let mut bytes = record.to_string().into_bytes();
        bytes.push(b'\n');
        if let Err(error) = file.write_all(&bytes) {
            self.failure = Some(error.to_string());
            self.file = None;
        }
    }

    pub(super) fn event(&mut self, command: &str, event: &str, data: &Value) {
        let severity = match event {
            "script-output" => return,
            "failed" => "error",
            "warning" => "warning",
            _ => "info",
        };
        if self.enabled(severity) {
            self.write(command, event, severity, metadata(data));
        }
    }

    pub(super) fn error(&mut self, command: &str, error: &CliError) {
        if self.enabled("error") {
            self.write(
                command,
                "diagnostic",
                "error",
                json!({"code":error.code,"message":error.message}),
            );
        }
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
    pub(super) fn begin_child(&mut self) {
        self.script_bytes = [0; 2];
        self.script_truncated = [false; 2];
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
    pub(super) fn chunk(&mut self, command: &str, stream: &str, text: &str) {
        let index = usize::from(stream == "stderr");
        if !self.enabled("debug") || self.script_truncated[index] || text.is_empty() {
            return;
        }
        let mut count = text
            .len()
            .min(super::process::CAPTURE_LIMIT - self.script_bytes[index]);
        while !text.is_char_boundary(count) {
            count -= 1;
        }
        self.script_bytes[index] += count;
        self.script_truncated[index] = count < text.len();
        self.write(command, "script-output", "debug", json!({"stream":stream,"text":&text[..count],"truncated":self.script_truncated[index],"encoding":"utf-8-with-replacement"}));
    }

    pub(super) fn warning(&mut self) -> Option<String> {
        let failure = self.failure.take()?;
        (self.level <= rank("warning")).then(|| format!("log file disabled: {failure}"))
    }
}
