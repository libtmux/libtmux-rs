use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use libtmux_core::{Effect, Error};

use super::Children;
use crate::{CancellationToken, RequestOptions, Server};

#[path = "../../tests/common/supervision.rs"]
mod fixture;

fn options(duration: Duration, limit: usize) -> RequestOptions {
    RequestOptions {
        budget: fixture::budget(duration, limit),
        cancellation: CancellationToken::default(),
    }
}

#[derive(Debug, Default)]
pub(super) struct CleanupGate {
    entered: AtomicBool,
    released: Mutex<bool>,
    changed: Condvar,
    pub(super) fail: bool,
    panic: bool,
}

impl CleanupGate {
    pub(super) fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        let mut released = self.released.lock().unwrap();
        while !*released {
            released = self.changed.wait(released).unwrap();
        }
        drop(released);
        assert!(!self.panic, "injected supervisor panic before cleanup");
    }

    fn wait_for_owner(&self) {
        let end = Instant::now() + Duration::from_secs(3);
        while !self.entered.load(Ordering::Acquire) {
            assert!(Instant::now() < end, "owner did not enter cleanup");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

struct ReleaseCleanup(Arc<CleanupGate>);

impl Drop for ReleaseCleanup {
    fn drop(&mut self) {
        self.0.release();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_close_confirms_delayed_cleanup_across_clones() {
    let fixture = fixture::Fixture::new();
    let gate = Arc::new(CleanupGate::default());
    let release = ReleaseCleanup(gate.clone());
    let children = Children {
        cleanup_gate: Some(gate.clone()),
        ..Children::with_limit(1)
    };
    let mut server = Server::at(fixture.path("socket")).unwrap();
    server.children = children.clone();
    let clone = server.clone();
    let worker = children.clone();
    let command = fixture.child(fixture::PARKED);
    let request = options(Duration::from_secs(5), 4096);
    let cancellation = request.cancellation.clone();
    let caller = std::thread::spawn(move || worker.dispatch(command, request, None));
    fixture.wait_for("pid");
    cancellation.cancel();
    gate.wait_for_owner();
    let result = caller.join().unwrap();
    assert!(
        matches!(result, Err(Error::CleanupPending(_))),
        "{result:?}"
    );
    let side_effect =
        || fixture.child("import pathlib, sys; pathlib.Path(sys.argv[1], 'ran').touch()");
    assert!(matches!(
        children.dispatch(side_effect(), options(Duration::from_secs(3), 4096), None),
        Err(Error::Busy { limit: 1 })
    ));
    let started = Instant::now();
    let pending = server.close_ordinary();
    assert!(
        matches!(pending, Err(Error::CleanupPending(_))),
        "{pending:?}"
    );
    assert_eq!(pending.unwrap_err().effect(), Effect::Unknown);
    assert!(started.elapsed() < Duration::from_secs(3));
    let pid = std::fs::read_to_string(fixture.path("pid")).unwrap();
    assert!(std::path::Path::new("/proc").join(pid.trim()).exists());
    assert!(matches!(
        clone
            .children
            .dispatch(side_effect(), options(Duration::from_secs(3), 4096), None),
        Err(Error::Closed)
    ));
    assert!(!fixture.path("ran").exists());
    let closer = std::thread::spawn(move || clone.close_ordinary());
    let repeated = server.close_ordinary();
    assert!(
        matches!(repeated, Err(Error::CleanupPending(_))),
        "{repeated:?}"
    );
    assert!(matches!(
        closer.join().unwrap(),
        Err(Error::CleanupPending(_))
    ));
    drop(release);
    server.close_ordinary().expect("real child is reaped");
    fixture.assert_reaped("pid");
    server.close_ordinary().expect("completion is repeatable");
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_close_retains_cleanup_failure_after_the_caller_returns() {
    let fixture = fixture::Fixture::new();
    let gate = Arc::new(CleanupGate {
        fail: true,
        ..CleanupGate::default()
    });
    let release = ReleaseCleanup(gate.clone());
    let children = Children {
        cleanup_gate: Some(gate.clone()),
        ..Children::with_limit(1)
    };
    let mut server = Server::at(fixture.path("socket")).unwrap();
    server.children = children.clone();
    let command = fixture.child(fixture::PARKED);
    let request = options(Duration::from_secs(5), 4096);
    let cancellation = request.cancellation.clone();
    let caller = std::thread::spawn(move || children.dispatch(command, request, None));
    fixture.wait_for("pid");
    cancellation.cancel();
    gate.wait_for_owner();
    assert!(matches!(
        caller.join().unwrap(),
        Err(Error::CleanupPending(_))
    ));
    assert!(matches!(
        server.close_ordinary(),
        Err(Error::CleanupPending(_))
    ));
    drop(release);
    for holder in [&server, &server.clone()] {
        let error = holder
            .close_ordinary()
            .expect_err("late cleanup failure survives its caller");
        let Error::Shutdown(summary) = error else {
            panic!("{error}")
        };
        assert!(
            summary.contains("injected cleanup signal failure"),
            "{summary}"
        );
        assert!(summary.len() <= 1024, "cleanup history is bounded");
    }
    fixture.assert_reaped("pid");
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_close_retains_ownership_and_failure_when_the_worker_panics() {
    let fixture = fixture::Fixture::new();
    let gate = Arc::new(CleanupGate {
        panic: true,
        ..CleanupGate::default()
    });
    let release = ReleaseCleanup(gate.clone());
    let children = Children {
        cleanup_gate: Some(gate.clone()),
        ..Children::with_limit(1)
    };
    let mut server = Server::at(fixture.path("socket")).unwrap();
    server.children = children.clone();
    let command = fixture.child(fixture::PARKED);
    let request = options(Duration::from_secs(5), 4096);
    let cancellation = request.cancellation.clone();
    let caller = std::thread::spawn(move || children.dispatch(command, request, None));
    fixture.wait_for("pid");
    cancellation.cancel();
    gate.wait_for_owner();
    assert!(matches!(
        caller.join().unwrap(),
        Err(Error::CleanupPending(_))
    ));
    assert!(matches!(
        server.close_ordinary(),
        Err(Error::CleanupPending(_))
    ));
    drop(release);
    for holder in [&server, &server.clone()] {
        let error = holder
            .close_ordinary()
            .expect_err("worker unwind is retained after reaping");
        assert!(
            matches!(error, Error::Shutdown(ref cause) if cause == "subprocess supervisor panicked"),
            "{error}"
        );
    }
    fixture.assert_reaped("pid");
}

#[test]
fn ordinary_close_allows_an_admitted_request_to_finish() {
    let fixture = fixture::Fixture::new();
    let children = Children::with_limit(1);
    let worker = children.clone();
    let command = fixture.child(fixture::PARKED);
    let caller = std::thread::spawn(move || {
        worker.dispatch(command, options(Duration::from_secs(5), 4096), None)
    });
    fixture.wait_for("pid");
    let mut server = Server::at(fixture.path("socket")).unwrap();
    server.children = children.clone();
    let closer = server.clone();
    let close = std::thread::spawn(move || closer.close_ordinary());
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        let refused = children.dispatch(
            fixture.child("import pathlib, sys; pathlib.Path(sys.argv[1], 'ran').touch()"),
            options(Duration::from_secs(3), 4096),
            None,
        );
        match refused {
            Err(Error::Closed) => break,
            Err(Error::Busy { limit: 1 }) => {
                assert!(Instant::now() < end, "close did not stop admission");
                std::thread::sleep(Duration::from_millis(2));
            }
            other => panic!("request was not refused: {other:?}"),
        }
    }
    assert!(!fixture.path("ran").exists());
    std::fs::write(fixture.path("release"), b"").unwrap();
    assert!(caller.join().unwrap().unwrap().0.status.success());
    close
        .join()
        .unwrap()
        .expect("admitted child finishes without cancellation");
    assert!(matches!(server.generation(), Err(Error::Closed)));
    assert!(matches!(
        server
            .clone()
            .new_session(libtmux_core::NewSession::new("closed")),
        Err(Error::Closed)
    ));
    server.close_ordinary().unwrap();
}

#[test]
fn ordinary_close_and_concurrent_admission_have_one_order_across_clones() {
    let fixture = fixture::Fixture::new();
    let mut server = Server::at(fixture.path("socket")).unwrap();
    server.children = Children::with_limit(8);
    let start = Arc::new(std::sync::Barrier::new(9));
    let workers: Vec<_> = (0..8)
        .map(|index| {
            let clone = server.clone();
            let start = start.clone();
            let command = fixture.child(&format!(
                "import pathlib, sys; pathlib.Path(sys.argv[1], 'ran-{index}').touch()"
            ));
            std::thread::spawn(move || {
                start.wait();
                clone
                    .children
                    .dispatch(command, options(Duration::from_secs(3), 4096), None)
            })
        })
        .collect();
    start.wait();
    server
        .close_ordinary()
        .expect("all earlier admissions finish");
    for (index, worker) in workers.into_iter().enumerate() {
        match worker.join().unwrap() {
            Ok((output, false)) => {
                assert!(output.status.success());
                assert!(fixture.path(&format!("ran-{index}")).exists());
            }
            Err(Error::Closed) => assert!(!fixture.path(&format!("ran-{index}")).exists()),
            other => panic!("admission and close disagreed: {other:?}"),
        }
    }
    server.close_ordinary().unwrap();
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
fn version_probes_discard_stdout_before_spawning_the_client() {
    let fixture = fixture::Fixture::new();
    let command = fixture.child("import os\nos.write(1,b'x'*131072)\nos.write(2,b'version')");
    let (output, late) = Children::default()
        .dispatch_probe(command, options(Duration::from_secs(3), 16))
        .expect("stdout cannot retain or overflow a version probe");
    assert!(output.status.success());
    assert!(!late);
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, b"version");
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
