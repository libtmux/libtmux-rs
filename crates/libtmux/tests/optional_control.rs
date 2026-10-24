//! Native control acquisition eligibility and socket generation ownership.

#![cfg(all(feature = "control-mode", feature = "test-support"))]
#![allow(clippy::expect_used)]

use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command as Native, Output};
use std::time::Duration;

use libtmux::control::ControlMode;
use libtmux::test::{TestServer, install_executable, retry_until};
use libtmux::{Command, Error, Server};

fn modern() -> PathBuf {
    std::env::var_os("LIBTMUX_TEST_TMUX").map_or_else(|| PathBuf::from("tmux"), PathBuf::from)
}

/// The tmux a wrapper script execs, as an absolute path: a script has no
/// `PATH` lookup of the harness's bare `tmux`.
fn executable(guard: &TestServer) -> PathBuf {
    guard
        .server()
        .resolved_tmux_executable()
        .expect("the harness tmux resolves on PATH")
}

fn minimum() -> Option<PathBuf> {
    let variable = "LIBTMUX_TEST_MINIMUM_TMUX";
    let found = std::env::var_os(variable);
    assert!(
        found.is_some()
            || !std::env::var("LIBTMUX_REQUIRE_NATIVE")
                .is_ok_and(|required| required.split(',').any(|name| name == variable)),
        "{variable} is listed in LIBTMUX_REQUIRE_NATIVE but is unset"
    );
    found.map(PathBuf::from)
}

#[path = "support/versions.rs"]
mod versions;

async fn qualified_fixture() -> Option<TestServer> {
    versions::control_fixture(TestServer::builder().start().await.expect("tmux starts")).await
}

fn native(binary: &Path, socket: &Path, arguments: &[&OsStr]) -> Output {
    Native::new(binary)
        .arg("-S")
        .arg(socket)
        .arg("-f")
        .arg("/dev/null")
        .args(arguments)
        .env_remove("TMUX")
        .output()
        .expect("native client")
}

fn install_python(wrapper: &Path, script: &str) {
    let source = wrapper.with_extension("py");
    std::fs::write(&source, script).expect("Python fixture source");
    let quoted = source
        .to_str()
        .expect("owned ASCII fixture path")
        .replace('\'', "'\\''");
    install_executable(
        wrapper,
        &format!("#!/bin/sh\nexec /usr/bin/python3 '{quoted}' \"$@\"\n"),
    )
    .expect("wrapper");
}

fn delayed_server(guard: &TestServer, directory: &Path, delay: &str, timeout: Duration) -> Server {
    let wrapper = directory.join("tmux");
    let script = format!(
        r"#!/usr/bin/python3
import os,sys,pathlib,time
root=pathlib.Path({root})
binary={binary}
if any(a.startswith('libtmux-version-') for a in sys.argv):
 counter=root/'probes'
 count=int(counter.read_text())+1 if counter.exists() else 1
 counter.write_text(str(count))
 (root/('probe-'+str(count)+'.pid')).write_text(str(os.getpid()))
 time.sleep({delay})
if '-C' in sys.argv and not any(a.startswith('libtmux-version-') for a in sys.argv): (root/'control-started').touch()
os.execv(binary,[binary,*sys.argv[1:]])
",
        root = serde_json::to_string(directory).expect("root"),
        binary = serde_json::to_string(&executable(guard)).expect("binary"),
    );
    install_python(&wrapper, &script);
    Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(wrapper)
        .default_timeout(timeout)
        .build()
        .expect("bounded client")
}

fn process_exists(pid: u32) -> bool {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .expect("owned probe pid");
    !matches!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    )
}

async fn probe_reaped(directory: &Path) -> bool {
    let Some(pid) = std::fs::read_to_string(directory.join("probe-2.pid"))
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
    else {
        return false;
    };
    retry_until(Duration::from_secs(2), async || !process_exists(pid))
        .await
        .is_ok()
}

fn carriers(socket: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(socket.parent().expect("socket parent"))
        .expect("socket directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| {
            path.file_name()
                .expect("name")
                .to_string_lossy()
                .starts_with(".lt-")
        })
        .collect()
}

async fn control_acquisition_rank(reported: &str, refused: bool) {
    let Some(guard) = qualified_fixture().await else {
        return;
    };
    let session = guard.server().new_session("keeper").await.expect("session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witness");
    let marker = directory.path().join("control-started");
    let wrapper = directory.path().join("tmux");
    let script = format!(
        r"#!/usr/bin/python3
import os,sys,pathlib,subprocess
binary={binary}
if any(a.startswith('libtmux-version-') for a in sys.argv):
 result=subprocess.run([binary,*sys.argv[1:]],capture_output=True)
 text=result.stderr.decode()
 marker=[a for a in sys.argv if a.startswith('libtmux-version-')][0].split('#')[0]
 lead='unknown command: '+marker
 if text.startswith(lead):
  fields=text.strip()[len(lead):].split(',')
  fields[0]={reported}
  text=lead+','.join(fields)+'\n'
 sys.stdout.buffer.write(result.stdout)
 sys.stderr.buffer.write(text.encode())
 sys.exit(result.returncode)
if '-C' in sys.argv and not any(a.startswith('libtmux-version-') for a in sys.argv): pathlib.Path({marker}).touch()
os.execv(binary,[binary,*sys.argv[1:]])
",
        binary = serde_json::to_string(&executable(&guard)).expect("binary"),
        reported = serde_json::to_string(reported).expect("version"),
        marker = serde_json::to_string(&marker).expect("marker"),
    );
    install_python(&wrapper, &script);
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(wrapper)
        .build()
        .expect("client");
    let result = ControlMode::attach(&server, session.id()).await;
    let identified = marker.exists();
    let rank_refused = matches!(
        &result,
        Err(Error::UnsupportedCapability { found, .. }) if found.raw() == reported
    );
    let any_refusal = matches!(&result, Err(Error::UnsupportedCapability { .. }));
    let failure = result.as_ref().err().map(ToString::to_string);
    // A daemon that really is the reported build passes the version fence and
    // attaches, so close that connection before sampling what it leaves behind.
    if let Ok(mode) = result {
        let (_commands, events) = mode.split();
        events.shutdown().await.expect("attached control closes");
    }
    let aliases = carriers(guard.socket_path());
    let clients = guard.server().clients().await.expect("native clients");
    let ordinary = guard.server().sessions().await.expect("ordinary listing");
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
    if refused {
        assert!(
            rank_refused,
            "a build ranking below 3.7 refuses before acquisition: {reported}: {failure:?}; control_started={identified}"
        );
        assert!(!identified, "no -C process was invoked for {reported}");
    } else {
        // A real daemon that is not the reported build fails the version
        // fence; what matters is that rank did not refuse the attach.
        assert!(
            !any_refusal,
            "a build ranking above 3.7 is not refused on rank: {reported}: {failure:?}"
        );
        assert!(identified, "the -C process was invoked for {reported}");
    }
    assert!(aliases.is_empty(), "no carrier was left for {reported}");
    assert!(clients.is_empty(), "no client registered for {reported}");
    assert_eq!(ordinary.len(), 1);
}

#[tokio::test]
async fn real_tmux_compat_optional_control_refuses_builds_ranking_below_3_7() {
    for reported in ["next-3.7", "3.7-rc1", "3.7-rc2"] {
        control_acquisition_rank(reported, true).await;
    }
}

#[tokio::test]
async fn real_tmux_compat_optional_control_accepts_builds_ranking_above_3_7() {
    for reported in ["3.8-rc2", "next-3.8", "next-3.9"] {
        control_acquisition_rank(reported, false).await;
    }
}

#[tokio::test]
async fn real_tmux_compat_optional_control_refuses_3_3a_before_identification() {
    let Some(minimum) = minimum() else { return };
    let guard = TestServer::builder()
        .tmux_executable(&minimum)
        .start()
        .await
        .expect("3.3a starts");
    let session = guard
        .server()
        .new_session("keeper")
        .await
        .expect("ordinary creation");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witness");
    let marker = directory.path().join("control-started");
    let wrapper = directory.path().join("tmux");
    let script = format!(
        "#!/usr/bin/python3\nimport os,sys,pathlib\nif '-C' in sys.argv and not any(a.startswith('libtmux-version-') for a in sys.argv): pathlib.Path({}).touch()\nos.execv({},[{},*sys.argv[1:]])\n",
        serde_json::to_string(&marker).expect("path"),
        serde_json::to_string(&modern()).expect("binary"),
        serde_json::to_string(&modern()).expect("binary")
    );
    install_python(&wrapper, &script);
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(wrapper)
        .build()
        .expect("client");
    let result = ControlMode::attach(&server, session.id()).await;
    assert!(
        matches!(result, Err(Error::UnsupportedCapability { .. })),
        "old control must refuse"
    );
    assert!(!marker.exists(), "no -C process was invoked");
    assert_eq!(server.sessions().await.expect("ordinary listing").len(), 1);
    assert!(carriers(guard.socket_path()).is_empty());
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn real_tmux_compat_optional_control_owns_a_private_socket_carrier() {
    let Some(guard) = qualified_fixture().await else {
        return;
    };
    let session = guard.server().new_session("keeper").await.expect("session");
    let control = ControlMode::attach(guard.server(), session.id())
        .await
        .expect("qualified control");
    let aliases = carriers(guard.socket_path());
    assert_eq!(
        aliases.len(),
        1,
        "one private carrier owns this acquisition"
    );
    let original = std::fs::metadata(guard.socket_path()).expect("socket metadata");
    for alias in &aliases {
        assert_eq!(
            std::fs::metadata(alias)
                .expect("carrier mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let linked = std::fs::metadata(alias.join("s")).expect("linked socket");
        assert_eq!(
            (linked.dev(), linked.ino()),
            (original.dev(), original.ino())
        );
    }
    assert!(
        control
            .send(Command::new("list-sessions"))
            .await
            .expect("control read")
            .succeeded()
    );
    control.shutdown().await.expect("owned client reaped");
    assert!(
        carriers(guard.socket_path()).is_empty(),
        "carrier removed after child cleanup"
    );
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn real_tmux_compat_optional_scanner_a_long_native_socket_can_acquire_control() {
    let Some(guard) = qualified_fixture().await else {
        return;
    };
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned long endpoint");
    let padding = 100_usize
        .checked_sub(directory.path().as_os_str().len() + 1)
        .expect("fixture root leaves room for a long socket");
    let parent = directory.path().join("p".repeat(padding));
    std::fs::create_dir(&parent).expect("long peer directory");
    let socket = parent.join("s");
    assert_eq!(socket.as_os_str().len(), 102);
    let binary = executable(&guard);
    let binary = binary.as_path();
    let started = native(
        binary,
        &socket,
        &[
            OsStr::new("new-session"),
            OsStr::new("-d"),
            OsStr::new("-s"),
            OsStr::new("keeper"),
            OsStr::new("exec sleep 60"),
        ],
    );
    assert!(
        started.status.success(),
        "the original native endpoint is valid"
    );
    let probes = directory.path().join("probes.jsonl");
    let wrapper = directory.path().join("tmux");
    let script = format!(
        r"#!/usr/bin/python3
import os,sys,pathlib,subprocess,json
binary={binary}
if any(a.startswith('libtmux-version-') for a in sys.argv):
 result=subprocess.run([binary,*sys.argv[1:]],capture_output=True)
 with pathlib.Path({probes}).open('a') as stream:
  stream.write(json.dumps({{'argv':sys.argv[1:],'status':result.returncode,'stderr':result.stderr.decode(errors='replace')}})+'\n')
 sys.stdout.buffer.write(result.stdout)
 sys.stderr.buffer.write(result.stderr)
 sys.exit(result.returncode)
os.execv(binary,[binary,*sys.argv[1:]])
",
        binary = serde_json::to_string(binary).expect("binary"),
        probes = serde_json::to_string(&probes).expect("probes"),
    );
    install_python(&wrapper, &script);
    let server = Server::builder()
        .socket_path(&socket)
        .tmux_executable(wrapper)
        .build()
        .expect("long endpoint handle");
    let session = server.sessions().await.expect("native listing").remove(0);
    let result = ControlMode::attach(&server, session.id()).await;
    let attached = result.is_ok();
    let error = result.as_ref().err().map(ToString::to_string);
    if let Ok(control) = result {
        control.shutdown().await.expect("control closes");
    }
    let records = std::fs::read_to_string(&probes).expect("actual native probe records");
    let _ = native(
        binary,
        &socket,
        &[OsStr::new("-N"), OsStr::new("kill-server")],
    );
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
    assert!(attached, "{error:?}; actual native probes: {records}");
}

#[tokio::test]
async fn real_tmux_compat_optional_control_probes_share_one_acquisition_deadline() {
    let Some(guard) = qualified_fixture().await else {
        return;
    };
    let session = guard.server().new_session("keeper").await.expect("session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned probe witness");
    let server = delayed_server(&guard, directory.path(), "1.2", Duration::from_secs(2));
    let result = ControlMode::attach(&server, session.id()).await;
    let timed_out = matches!(result, Err(Error::Timeout { .. }));
    if let Ok(control) = result {
        control.shutdown().await.expect("unexpected control closes");
    }
    let reaped = probe_reaped(directory.path()).await;
    let invoked = directory.path().join("control-started").exists();
    let retained = carriers(guard.socket_path());
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
    assert!(timed_out, "two probes cannot each spend the full budget");
    assert!(!invoked, "deadline stops before invoking -C");
    assert!(retained.is_empty(), "timeout drops the private carrier");
    assert!(reaped, "timed-out probe is reaped");
}

#[tokio::test]
async fn real_tmux_compat_optional_control_cancellation_reaps_the_probe_and_carrier() {
    let Some(guard) = qualified_fixture().await else {
        return;
    };
    let session = guard.server().new_session("keeper").await.expect("session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned probe witness");
    let server = delayed_server(
        &guard,
        directory.path(),
        "300 if count == 2 else 0",
        Duration::from_secs(3),
    );
    let attaching_server = server.clone();
    let session = session.id().clone();
    let attaching =
        tokio::spawn(async move { ControlMode::attach(&attaching_server, &session).await });
    let waiting = retry_until(Duration::from_secs(2), async || {
        directory.path().join("probe-2.pid").is_file()
    })
    .await;
    attaching.abort();
    let _ = attaching.await;
    let reaped = probe_reaped(directory.path()).await;
    let invoked = directory.path().join("control-started").exists();
    let retained = carriers(guard.socket_path());
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
    assert!(waiting.is_ok(), "cancellation reached the pinned probe");
    assert!(!invoked, "cancelled acquisition never invokes -C");
    assert!(
        retained.is_empty(),
        "cancellation drops the private carrier"
    );
    assert!(reaped, "cancelled probe is reaped");
}

#[tokio::test]
async fn real_tmux_compat_optional_control_replacement_cannot_redirect_identification() {
    let Some(minimum) = minimum() else { return };
    let modern = modern();
    let Some(guard) = qualified_fixture().await else {
        return;
    };
    let session = guard.server().new_session("keeper").await.expect("session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned witness");
    let wrapper = directory.path().join("tmux");
    let witness = directory.path().join("witness.json");
    let script = format!(
        r"#!/usr/bin/python3
import os,sys,pathlib,subprocess,json,time
modern={modern}
minimum={minimum}
socket={socket}
witness={witness}
def native(binary,*args):
 return subprocess.run([binary,'-S',socket,'-f','/dev/null',*args],capture_output=True,timeout=4,cwd=pathlib.Path(witness).parent)
if '-C' in sys.argv and not any(a.startswith('libtmux-version-') for a in sys.argv):
 assert native(modern,'-N','kill-server').returncode == 0
 end=time.monotonic()+3
 while native(modern,'-N','display-message','-p','version').returncode == 0:
  assert time.monotonic()<end
  time.sleep(.005)
 assert native(minimum,'-vv','new-session','-d','-s','replacement','exec sleep 300').returncode == 0
 child=subprocess.Popen([modern,*sys.argv[1:]],stdin=subprocess.PIPE,stdout=subprocess.PIPE)
 end=time.monotonic()+1
 guard_received=False
 while time.monotonic()<end:
  logs=''.join(path.read_text(errors='replace') for path in pathlib.Path(witness).parent.glob('tmux-server-*.log'))
  guard_received='libtmux-core-version-changed' in logs
  if guard_received or child.poll() is not None: break
  time.sleep(.005)
 child.terminate()
 output=child.communicate(timeout=4)[0]
 pathlib.Path(witness).write_text(json.dumps({{'argv':sys.argv[1:],'stdout':output.decode('utf-8','replace'),'guard_received':guard_received,'daemon':native(minimum,'-N','display-message','-p','#{{version}}').stdout.decode().strip()}}))
 sys.exit(1)
os.execv(modern,[modern,*sys.argv[1:]])
",
        modern = serde_json::to_string(&modern).expect("binary"),
        minimum = serde_json::to_string(&minimum).expect("binary"),
        socket = serde_json::to_string(guard.socket_path()).expect("socket"),
        witness = serde_json::to_string(&witness).expect("witness")
    );
    install_python(&wrapper, &script);
    let server = Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(wrapper)
        .build()
        .expect("client");
    let result = ControlMode::attach(&server, session.id()).await;
    assert!(result.is_err(), "replaced acquisition refuses");
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&witness).expect("replacement boundary reached"))
            .expect("witness parses");
    assert_eq!(record["daemon"], "3.3a");
    assert!(
        !record["guard_received"].as_bool().expect("guard witness"),
        "post-identification guard reached replacement: {record}"
    );
    assert!(
        !record["stdout"].as_str().expect("stdout").starts_with('%'),
        "control initialized on replacement: {record}"
    );
    assert!(
        carriers(guard.socket_path()).is_empty(),
        "failed acquisition removes carrier"
    );
    let clients = native(
        &minimum,
        guard.socket_path(),
        &[
            OsStr::new("-N"),
            OsStr::new("list-clients"),
            OsStr::new("-F"),
            OsStr::new("#{client_pid}"),
        ],
    );
    assert!(
        clients.stdout.is_empty(),
        "no acquired control client remains"
    );
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
}

fn delayed_opening_server(guard: &TestServer, directory: &Path, timeout: Duration) -> Server {
    let wrapper = directory.join("tmux");
    let script = format!(
        r"#!/usr/bin/python3
import os,sys,pathlib,time
root=pathlib.Path({root})
binary={binary}
if any(a.startswith('libtmux-version-') for a in sys.argv):
 counter=root/'probes'
 count=int(counter.read_text())+1 if counter.exists() else 1
 counter.write_text(str(count))
 time.sleep(.15)
if '-C' in sys.argv and not any(a.startswith('libtmux-version-') for a in sys.argv):
 (root/'control.pid').write_text(str(os.getpid()))
 if (root/'mode').read_text() == 'slow': time.sleep(.55)
os.execv(binary,[binary,*sys.argv[1:]])
",
        root = serde_json::to_string(directory).expect("root"),
        binary = serde_json::to_string(&executable(guard)).expect("binary"),
    );
    install_python(&wrapper, &script);
    Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(wrapper)
        .default_timeout(timeout)
        .control_client_limits(libtmux::ControlClientLimits::default().max_clients(1))
        .build()
        .expect("bounded control client")
}

#[tokio::test]
async fn real_tmux_compat_optional_control_opening_shares_the_acquisition_deadline() {
    let Some(guard) = qualified_fixture().await else {
        return;
    };
    let session = guard.server().new_session("keeper").await.expect("session");
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").expect("owned opening witness");
    std::fs::write(directory.path().join("mode"), "slow").expect("slow opening");
    let server = delayed_opening_server(&guard, directory.path(), Duration::from_millis(700));
    let started = std::time::Instant::now();
    let result = ControlMode::attach(&server, session.id()).await;
    let elapsed = started.elapsed();
    let timed_out = matches!(
        &result,
        Err(Error::ControlMode {
            kind: libtmux::ControlModeErrorKind::TimedOut,
            ..
        })
    );
    let kind = result.as_ref().err().map(Error::kind);
    let retryable = result.as_ref().err().is_some_and(Error::is_transient);
    if let Ok(control) = result {
        control.shutdown().await.expect("unexpected control closes");
    }
    let probes = std::fs::read_to_string(directory.path().join("probes")).expect("probe count");
    let pid = std::fs::read_to_string(directory.path().join("control.pid"))
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok());
    let reaped = pid.is_some_and(|pid| !process_exists(pid));
    let retained = carriers(guard.socket_path());
    let clients = guard.server().clients().await.expect("native clients");
    std::fs::write(directory.path().join("mode"), "ready").expect("ordinary opening");
    let reusable = ControlMode::attach(&server, session.id())
        .await
        .expect("capacity is returned");
    let steady = reusable
        .send(Command::new("run-shell").arg("sleep .5"))
        .await
        .expect("steady-state command keeps its own full timeout");
    reusable.shutdown().await.expect("reused client closes");
    let windows = session.windows().await.expect("ordinary topology");
    server.shutdown().await.expect("client closes");
    guard.shutdown().await.expect("fixture closes");
    assert!(
        timed_out,
        "opening must use the remaining acquisition budget: {elapsed:?}"
    );
    assert_eq!(probes, "2", "both metadata probes completed before -C");
    assert!(pid.is_some(), "the delayed control process was started");
    assert!(reaped, "the timed-out control process was reaped");
    assert!(retained.is_empty(), "the timed-out carrier was removed");
    assert!(
        clients.is_empty(),
        "the timed-out client is no longer registered"
    );
    assert_eq!(kind, Some(libtmux::ErrorKind::Timeout));
    assert!(!retryable, "the failed connection must be reacquired");
    assert!(steady.succeeded());
    assert_eq!(windows.len(), 1, "opening did not mutate the session");
}
