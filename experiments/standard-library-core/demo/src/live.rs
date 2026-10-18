//! Capture a real pane through the Tokio adapter using core-owned values.

use std::{
    error::Error,
    ffi::OsString,
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use libtmux::{CapturePane, PaneSnapshot};
use libtmux_tokio::Server;

struct Fixture {
    directory: PathBuf,
    program: OsString,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = PathBuf::from("/tmp/libtmux-rs-dev");
        std::fs::create_dir_all(&root)?;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let directory = root.join(format!("core-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&directory)?;
        let fixture = Self {
            directory,
            program: std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into()),
        };
        let output = fixture
            .command()
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "core-demo",
                "-x",
                "80",
                "-y",
                "24",
                "printf 'hello from the Tokio adapter\\n'; sleep 60",
            ])
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "starting tmux failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(fixture)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .arg("-S")
            .arg(self.directory.join("socket"))
            .env_remove("TMUX");
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
        let _ = std::fs::remove_file(self.directory.join("socket"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let server = Server::new(fixture.directory.join("socket")).program(fixture.program.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let snapshot = runtime.block_on(async {
        let panes = server.panes().await?;
        let pane = panes
            .first()
            .ok_or("the isolated server returned no panes")?;
        let request = CapturePane::new(pane.snapshot().id.clone()).history(10);
        let capture = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let capture = server.capture(&request).await?;
                if capture
                    .to_string_lossy()
                    .contains("hello from the Tokio adapter")
                {
                    return Ok::<_, Box<dyn Error>>(capture);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        assert!(
            pane.capture()
                .await?
                .as_bytes()
                .starts_with(b"hello from the Tokio adapter\n")
        );
        println!("{}", capture.to_string_lossy().trim_end());
        Ok::<PaneSnapshot, Box<dyn Error>>(pane.snapshot().clone())
    })?;
    drop(runtime);
    println!(
        "detached snapshot after runtime shutdown: {} ({} x {})",
        snapshot.id, snapshot.width, snapshot.height
    );
    Ok(())
}
