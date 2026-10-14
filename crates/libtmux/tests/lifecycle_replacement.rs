//! Receipt authority must cover final writes as well as cleanup.
#![cfg(feature = "test-support")]

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use libtmux::lifecycle::{FindOrCreate, PaneIdentity};
use libtmux::test::{install_executable, retry_until};
use libtmux::{Command, Error, NewSessionOptions, Server, SplitDirection, SplitOptions};

const OLD_TOKEN: &str = "11111111111111111111111111111111";
const NEW_TOKEN: &str = "22222222222222222222222222222222";

fn split() -> SplitOptions {
    SplitOptions::new(SplitDirection::Below).command("sleep 300")
}

#[tokio::test]
async fn window_finalization_refuses_a_real_replacement_with_reused_ids() {
    for collision in [false, true] {
        finalization_refuses_replacement("window", collision).await;
    }
}

#[tokio::test]
async fn pane_finalization_refuses_a_real_replacement_with_reused_ids() {
    for collision in [false, true] {
        finalization_refuses_replacement("pane", collision).await;
    }
}

#[allow(clippy::unwrap_used, reason = "fixture setup and observations")]
async fn finalization_refuses_replacement(kind: &'static str, collision: bool) {
    let mut fixture = Fixture::new(collision).await;
    let session = fixture.client.session("keeper").await.unwrap().unwrap();
    let window = session.active_window().await.unwrap().unwrap();
    std::fs::write(fixture.directory.join("hold"), "").unwrap();
    let request = tokio::spawn(async move {
        if kind == "window" {
            session
                .find_or_create_window("requested-window")
                .await
                .map(|result| result.is_created())
        } else {
            window
                .find_or_create_pane(
                    PaneIdentity::new("@review-role", "requested-role").unwrap(),
                    split(),
                )
                .await
                .map(|result| result.is_created())
        }
    });
    retry_until(Duration::from_secs(5), || async {
        fixture.directory.join("ready").exists()
    })
    .await
    .unwrap();
    let receipt = std::fs::read_to_string(fixture.directory.join("receipt")).unwrap();
    let created_id = receipt.split_whitespace().nth(2).unwrap().to_owned();
    fixture.replace().await;
    let replacement_id = fixture.prepare_replacement(kind).await;
    let before = fixture.values(kind).await;
    std::fs::remove_file(fixture.directory.join("hold")).unwrap();
    let result = request.await.unwrap();
    let after = fixture.values(kind).await;
    let alive = fixture.child.try_wait().unwrap().is_none();
    eprintln!("{kind}, collision={collision}, id={created_id}, result={result:?}");
    fixture.finish().await;

    assert_eq!(created_id, replacement_id, "replacement must reuse the ID");
    assert_eq!(after, before, "final writes reached the replacement");
    assert!(alive, "failed rollback must leave the replacement alive");
    assert!(
        matches!(result, Err(Error::AcquisitionRollback { operation, cleanup })
            if mismatch(&operation, collision) && mismatch(&cleanup, collision)),
        "finalization and its refused rollback must both remain visible",
    );
}

fn mismatch(error: &Error, collision: bool) -> bool {
    if collision {
        matches!(error, Error::OwnershipTokenChanged)
    } else {
        matches!(error, Error::ServerGenerationChanged { .. })
    }
}

#[tokio::test]
async fn all_owners_refuse_a_real_replacement_with_equal_numeric_identity() {
    let mut fixture = Fixture::new(true).await;
    let session = fixture.client.session("keeper").await.unwrap().unwrap();
    let window = session.active_window().await.unwrap().unwrap();
    let pane = window.active_pane().await.unwrap().unwrap();
    let server_owner = fixture.client.adopt().await.unwrap();
    let session_owner = session.adopt().await.unwrap();
    let window_owner = window.adopt().await.unwrap();
    let pane_owner = pane.adopt().await.unwrap();
    let accepted = server_owner.generation();
    let old_ids = (session.id().clone(), window.id().clone(), pane.id().clone());
    fixture.replace().await;
    let numeric_fields = fixture.client.generation().await.unwrap();
    let replacement = fixture.observer.session("keeper").await.unwrap().unwrap();
    let fresh_window = replacement.active_window().await.unwrap().unwrap();
    let fresh_pane = fresh_window.active_pane().await.unwrap().unwrap();
    let new_ids = (
        replacement.id().clone(),
        fresh_window.id().clone(),
        fresh_pane.id().clone(),
    );
    let outcomes = [
        pane_owner.close().await,
        window_owner.close().await,
        session_owner.close().await,
        server_owner.close().await,
    ];
    let closed = [
        pane_owner.is_closed(),
        window_owner.is_closed(),
        session_owner.is_closed(),
        server_owner.is_closed(),
    ];
    let survivors = fixture.observer.panes().await.map(|panes| panes.len());
    let alive = fixture.child.try_wait().unwrap().is_none();
    eprintln!("equal numeric fields: {accepted:?} == {numeric_fields:?}; outcomes={outcomes:?}");
    fixture.finish().await;

    assert_eq!(
        accepted, numeric_fields,
        "fixture forces both numeric fields equal"
    );
    assert_eq!(old_ids, new_ids, "all child IDs are reused");
    assert!(
        outcomes
            .iter()
            .all(|result| matches!(result, Err(Error::OwnershipTokenChanged)))
    );
    assert_eq!(closed, [false; 4], "each failed owner remains retryable");
    assert!(matches!(survivors, Ok(1)), "replacement pane survives");
    assert!(alive, "replacement daemon survives");
}

#[tokio::test]
async fn guarded_finalization_preserves_literal_names_and_every_nonzero_value_byte() {
    let fixture = Fixture::new(false).await;
    let session = fixture.client.session("keeper").await.unwrap().unwrap();
    let name = OsString::from("- ' \" $HOME ; #{pane_id} %ifλ");
    let outcome = async {
        let created = session.find_or_create_window(name.clone()).await?;
        let window = created.resource();
        let actual_name = fixture.observer.window_by_id(window.id()).await?.unwrap();
        let value = OsString::from_vec((1..=255).collect());
        let identity = PaneIdentity::new("@literal", value)?;
        let pane = window
            .find_or_create_pane(identity.clone(), split())
            .await?;
        let reused = window.find_or_create_pane(identity, split()).await?;
        Ok::<_, Error>((
            actual_name,
            pane.resource().id() == reused.resource().id(),
            matches!(reused, FindOrCreate::Reused(_)),
        ))
    }
    .await;
    fixture.finish().await;

    let (actual_name, same_pane, reused_is_borrowed) = outcome.unwrap();
    assert_eq!(actual_name.name().as_bytes(), name.as_encoded_bytes());
    assert!(
        same_pane && reused_is_borrowed,
        "literal value finds the created pane again"
    );
}

#[cfg(feature = "control-mode")]
#[tokio::test]
async fn guarded_finalization_preserves_literals_over_control_mode() {
    let fixture = Fixture::new(false).await;
    let session = fixture.observer.session("keeper").await.unwrap().unwrap();
    let (sender, events) = libtmux::control::ControlMode::attach(&fixture.observer, session.id())
        .await
        .unwrap()
        .split();
    let routed = fixture.observer.over_control_mode(&sender).await.unwrap();
    let name = "control ' \" $HOME ; #{pane_id}";
    let outcome = async {
        let session = routed.session("keeper").await?.unwrap();
        let window = session.find_or_create_window(name).await?;
        let actual = fixture
            .observer
            .window_by_id(window.resource().id())
            .await?
            .unwrap();
        let identity = PaneIdentity::new("@control-literal", "quotes ' \" \\ $HOME ;\n#{pane_id}")?;
        let pane = window
            .resource()
            .find_or_create_pane(identity.clone(), split())
            .await?;
        let reused = window
            .resource()
            .find_or_create_pane(identity, split())
            .await?;
        Ok::<_, Error>((
            actual,
            pane.resource().id() == reused.resource().id(),
            reused.is_created(),
        ))
    }
    .await;
    let transport_cleanup = events.shutdown().await;
    fixture.finish().await;

    transport_cleanup.unwrap();
    let (actual, same_id, created) = outcome.unwrap();
    assert_eq!(actual.name().as_bytes(), name.as_bytes());
    assert!(same_id && !created);
}

struct Fixture {
    directory: PathBuf,
    observer: Server,
    client: Server,
    child: tokio::process::Child,
}

#[allow(
    clippy::unwrap_used,
    reason = "owned foreground fixture setup and teardown"
)]
impl Fixture {
    async fn new(collision: bool) -> Self {
        std::fs::create_dir_all("/tmp/libtmux-rs-test").unwrap();
        // An early failure retains the directory; only finish removes it after exit.
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap().keep();
        eprintln!("owned fixture directory: {}", directory.display());
        let observer = Server::builder()
            .socket_path(directory.join("server.sock"))
            .tmux_executable(std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into()))
            .build()
            .unwrap();
        let config = directory.join("tmux.conf");
        std::fs::write(
            &config,
            "set -s exit-empty off\nset -g default-shell /bin/sh\n",
        )
        .unwrap();
        let child = start(&observer, &config).await;
        let script = directory.join("wrapper");
        wrapper(&script, &observer, collision);
        let client = Server::builder()
            .socket_path(observer.socket_path())
            .tmux_executable(script)
            .default_timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let fixture = Self {
            directory,
            observer,
            client,
            child,
        };
        fixture.populate(OLD_TOKEN).await;
        fixture
    }

    async fn populate(&self, token: &str) {
        self.observer
            .new_session(NewSessionOptions::new("keeper").command("sleep 300"))
            .await
            .unwrap();
        self.observer
            .cmd(
                Command::new("set-option")
                    .arg("-s")
                    .arg("@libtmux_owner_generation")
                    .arg(token),
            )
            .await
            .unwrap();
    }

    async fn replace(&mut self) {
        let old_pid = self.child.id().unwrap();
        self.observer.kill().await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        eprintln!("old daemon {old_pid} exit observed: {status}");
        self.child = start(&self.observer, &self.directory.join("tmux.conf")).await;
        assert_ne!(
            self.child.id().unwrap(),
            old_pid,
            "a real process replaces the old one"
        );
        eprintln!("replacement daemon {} started", self.child.id().unwrap());
        self.populate(NEW_TOKEN).await;
    }

    async fn prepare_replacement(&self, kind: &str) -> String {
        let session = self.observer.session("keeper").await.unwrap().unwrap();
        if kind == "window" {
            let window = session.new_window("untouched-window").await.unwrap();
            window
                .set_option("automatic-rename-format", "untouched-window")
                .await
                .unwrap();
            window.set_option("automatic-rename", "on").await.unwrap();
            window.id().to_string()
        } else {
            let pane = session
                .active_window()
                .await
                .unwrap()
                .unwrap()
                .split(split())
                .await
                .unwrap();
            pane.set_option("@review-role", "untouched-role")
                .await
                .unwrap();
            pane.id().to_string()
        }
    }

    async fn values(&self, kind: &str) -> Vec<u8> {
        let command = if kind == "window" {
            Command::new("display-message")
                .arg("-p")
                .arg("-t")
                .arg("@1")
                .arg("#{window_name} #{automatic-rename}")
        } else {
            Command::new("show-options")
                .arg("-pqv")
                .arg("-t")
                .arg("%1")
                .arg("@review-role")
        };
        let result = self.observer.cmd(command).await.unwrap();
        assert!(result.success(), "replacement lookup: {result:?}");
        result.stdout().to_vec()
    }

    async fn finish(mut self) {
        let pid = self.child.id();
        if self.child.try_wait().unwrap().is_none() {
            self.observer.kill().await.unwrap();
        }
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        eprintln!("final daemon {pid:?} exit observed: {status}");
        let cleanup = self.client.drain_cleanup().await;
        self.client.shutdown().await.unwrap();
        self.observer.shutdown().await.unwrap();
        std::fs::remove_dir_all(&self.directory).unwrap();
        eprintln!(
            "owned directory removed after exit: {}",
            self.directory.display()
        );
        assert!(cleanup.is_empty(), "{cleanup:?}");
    }
}

#[allow(clippy::unwrap_used, reason = "owned foreground fixture setup")]
async fn start(server: &Server, config: &Path) -> tokio::process::Child {
    let child = tokio::process::Command::new(server.tmux_executable())
        .args(["-D", "-S"])
        .arg(server.socket_path())
        .arg("-f")
        .arg(config)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    retry_until(Duration::from_secs(5), || async {
        server.generation().await.is_ok()
    })
    .await
    .unwrap();
    child
}

#[allow(clippy::unwrap_used, reason = "fault wrapper fixture setup")]
fn wrapper(script: &Path, observer: &Server, collision: bool) {
    let body = format!(
        r#"#!/bin/sh
exec /usr/bin/python3 - "$@" <<'PY'
import pathlib, subprocess, sys, time
root = pathlib.Path({root:?})
args = sys.argv[1:]
if {collision}:
    # Force equal fields on both real daemons, including the executed guard.
    args = [arg.replace('#{{pid}}', '700001').replace('#{{start_time}}', '700002') for arg in args]
result = subprocess.run([{tmux:?}, *args], capture_output=True)
if result.returncode == 0 and any(arg in ['new-window', 'split-window'] for arg in args):
    (root / 'receipt').write_bytes(result.stdout)
    (root / 'ready').touch()
    deadline = time.monotonic() + 15
    while (root / 'hold').exists():
        if time.monotonic() >= deadline: raise RuntimeError('creation receipt was not released')
        time.sleep(.005)
sys.stdout.buffer.write(result.stdout)
sys.stderr.buffer.write(result.stderr)
sys.exit(result.returncode)
PY
"#,
        root = script.parent().unwrap().to_string_lossy(),
        tmux = observer.tmux_executable().to_string_lossy(),
        collision = if collision { "True" } else { "False" },
    );
    install_executable(script, &body).unwrap();
}
