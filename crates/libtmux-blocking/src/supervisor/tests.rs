use std::process::Command;
use std::time::{Duration, Instant};

use libtmux_core::{Effect, Error};

use super::Children;
use crate::{CancellationToken, RequestOptions};

#[path = "../../tests/common/supervision.rs"]
mod fixture;

fn options(duration: Duration, limit: usize) -> RequestOptions {
    RequestOptions {
        budget: fixture::budget(duration, limit),
        cancellation: CancellationToken::default(),
    }
}

#[test]
fn drains_both_streams_before_waiting_for_exit() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(
        "import os, threading\na=threading.Thread(target=lambda: os.write(1,b'a'*131072))\nb=threading.Thread(target=lambda: os.write(2,b'b'*131072))\na.start(); b.start(); a.join(); b.join()",
    );
    let (output, late) = Children::default()
        .dispatch(command, options(Duration::from_secs(3), 131_072), None)
        .expect("both writers finish");
    assert!(output.status.success());
    assert!(!late);
    assert_eq!(output.stdout, vec![b'a'; 131_072]);
    assert_eq!(output.stderr, vec![b'b'; 131_072]);
}

#[test]
fn rejects_a_request_before_spawning_after_its_deadline() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child("import pathlib, sys; pathlib.Path(sys.argv[1], 'ran').touch()");
    let result = Children::default().dispatch(command, options(Duration::ZERO, 16), None);
    assert!(matches!(result, Err(Error::BeforeSpawn)));
    assert!(!fixture.path("ran").exists());
}

#[test]
fn cancellation_before_spawn_cannot_run_the_request() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child("import pathlib, sys; pathlib.Path(sys.argv[1], 'ran').touch()");
    let request = options(Duration::from_secs(3), 16);
    request.cancellation.cancel();
    let error = Children::default()
        .dispatch(command, request, None)
        .expect_err("cancelled request is not started");
    assert!(matches!(error, Error::CancelledBeforeSpawn));
    assert_eq!(error.effect(), Effect::NotSubmitted);
    assert!(!fixture.path("ran").exists());
}

#[test]
fn a_missing_executable_has_no_submitted_effect() {
    let fixture = fixture::Fixture::new();
    let error = Children::default()
        .dispatch(
            Command::new(fixture.path("missing")),
            options(Duration::from_secs(3), 16),
            None,
        )
        .expect_err("executable is absent");
    assert_eq!(error.effect(), Effect::NotSubmitted);
    assert!(matches!(error, Error::Spawn(source) if source.kind() == std::io::ErrorKind::NotFound));
}

#[cfg(target_os = "linux")]
#[test]
fn each_stream_limit_reclaims_the_child() {
    for (fd, stream) in [(1, "stdout"), (2, "stderr")] {
        let fixture = fixture::Fixture::new();
        let mut command = fixture.child(
            "import os, sys, time\nfrom pathlib import Path\nPath(sys.argv[1], 'pid').write_text(str(os.getpid()))\nos.write(int(sys.argv[2]), b'x'*4097)\ntime.sleep(30)",
        );
        command.arg(fd.to_string());
        let result =
            Children::default().dispatch(command, options(Duration::from_secs(3), 4096), None);
        assert!(
            matches!(result, Err(Error::OutputLimit { stream: actual, limit: 4096 }) if actual == stream)
        );
        fixture.assert_reaped("pid");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn closed_pipes_do_not_hide_a_stalled_child() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(
        "import os, sys, time\nfrom pathlib import Path\nPath(sys.argv[1], 'pid').write_text(str(os.getpid()))\nos.close(1); os.close(2); time.sleep(30)",
    );
    let started = Instant::now();
    let result =
        Children::default().dispatch(command, options(Duration::from_millis(500), 4096), None);
    assert!(matches!(result, Err(Error::Deadline)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    fixture.assert_reaped("pid");
}

#[cfg(target_os = "linux")]
#[test]
fn a_child_that_leaves_its_group_is_still_killed_and_reaped() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(
        "import os, sys, time\nfrom pathlib import Path\nos.setpgid(0, os.getpgid(os.getppid()))\nPath(sys.argv[1], 'pid').write_text(str(os.getpid()))\ntime.sleep(30)",
    );
    let result =
        Children::default().dispatch(command, options(Duration::from_millis(500), 4096), None);
    assert!(matches!(result, Err(Error::Deadline)), "{result:?}");
    fixture.assert_reaped("pid");
}

#[cfg(target_os = "linux")]
#[test]
fn cancellation_reclaims_a_running_child_and_its_descendant() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(
        "import os, subprocess, sys, time\nfrom pathlib import Path\nroot=Path(sys.argv[1])\n(root/'pid').write_text(str(os.getpid()))\nchild=subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])\n(root/'descendant').write_text(str(child.pid))\ntime.sleep(30)",
    );
    let request = options(Duration::from_secs(10), 4096);
    let cancellation = request.cancellation.clone();
    let worker = std::thread::spawn(move || Children::default().dispatch(command, request, None));
    fixture.wait_for("descendant");
    cancellation.cancel();
    let result = worker.join().expect("caller joins");
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    fixture.assert_reaped("pid");
    fixture.assert_stopped("descendant");
}

#[cfg(target_os = "linux")]
#[test]
fn a_dead_group_leader_does_not_hide_descendants_holding_its_pipes() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(
        "import os, subprocess, sys\nfrom pathlib import Path\nroot=Path(sys.argv[1])\n(root/'pid').write_text(str(os.getpid()))\nchild=subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])\n(root/'descendant').write_text(str(child.pid))",
    );
    let result =
        Children::default().dispatch(command, options(Duration::from_millis(500), 4096), None);
    assert!(matches!(result, Err(Error::Deadline)), "{result:?}");
    fixture.assert_reaped("pid");
    fixture.assert_stopped("descendant");
}

#[cfg(target_os = "linux")]
#[test]
fn successful_completion_stops_descendants_before_reaping_the_leader() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(
        "import os, subprocess, sys\nfrom pathlib import Path\nroot=Path(sys.argv[1])\n(root/'pid').write_text(str(os.getpid()))\nchild=subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n(root/'descendant').write_text(str(child.pid))",
    );
    let (output, _) = Children::default()
        .dispatch(command, options(Duration::from_secs(3), 4096), None)
        .expect("leader exits successfully");
    assert!(output.status.success());
    fixture.assert_reaped("pid");
    fixture.assert_stopped("descendant");
}

#[cfg(target_os = "linux")]
#[test]
fn pipe_setup_failure_after_spawn_reclaims_the_process() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(fixture::PARKED);
    let children = Children {
        reject_pipe_setup: Some(fixture.path("pid")),
        ..Children::default()
    };
    let result = children.dispatch(command, options(Duration::from_secs(3), 4096), None);
    assert!(matches!(result, Err(Error::LostReply(_))), "{result:?}");
    fixture.assert_reaped("pid");
}

#[test]
fn clones_share_admission_and_release_a_completed_slot() {
    let fixture = fixture::Fixture::new();
    let children = Children::with_limit(1);
    let worker = children.clone();
    let parked = fixture.child(fixture::PARKED);
    let first = std::thread::spawn(move || {
        worker.dispatch(parked, options(Duration::from_secs(3), 4096), None)
    });
    fixture.wait_for("pid");
    let side_effect =
        || fixture.child("import pathlib, sys; pathlib.Path(sys.argv[1], 'ran').touch()");
    let extra = children.dispatch(side_effect(), options(Duration::from_secs(3), 4096), None);
    assert!(matches!(extra, Err(Error::Busy { limit: 1 })));
    assert!(!fixture.path("ran").exists());
    std::fs::write(fixture.path("release"), b"").expect("release first child");
    assert!(
        first
            .join()
            .expect("caller joins")
            .expect("first child finishes")
            .0
            .status
            .success()
    );
    assert!(
        children
            .dispatch(side_effect(), options(Duration::from_secs(3), 4096), None)
            .expect("slot reused")
            .0
            .status
            .success()
    );
    assert!(fixture.path("ran").exists());
}

#[test]
fn interrupted_creation_retains_only_a_complete_typed_receipt() {
    let fixture = fixture::Fixture::new();
    let socket = fixture.path("socket");
    for (bytes, created) in [
        ("%begin 1 2 0\\n123 456 %7 $8 @9\\n%end 1 2 0\\n", true),
        ("%begin 1 2 0\\n123 456 %7 $8 @9\\n%end 1 ", false),
    ] {
        let command = fixture.child(&format!(
            "import os,time\nos.write(1,b'{bytes}')\ntime.sleep(30)"
        ));
        let error = Children::default()
            .dispatch(
                command,
                options(Duration::from_millis(500), 4096),
                Some(socket.clone()),
            )
            .expect_err("reply stops before terminal exit");
        if created {
            let Error::CreationInterrupted { receipt, source } = error else {
                panic!("{error}")
            };
            assert_eq!(receipt.session().to_string(), "$8");
            assert_eq!(receipt.window().to_string(), "@9");
            assert_eq!(receipt.pane().id().to_string(), "%7");
            assert!(matches!(*source, Error::Deadline));
        } else {
            assert!(matches!(error, Error::Deadline));
            assert_eq!(error.effect(), Effect::Unknown);
        }
    }
}

#[test]
fn stdout_limit_keeps_a_receipt_already_inside_the_byte_budget() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child(
        "import os,time\nos.write(1,b'%begin 1 2 0\\n123 456 %7 $8 @9\\n%end 1 2 0\\n'+b'x'*8192)\ntime.sleep(30)",
    );
    let error = Children::default()
        .dispatch(
            command,
            options(Duration::from_secs(3), 4096),
            Some(fixture.path("socket")),
        )
        .expect_err("stdout exceeds its budget after creation");
    let Error::CreationInterrupted { receipt, source } = error else {
        panic!("{error}")
    };
    assert_eq!(receipt.session().to_string(), "$8");
    assert!(matches!(
        *source,
        Error::OutputLimit {
            stream: "stdout",
            limit: 4096
        }
    ));
}

#[test]
fn receipt_shaped_capture_bytes_remain_an_observation_failure() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child("import os,time\nos.write(1,b'%begin 1 2 0\\n123 456 %7 $8 @9\\n%end 1 2 0\\n')\ntime.sleep(30)");
    let error = Children::default()
        .dispatch(command, options(Duration::from_millis(500), 4096), None)
        .expect_err("capture reply has no terminal exit")
        .during_read(libtmux_core::CommandKind::CapturePane);
    assert_eq!(error.effect(), Effect::Observation);
    assert!(matches!(error, Error::ObservationFailed { .. }));
}
