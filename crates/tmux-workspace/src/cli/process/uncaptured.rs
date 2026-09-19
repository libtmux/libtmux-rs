use std::{ffi::OsString, path::Path};

use super::{ChildOutput, CliError, Reporter, Result};

pub(in crate::cli) fn require_support() -> Result<()> {
    Err(unsupported())
}

pub(in crate::cli) async fn run(
    _argv: &[OsString],
    _directory: &Path,
    _report: &mut Reporter,
    _input_index: Option<usize>,
    _envs: &[(&str, &str)],
) -> Result<ChildOutput> {
    Err(unsupported())
}

fn unsupported() -> CliError {
    CliError::new(
        "unsupported_child_process",
        "captured child commands are unavailable on this target because the native bindings lack a safe non-reaping process observer; choose a command without captured children",
    )
}
