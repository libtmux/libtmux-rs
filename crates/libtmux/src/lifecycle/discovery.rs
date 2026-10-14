use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::PathBuf;
use std::time::Duration;

use crate::{Error, Server, ServerGeneration};

/// Limits and captured roots for a nonrecursive socket search.
///
/// Roots whose final component is a symlink and socket symlinks are skipped. Successful probes
/// deduplicate daemon generations; duplicate paths still produce diagnostics.
/// Results cover only the visited roots within these entry, probe and time
/// budgets. Discovery never starts a daemon. Filesystem enumeration runs in a
/// blocking worker; timing out stops awaiting it, and its entry cap still
/// bounds its work if the filesystem resumes.
#[derive(Clone, Debug)]
pub struct Discovery {
    /// Directories whose immediate children should be inspected.
    pub roots: Vec<PathBuf>,
    /// Maximum directory entries examined across all roots.
    pub max_entries: usize,
    /// Maximum socket candidates contacted.
    pub max_probes: usize,
    /// Maximum elapsed time for the complete search.
    pub timeout: Duration,
    /// Maximum duration of one no-start probe.
    pub probe_timeout: Duration,
    /// tmux executable used by the probes.
    pub executable: OsString,
}

/// A daemon that answered a no-start probe.
#[derive(Debug)]
pub struct DiscoveredServer {
    /// The endpoint that answered. This is a borrowed handle.
    pub server: Server,
    /// Daemon identity at probe time; later calls can observe a replacement.
    pub generation: ServerGeneration,
}

/// A candidate or root that discovery did not include.
#[derive(Debug)]
pub struct DiscoveryDiagnostic {
    /// Root or candidate path involved in this diagnostic.
    pub path: PathBuf,
    /// Why discovery skipped or could not inspect the path.
    pub reason: DiscoverySkip,
}

/// Distinguishes unreadable roots, stale sockets and excluded entries.
#[derive(Debug)]
pub enum DiscoverySkip {
    /// A filesystem operation failed.
    Io(std::io::Error),
    /// The path was a symlink, which discovery does not follow.
    Symlink,
    /// An entry was not a Unix socket, or a root was not a directory.
    WrongType,
    /// Another root already named this socket inode.
    DuplicatePath,
    /// Another endpoint already reported this daemon.
    DuplicateDaemon,
    /// The socket was present but tmux did not answer successfully.
    Probe(Error),
    /// The candidate exceeded its probe deadline.
    ProbeTimeout,
}

/// The budget that ended the search before it visited all candidates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryTruncation {
    /// The directory-entry budget ended.
    Entries,
    /// The socket-probe budget ended.
    Probes,
    /// The total elapsed-time budget ended.
    Time,
}

/// Successful borrowed handles, diagnostics and an explicit completeness bound.
#[derive(Debug, Default)]
pub struct DiscoveryReport {
    /// Daemons discovered before the bounds ended.
    pub servers: Vec<DiscoveredServer>,
    /// Per-root and per-candidate skipped/failed operations.
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    /// The first exhausted budget; `None` means the selected roots were visited.
    pub truncated: Option<DiscoveryTruncation>,
    /// Number of directory entries inspected.
    pub entries: usize,
    /// Number of socket candidates probed.
    pub probes: usize,
}

impl Discovery {
    /// Search the given directories with finite default budgets.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
            max_entries: 1024,
            max_probes: 64,
            timeout: Duration::from_secs(5),
            probe_timeout: Duration::from_millis(250),
            executable: OsString::from("tmux"),
        }
    }

    /// Capture the current user's configured socket roots and ordinary endpoint.
    ///
    /// Includes `/tmp/tmux-UID`, the captured nonempty `TMUX_TMPDIR/tmux-UID`
    /// and the selected endpoint's parent. Add explicit roots to search other
    /// directories; these defaults do not enumerate the whole machine.
    ///
    /// # Errors
    /// Returns the ordinary endpoint configuration error when selected inputs
    /// are invalid. This constructor performs no tmux command.
    pub fn configured() -> Result<Self, Error> {
        let server = Server::new()?;
        let uid = rustix::process::getuid().as_raw();
        let mut roots = vec![PathBuf::from(format!("/tmp/tmux-{uid}"))];
        if let Some(root) = std::env::var_os("TMUX_TMPDIR").filter(|s| !s.is_empty()) {
            roots.push(PathBuf::from(root).join(format!("tmux-{uid}")));
        }
        if let Some(root) = server.socket_path().parent() {
            roots.push(root.to_owned());
        }
        roots.sort();
        roots.dedup();
        Ok(Self::new(roots))
    }

    /// Probe bounded candidates and retain failures alongside successful results.
    pub async fn scan(&self) -> DiscoveryReport {
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut report = DiscoveryReport::default();
        let mut inodes = HashSet::new();
        let mut daemons = HashSet::new();
        for root in &self.roots {
            if tokio::time::Instant::now() >= deadline {
                report.truncated = Some(DiscoveryTruncation::Time);
                break;
            }
            let root = root.clone();
            let bound = self.max_entries.saturating_sub(report.entries);
            let scan_root = root.clone();
            let scan = tokio::task::spawn_blocking(move || read_root(scan_root, bound));
            let candidates = match tokio::time::timeout_at(deadline, scan).await {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        path: root,
                        reason: DiscoverySkip::Io(std::io::Error::other(error)),
                    });
                    continue;
                }
                Err(_) => {
                    report.truncated = Some(DiscoveryTruncation::Time);
                    break;
                }
            };
            report.entries += candidates.entries;
            report.diagnostics.extend(candidates.diagnostics);
            for (path, inode) in candidates.sockets {
                if !inodes.insert(inode) {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        path,
                        reason: DiscoverySkip::DuplicatePath,
                    });
                    continue;
                }
                if report.probes >= self.max_probes {
                    report.truncated = Some(DiscoveryTruncation::Probes);
                    return report;
                }
                if tokio::time::Instant::now() >= deadline {
                    report.truncated = Some(DiscoveryTruncation::Time);
                    return report;
                }
                report.probes += 1;
                let server = match Server::builder()
                    .socket_path(&path)
                    .tmux_executable(&self.executable)
                    .default_timeout(self.probe_timeout)
                    .build()
                {
                    Ok(server) => server,
                    Err(error) => {
                        report.diagnostics.push(DiscoveryDiagnostic {
                            path,
                            reason: DiscoverySkip::Probe(error),
                        });
                        continue;
                    }
                };
                let outcome = tokio::time::timeout_at(
                    deadline.min(tokio::time::Instant::now() + self.probe_timeout),
                    server.generation_no_start(),
                )
                .await;
                match outcome {
                    Ok(Ok(generation)) if daemons.insert(generation) => {
                        report.servers.push(DiscoveredServer { server, generation });
                    }
                    Ok(Ok(_)) => report.diagnostics.push(DiscoveryDiagnostic {
                        path,
                        reason: DiscoverySkip::DuplicateDaemon,
                    }),
                    Ok(Err(error)) => report.diagnostics.push(DiscoveryDiagnostic {
                        path,
                        reason: DiscoverySkip::Probe(error),
                    }),
                    Err(_) => report.diagnostics.push(DiscoveryDiagnostic {
                        path,
                        reason: DiscoverySkip::ProbeTimeout,
                    }),
                }
                if tokio::time::Instant::now() >= deadline {
                    report.truncated = Some(DiscoveryTruncation::Time);
                    return report;
                }
            }
            if candidates.truncated {
                report.truncated = Some(DiscoveryTruncation::Entries);
                break;
            }
        }
        report
    }
}

#[derive(Default)]
struct Candidates {
    sockets: Vec<(PathBuf, (u64, u64))>,
    diagnostics: Vec<DiscoveryDiagnostic>,
    entries: usize,
    truncated: bool,
}

fn read_root(root: PathBuf, bound: usize) -> Candidates {
    let mut result = Candidates::default();
    let entries = match std::fs::symlink_metadata(&root).and_then(|metadata| {
        if metadata.file_type().is_symlink() {
            result.diagnostics.push(DiscoveryDiagnostic {
                path: root.clone(),
                reason: DiscoverySkip::Symlink,
            });
            return Ok(None);
        }
        if !metadata.is_dir() {
            result.diagnostics.push(DiscoveryDiagnostic {
                path: root.clone(),
                reason: DiscoverySkip::WrongType,
            });
            return Ok(None);
        }
        std::fs::read_dir(&root).map(Some)
    }) {
        Ok(Some(entries)) => entries,
        Ok(None) => return result,
        Err(error) => {
            result.diagnostics.push(DiscoveryDiagnostic {
                path: root,
                reason: DiscoverySkip::Io(error),
            });
            return result;
        }
    };
    for entry in entries {
        if result.entries >= bound {
            result.truncated = true;
            break;
        }
        result.entries += 1;
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => {
                result.diagnostics.push(DiscoveryDiagnostic {
                    path: root.clone(),
                    reason: DiscoverySkip::Io(error),
                });
                continue;
            }
        };
        let reason = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => Some(DiscoverySkip::Symlink),
            Ok(metadata) if metadata.file_type().is_socket() => {
                result
                    .sockets
                    .push((path.clone(), (metadata.dev(), metadata.ino())));
                None
            }
            Ok(_) => Some(DiscoverySkip::WrongType),
            Err(error) => Some(DiscoverySkip::Io(error)),
        };
        if let Some(reason) = reason {
            result
                .diagnostics
                .push(DiscoveryDiagnostic { path, reason });
        }
    }
    result
}
