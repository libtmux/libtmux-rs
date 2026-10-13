//! Concrete tmux handles backed by Tokio, sharing values with `libtmux`.

use std::{ffi::OsString, fmt, io, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

pub use libtmux::{CapturePane, DecodeError, PaneId, PaneSnapshot, TmuxText};

const DEADLINE: Duration = Duration::from_secs(5);
const OUTPUT_LIMIT: u64 = 2 * 1024 * 1024;

/// A failure to execute or decode a tmux request.
#[derive(Debug)]
pub enum Error {
    /// The executable or one of its pipes failed.
    Io(io::Error),
    /// The command exceeded its five-second deadline.
    Timeout,
    /// A pipe exceeded the output bound.
    OutputLimit,
    /// tmux rejected the command. Stderr retains its original bytes.
    Rejected(TmuxText),
    /// The listing did not match the core's format.
    Decode(DecodeError),
    /// Killing or waiting for a failed client also failed.
    Cleanup {
        /// The command failure that triggered cleanup.
        command: Box<Error>,
        /// The cleanup failure.
        source: io::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "tmux I/O failed: {error}"),
            Self::Timeout => formatter.write_str("tmux exceeded the five-second deadline"),
            Self::OutputLimit => formatter.write_str("tmux output exceeded 2 MiB"),
            Self::Rejected(stderr) => write!(
                formatter,
                "tmux rejected the request: {}",
                stderr.to_string_lossy()
            ),
            Self::Decode(error) => error.fmt(formatter),
            Self::Cleanup { command, source } => {
                write!(formatter, "{command}; cleanup failed: {source}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) | Self::Cleanup { source, .. } => Some(source),
            Self::Decode(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Connection {
    socket: PathBuf,
    program: OsString,
}

/// A connection to one existing tmux server.
#[derive(Clone, Debug)]
pub struct Server(Arc<Connection>);

impl Server {
    /// Use the `tmux` executable on `PATH` and an explicit socket.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self(Arc::new(Connection {
            socket: socket.into(),
            program: "tmux".into(),
        }))
    }

    /// Use a particular tmux executable.
    #[must_use]
    pub fn program(self, program: impl Into<OsString>) -> Self {
        Self(Arc::new(Connection {
            socket: self.0.socket.clone(),
            program: program.into(),
        }))
    }

    /// List panes as concrete handles with detached core snapshots.
    ///
    /// # Errors
    /// Returns an execution error or [`Error::Decode`] for a malformed listing.
    pub async fn panes(&self) -> Result<Vec<Pane>, Error> {
        let bytes = self.run(&libtmux::LIST_PANES).await?;
        let snapshots = libtmux::decode_panes(&bytes).map_err(Error::Decode)?;
        Ok(snapshots
            .into_iter()
            .map(|snapshot| Pane {
                server: self.clone(),
                snapshot,
            })
            .collect())
    }

    /// Execute a request constructed by the standard-library core.
    ///
    /// # Errors
    /// Returns an error when spawning, reading, waiting, or the command fails.
    pub async fn capture(&self, request: &CapturePane) -> Result<TmuxText, Error> {
        self.run(&request.args()).await.map(TmuxText::from_bytes)
    }

    async fn run(&self, args: &[impl AsRef<std::ffi::OsStr>]) -> Result<Vec<u8>, Error> {
        let mut child = Command::new(&self.0.program)
            .arg("-S")
            .arg(&self.0.socket)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::Io)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Io(io::Error::other("missing tmux stdout pipe")))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Io(io::Error::other("missing tmux stderr pipe")))?;
        let result = timeout(DEADLINE, async {
            tokio::try_join!(read_output(stdout), read_output(stderr), async {
                child.wait().await.map_err(Error::Io)
            },)
        })
        .await
        .unwrap_or(Err(Error::Timeout));
        match result {
            Ok((stdout, _, status)) if status.success() => Ok(stdout),
            Ok((_, stderr, _)) => Err(Error::Rejected(TmuxText::from_bytes(stderr))),
            Err(command) => {
                if let Err(source) = child.kill().await {
                    return Err(Error::Cleanup {
                        command: Box::new(command),
                        source,
                    });
                }
                Err(command)
            }
        }
    }
}

async fn read_output(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    reader
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(Error::Io)?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(Error::OutputLimit);
    }
    Ok(bytes)
}

/// A live pane handle; its stored data is a value from the core crate.
#[derive(Clone, Debug)]
pub struct Pane {
    server: Server,
    snapshot: PaneSnapshot,
}

impl Pane {
    /// Borrow the last fetched snapshot without doing I/O.
    pub const fn snapshot(&self) -> &PaneSnapshot {
        &self.snapshot
    }

    /// Capture the visible screen as bytes.
    ///
    /// # Errors
    /// Returns the same execution errors as [`Server::capture`].
    pub async fn capture(&self) -> Result<TmuxText, Error> {
        self.server
            .capture(&CapturePane::new(self.snapshot.id.clone()))
            .await
    }
}
