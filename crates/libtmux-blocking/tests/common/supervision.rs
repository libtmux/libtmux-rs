use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use libtmux_core::supervision::Budget;

#[derive(Debug)]
pub(super) struct Fixture(tempfile::TempDir);

impl Fixture {
    pub(super) fn new() -> Self {
        let root = Path::new("/tmp/libtmux-rs-test");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .expect("fixture root exists");
        Self(
            tempfile::Builder::new()
                .prefix("blocking-child-")
                .tempdir_in(root)
                .expect("private fixture directory"),
        )
    }

    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }

    pub(super) fn child(&self, source: &str) -> Command {
        let mut command = Command::new("python3");
        command
            .args(["-c", source])
            .arg(self.0.path())
            .current_dir(self.0.path())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
    }

    pub(super) fn wait_for(&self, name: &str) {
        let end = Instant::now() + Duration::from_secs(3);
        while !std::fs::read(self.path(name)).is_ok_and(|bytes| !bytes.is_empty()) {
            assert!(Instant::now() < end, "child did not publish {name}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn assert_reaped(&self, name: &str) {
        let pid = std::fs::read_to_string(self.path(name)).expect("child published PID");
        let process = Path::new("/proc").join(pid.trim());
        let end = Instant::now() + Duration::from_secs(2);
        while process.exists() {
            assert!(Instant::now() < end, "owned direct child was not reaped");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn assert_stopped(&self, name: &str) {
        let pid = std::fs::read_to_string(self.path(name)).expect("descendant published PID");
        let status = Path::new("/proc").join(pid.trim()).join("stat");
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::read_to_string(&status) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Ok(stat)
                    if stat
                        .rsplit_once(") ")
                        .is_some_and(|(_, rest)| rest.starts_with('Z')) =>
                {
                    break;
                }
                Ok(_) | Err(_) => {
                    assert!(Instant::now() < end, "owned descendant was not stopped");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
    }
}

pub(super) fn budget(duration: Duration, bytes_per_stream: usize) -> Budget {
    Budget {
        deadline: Instant::now() + duration,
        bytes_per_stream,
    }
}

pub(super) const PARKED: &str = r"
import os, sys, time
from pathlib import Path
root = Path(sys.argv[1])
(root / 'pid').write_text(str(os.getpid()))
while not (root / 'release').exists():
    time.sleep(0.002)
";
