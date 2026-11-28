//! Install a fixture script and wait for concurrent forked writers to close.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

pub(crate) fn install(path: &Path, script: &str, timeout: Duration) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, Write as _};

    let Some((shebang, body)) = script
        .split_once('\n')
        .filter(|(first, _)| first.starts_with("#!"))
    else {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "an executable script starts with a #! line",
        ));
    };
    let staged = path.with_extension("staged");
    let mut file = fs::File::create(&staged)?;
    write!(
        file,
        "{shebang}\nif [ -n \"${{LIBTMUX_EXECUTABLE_READY:-}}\" ]; then exit 0; fi\n{body}"
    )?;
    file.sync_all()?;
    drop(file);
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o700))?;
    fs::rename(&staged, path)?;
    run_until_ready(path, Instant::now() + timeout)
}

/// Run `path` with `LIBTMUX_EXECUTABLE_READY` set until it can be executed,
/// or `deadline` passes while it is still busy.
pub(crate) fn run_until_ready(path: &Path, deadline: Instant) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    loop {
        let run = ProcessCommand::new(path)
            .env("LIBTMUX_EXECUTABLE_READY", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match run {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(Error::other(format!(
                    "{} exited with {status} on its readiness run",
                    path.display()
                )));
            }
            Err(error) if error.kind() == ErrorKind::ExecutableFileBusy => {
                if Instant::now() >= deadline {
                    return Err(Error::new(
                        ErrorKind::TimedOut,
                        format!("{} stayed busy: {error}", path.display()),
                    ));
                }
                std::thread::yield_now();
            }
            Err(error) => return Err(error),
        }
    }
}
