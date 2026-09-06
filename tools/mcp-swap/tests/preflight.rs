//! Bounded MCP initialize preflight tests.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use mcp_swap::config::ServerSpec;
use mcp_swap::preflight::preflight;
use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};
use tempfile::tempdir;

#[test]
fn initialize_result_is_accepted_with_the_spec_environment() {
    let root = tempdir().expect("temporary root");
    let script = executable(
        root.path(),
        "success",
        "#!/bin/sh\nread request\n[ \"$TOKEN\" = yes ] || exit 9\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-06-18\"}}'\n",
    );
    let spec = ServerSpec {
        command: script.to_string_lossy().into_owned(),
        args: Vec::new(),
        env: BTreeMap::from([("TOKEN".into(), "yes".into())]),
    };

    preflight(&spec, Duration::from_secs(2)).expect("initialize response");
}

#[test]
fn initialize_result_is_accepted_before_a_long_lived_server_exits() {
    let root = tempdir().expect("temporary root");
    let child_pid = root.path().join("child.pid");
    let script = executable(
        root.path(),
        "long-lived",
        &format!(
            "#!/bin/sh\nread request\nsleep 30 &\necho $! > {}\nprintf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"protocolVersion\":\"2025-06-18\"}}}}'\nwait\n",
            child_pid.display()
        ),
    );
    let spec = ServerSpec {
        command: script.to_string_lossy().into_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let started = Instant::now();

    preflight(&spec, Duration::from_secs(2)).expect("initialize response before exit");

    assert!(started.elapsed() < Duration::from_secs(1));
    assert_process_stopped(&child_pid);
}

#[test]
fn launch_failure_and_stderr_are_reported() {
    let missing = ServerSpec {
        command: "/no/such/mcp-swap-server".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    assert!(
        preflight(&missing, Duration::from_millis(100))
            .expect_err("launch failure")
            .to_string()
            .contains("launch")
    );

    let root = tempdir().expect("temporary root");
    let script = executable(
        root.path(),
        "stderr",
        "#!/bin/sh\nread request\nprintf '%s\\n' 'first detail' >&2\nprintf '%s\\n' 'last diagnostic' >&2\nprintf '%s\\n' 'not-json'\nexit 1\n",
    );
    let error = preflight(
        &ServerSpec {
            command: script.to_string_lossy().into_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
        Duration::from_secs(2),
    )
    .expect_err("missing initialize result");
    assert!(error.to_string().contains("last diagnostic"));
}

#[test]
fn incomplete_initialize_result_is_rejected() {
    let root = tempdir().expect("temporary root");
    let script = executable(
        root.path(),
        "incomplete",
        "#!/bin/sh\nread request\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\n",
    );

    preflight(
        &ServerSpec {
            command: script.to_string_lossy().into_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
        Duration::from_secs(2),
    )
    .expect_err("incomplete initialize result");
}

#[test]
fn timeout_kills_the_bounded_process_group() {
    let root = tempdir().expect("temporary root");
    let script = executable(root.path(), "hang", "#!/bin/sh\nread request\nsleep 30\n");
    let started = Instant::now();

    let error = preflight(
        &ServerSpec {
            command: script.to_string_lossy().into_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
        Duration::from_millis(100),
    )
    .expect_err("timeout");

    assert!(error.to_string().contains("within"));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn long_lived_oversized_streams_are_refused_and_reaped() {
    let root = tempdir().expect("temporary root");
    for (name, redirect) in [("stdout", ""), ("stderr", " >&2")] {
        let child_pid = root.path().join(format!("{name}.pid"));
        let heartbeat = root.path().join(format!("{name}.heartbeat"));
        let script = executable(
            root.path(),
            name,
            &format!(
                "#!/bin/sh\nread request\nwhile :; do printf tick >> {}; sleep 0.01; done &\necho $! > {}\nyes x{redirect}\nwait\n",
                heartbeat.display(),
                child_pid.display()
            ),
        );
        let started = Instant::now();

        let error = preflight(
            &ServerSpec {
                command: script.to_string_lossy().into_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
            Duration::from_secs(2),
        )
        .expect_err("oversized output");

        assert!(error.to_string().contains("exceeds"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_process_stopped(&child_pid);
        let size = fs::metadata(&heartbeat).expect("heartbeat").len();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            fs::metadata(&heartbeat).expect("stopped heartbeat").len(),
            size
        );
    }
}

fn assert_process_stopped(pid_path: &Path) {
    let raw_pid = fs::read_to_string(pid_path).expect("descendant PID");
    let pid = Pid::from_raw(raw_pid.trim().parse().expect("numeric descendant PID"))
        .expect("positive descendant PID");
    for _ in 0..100 {
        if matches!(test_kill_process(pid), Err(Errno::SRCH)) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("preflight descendant remains alive");
}

fn executable(root: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::write(&path, body).expect("script");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("script mode");
    path
}
