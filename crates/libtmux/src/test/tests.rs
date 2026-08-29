/// A deadline widens for a loaded machine and never narrows.
///
/// The fixture's five seconds bound a tmux that starts with a core to
/// spare. Under a machine running several times its cores in work they
/// stop bounding startup and start deciding the result, which is the
/// failure `design.md` names and then leaves to a constant. A scale of
/// less than one would push it the wrong way, so it is refused rather
/// than honoured: nothing here is trying to make a fixture fail sooner.
#[test]
fn a_timeout_scale_only_ever_widens() {
    use super::{parse_timeout_scale, scaled};

    for (given, expected) in [
        (None, 1.0),
        (Some("2"), 2.0),
        (Some("  3.5  "), 3.5),
        // Anything that is not a number is a typo, not an instruction.
        (Some(""), 1.0),
        (Some("later"), 1.0),
        (Some("NaN"), 1.0),
        (Some("inf"), 1.0),
        // Narrowing is refused, not obeyed.
        (Some("0"), 1.0),
        (Some("0.25"), 1.0),
        (Some("-4"), 1.0),
    ] {
        assert!(
            (parse_timeout_scale(given) - expected).abs() < f64::EPSILON,
            "{given:?} should read as {expected}, got {}",
            parse_timeout_scale(given),
        );
    }

    // Unset, the deadline is exactly what it was before this existed.
    assert_eq!(scaled(Duration::from_secs(5)), Duration::from_secs(5));
}

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};
#[cfg(target_os = "linux")]
use rustix::process::{Signal, WaitOptions, getpgid, kill_process, waitpid};

use super::{
    CleanupOutcome, LeaderObservation, Lifecycle, OwnedFiles, TestServerBuilder,
    TestServerErrorKind, scaled, socket_path_fits_tmux,
};

#[cfg(target_os = "linux")]
const CONTAINMENT_FAILURE_CHILD: &str = "LIBTMUX_CONTAINMENT_FAILURE_CHILD";
#[cfg(target_os = "linux")]
struct ChildGuard {
    child: std::process::Child,
}

#[cfg(target_os = "linux")]
impl ChildGuard {
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

#[cfg(target_os = "linux")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn never_observe_fallback_cleans_an_exited_leaders_group() {
    let root = tempfile::tempdir().expect("fake executable directory is created");
    let executable = root.path().join("tmux");
    let pids = root.path().join("pids");
    let quoted_pids = format!("'{}'", pids.to_string_lossy().replace('\'', "'\\''"));
    let script = format!(
        "#!/bin/sh\n\
             if [ \"${{1-}}\" = '__libtmux_fixture_ready__' ]; then exit 0; fi\n\
             for argument in \"$@\"; do\n\
               if [ \"$argument\" = '-D' ]; then\n\
                 (trap '' TERM; sleep 86400 & wait) &\n\
                 helper=$!\n\
                 printf '%s\\n%s\\n' \"$$\" \"$helper\" > {quoted_pids}.new\n\
                 mv {quoted_pids}.new {quoted_pids}\n\
                 exit 23\n\
               fi\n\
             done\n\
             exit 1\n",
    );
    let mut file = fs::File::create(&executable).expect("fake executable is created");
    file.write_all(script.as_bytes())
        .expect("fake executable is written");
    file.sync_all().expect("fake executable is durable");
    drop(file);
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
        .expect("fake executable is executable");
    let executable_deadline = Instant::now() + scaled(Duration::from_secs(5));
    loop {
        match ProcessCommand::new(&executable)
            .arg("__libtmux_fixture_ready__")
            .status()
        {
            Ok(status) => {
                assert!(status.success(), "fake executable readiness failed");
                break;
            }
            Err(error)
                if error.raw_os_error() == Some(Errno::TXTBSY.raw_os_error())
                    && Instant::now() < executable_deadline =>
            {
                std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
            }
            Err(error) => panic!("fake executable readiness failed: {error}"),
        }
    }

    // The subject here is the daemon-exited path, not the clock. A
    // lifecycle timeout of the same magnitude as the observer interval
    // makes the two race, and on a loaded machine the clock wins: the
    // failure reads `StartupTimedOut`, which is a different path through
    // the code and says nothing about the one under test. The ceiling is
    // therefore far above the interval. It costs nothing, because a
    // daemon that exits is noticed when it exits.
    let error = TestServerBuilder::new()
        .tmux_executable(&executable)
        .lifecycle_timeout(Duration::from_secs(5))
        .start_with_leader_observer(
            |_| LeaderObservation::Unavailable,
            Some(Duration::from_millis(50)),
        )
        .await
        .expect_err("exited fallback leader is rejected");
    assert_eq!(error.kind(), TestServerErrorKind::DaemonExited);

    let published = fs::read_to_string(&pids).expect("leader publishes both PIDs");
    let pids = published
        .lines()
        .map(|value| value.parse::<i32>().expect("published PID is numeric"))
        .collect::<Vec<_>>();

    assert_eq!(pids.len(), 2);
    for raw_pid in pids {
        let pid = Pid::from_raw(raw_pid).expect("published PID is nonzero");
        let deadline = Instant::now() + scaled(Duration::from_secs(5));
        while !matches!(test_kill_process(pid), Err(Errno::SRCH)) {
            assert!(
                Instant::now() < deadline,
                "fallback process survives cleanup"
            );
            std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
        }
    }
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
#[test]
fn unavailable_observer_caps_oversized_grace_before_group_cleanup() {
    for timeout in [Duration::MAX, Duration::from_secs(100 * 365 * 24 * 60 * 60)] {
        let root = tempfile::tempdir().expect("fallback process directory is created");
        let pids_path = root.path().join("pids");
        let mut child = ProcessCommand::new("sh");
        child
            .arg("-c")
            .arg(
                "(trap '' TERM; sleep 86400 & wait) &\n\
                     helper=$!\n\
                     printf '%s\\n%s\\n' \"$$\" \"$helper\" > \"$PIDS.new\"\n\
                     mv \"$PIDS.new\" \"$PIDS\"\n\
                     exit 23\n",
            )
            .env("PIDS", &pids_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = child.spawn().expect("fallback leader starts");
        let leader = Pid::from_child(&child);
        let deadline = Instant::now() + scaled(Duration::from_secs(5));
        while super::leader_exited_unreaped(leader) != LeaderObservation::ExitedUnreaped {
            assert!(
                Instant::now() < deadline,
                "fallback leader exits before the observation deadline"
            );
            std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
        }
        let published =
            fs::read_to_string(&pids_path).expect("fallback leader atomically publishes both PIDs");
        let pids = published
            .lines()
            .map(|value| value.parse::<i32>().expect("published PID is numeric"))
            .collect::<Vec<_>>();
        let files = OwnedFiles::create().expect("owned files are prepared");
        let mut lifecycle = Lifecycle::new_with_leader_observer(
            child,
            files,
            |_| LeaderObservation::Unavailable,
            Some(Duration::from_millis(25)),
        );

        let started = Instant::now();
        assert!(matches!(
            lifecycle.cleanup(timeout),
            CleanupOutcome::Complete
        ));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "unobservable graceful cleanup uses its fallback ceiling"
        );
        assert_eq!(pids.len(), 2);
        for raw_pid in pids {
            let pid = Pid::from_raw(raw_pid).expect("published PID is nonzero");
            while !matches!(test_kill_process(pid), Err(Errno::SRCH)) {
                assert!(
                    Instant::now() < deadline,
                    "fallback process survives terminal group cleanup"
                );
                std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
            }
        }
    }
}

#[test]
fn setup_refuses_a_substituted_directory_entry() {
    let outside = tempfile::tempdir().expect("outside directory is created");
    let sentinel = outside.path().join("sentinel");
    fs::write(&sentinel, b"preserve").expect("outside sentinel is written");
    let mut renamed = None;
    let result = OwnedFiles::create_with_setup_hook(|directory| {
        let moved = directory.with_extension("renamed");
        fs::rename(directory, &moved).expect("owned directory is renamed");
        symlink(outside.path(), directory).expect("old name is substituted");
        renamed = Some((directory.to_path_buf(), moved));
    });
    let Some(error) = result.err() else {
        unreachable!("substituted setup path is rejected");
    };
    let (substitution, moved) = renamed.expect("hook records both paths");

    assert_eq!(error.kind(), TestServerErrorKind::FilesystemSetupFailed);
    assert_eq!(fs::read(&sentinel).expect("sentinel remains"), b"preserve");
    assert!(!outside.path().join(super::CONFIG_NAME).exists());

    fs::remove_file(substitution).expect("test removes its symlink");
    fs::remove_dir(moved).expect("test removes the retained directory");
}

#[test]
fn unproved_lifecycle_containment_retains_owned_files_on_drop() {
    let mut files = OwnedFiles::create().expect("owned files are prepared");
    let directory = files
        .socket_path
        .parent()
        .expect("socket has an owned directory")
        .to_path_buf();
    let config = files.config_path.clone();

    files.retain_until_contained();
    drop(files);

    assert!(directory.exists(), "unproved lifecycle retains its root");
    assert!(config.exists(), "unproved lifecycle retains its config");
    fs::remove_file(config).expect("test removes the retained config");
    fs::remove_file(directory.join(super::OWNER_NAME))
        .expect("test removes the retained owner record");
    fs::remove_dir(directory).expect("test removes the retained root");
}

#[test]
fn socket_limit_reserves_tmux_c_string_terminator() {
    let mut accepted = vec![b'a'; 1];
    while rustix::net::SocketAddrUnix::new(PathBuf::from(OsString::from_vec(accepted.clone())))
        .is_ok()
    {
        accepted.push(b'a');
    }
    accepted.pop();

    let full_sun_path = PathBuf::from(OsString::from_vec(accepted));
    let tmux_max = PathBuf::from(OsString::from_vec(
        full_sun_path.as_os_str().as_encoded_bytes()
            [..full_sun_path.as_os_str().as_encoded_bytes().len() - 1]
            .to_vec(),
    ));

    assert!(!socket_path_fits_tmux(&full_sun_path));
    assert!(socket_path_fits_tmux(&tmux_max));

    let with_nul = PathBuf::from(OsString::from_vec(b"socket\0tail".to_vec()));
    assert!(!socket_path_fits_tmux(&with_nul));
}

#[test]
fn final_wait_disarms_lifecycle_before_drop() {
    let files = OwnedFiles::create().expect("owned files are created");
    let mut command = ProcessCommand::new("sh");
    command.arg("-c").arg("exec sleep 86400").process_group(0);
    let child = command.spawn().expect("child starts in its own group");
    let mut lifecycle = Lifecycle::new(child, files);

    assert!(matches!(
        lifecycle.force_cleanup(),
        CleanupOutcome::Complete
    ));
    assert!(
        lifecycle.numeric_signaling_retired(),
        "a reaped lifecycle cannot signal its old group"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn externally_reaped_leader_does_not_signal_retired_process_group() {
    let files = OwnedFiles::create().expect("owned files are created");
    let directory = files
        .socket_path
        .parent()
        .expect("owned socket has a directory")
        .to_path_buf();
    let config = files.config_path.clone();
    let mut leader = ProcessCommand::new("sh");
    leader
        .arg("-c")
        .arg("exec sleep 86400")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = leader.spawn().expect("leader starts in its own group");
    let leader = Pid::from_child(&child);
    let mut helper = ProcessCommand::new("sh");
    helper
        .arg("-c")
        .arg("exec sleep 86400")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(leader.as_raw_pid());
    let mut helper = ChildGuard {
        child: helper
            .spawn()
            .expect("helper joins the leader process group"),
    };
    assert_eq!(
        getpgid(Some(Pid::from_child(&helper.child))).expect("helper has a process group"),
        leader,
    );
    let mut lifecycle = Lifecycle::new(child, files);

    kill_process(leader, Signal::KILL).expect("external owner kills the leader");
    let deadline = Instant::now() + scaled(Duration::from_secs(5));
    loop {
        match waitpid(Some(leader), WaitOptions::NOHANG) {
            Ok(Some((_pid, _status))) => break,
            Ok(None) | Err(Errno::INTR) if Instant::now() < deadline => {
                std::thread::sleep(super::CLEANUP_POLL_INTERVAL);
            }
            outcome => panic!("external owner could not reap the leader: {outcome:?}"),
        }
    }

    let outcome = lifecycle.force_cleanup();

    assert!(matches!(
        outcome,
        CleanupOutcome::LifecycleAndFilesystemFailed
    ));
    let survived_cleanup = helper.is_alive();
    drop(lifecycle);
    let survived_drop = helper.is_alive();
    let root_retained = directory.exists();
    let config_retained = config.exists();

    drop(helper);
    fs::remove_file(config).expect("test removes the retained config");
    fs::remove_dir_all(directory).expect("test removes the retained root");

    assert!(
        survived_cleanup,
        "lost child ownership retires numeric process-group signaling"
    );
    assert!(
        survived_drop,
        "lifecycle drop cannot rearm a lost process group"
    );
    assert!(root_retained, "lost ownership retains its root");
    assert!(config_retained, "lost ownership retains its config");
}

#[cfg(target_os = "linux")]
#[test]
fn containment_failure_cannot_rearm_a_reaped_leader() {
    if std::env::var_os(CONTAINMENT_FAILURE_CHILD).is_some() {
        containment_failure_cannot_rearm_a_reaped_leader_inner();
        return;
    }

    let executable = std::env::current_exe().expect("test executable path is available");
    let status = ProcessCommand::new("sh")
        .arg("-c")
        .arg(
            "ulimit -n 64; exec \"$1\" --exact \
                 test::tests::containment_failure_cannot_rearm_a_reaped_leader --nocapture",
        )
        .arg("sh")
        .arg(executable)
        .env(CONTAINMENT_FAILURE_CHILD, "1")
        .status()
        .expect("isolated containment-failure test starts");
    assert!(status.success(), "isolated containment-failure test passes");
}

#[cfg(target_os = "linux")]
fn containment_failure_cannot_rearm_a_reaped_leader_inner() {
    use rustix::process::{WaitOptions, waitpid};

    let files = OwnedFiles::create().expect("owned files are created");
    let directory = files
        .socket_path
        .parent()
        .expect("owned socket has a directory")
        .to_path_buf();
    let config = files.config_path.clone();
    let mut command = ProcessCommand::new("sh");
    command.arg("-c").arg("exec sleep 86400").process_group(0);
    let child = command.spawn().expect("child starts in its own group");
    let leader = Pid::from_child(&child);
    let mut lifecycle = Lifecycle::new(child, files);

    let mut descriptors = Vec::new();
    loop {
        match fs::File::open("/dev/null") {
            Ok(descriptor) => descriptors.push(descriptor),
            Err(error) if error.raw_os_error() == Some(Errno::MFILE.raw_os_error()) => {
                break;
            }
            Err(error) => panic!("descriptor exhaustion failed: {error}"),
        }
    }
    let Err(scan_error) = fs::read_dir("/proc") else {
        panic!("descriptor exhaustion must prevent containment scanning");
    };
    assert_eq!(scan_error.raw_os_error(), Some(Errno::MFILE.raw_os_error()));

    let outcome = lifecycle.force_cleanup();
    drop(descriptors);

    assert!(matches!(
        waitpid(Some(leader), WaitOptions::NOHANG),
        Err(Errno::CHILD)
    ));
    assert!(matches!(
        outcome,
        CleanupOutcome::LifecycleAndFilesystemFailed
    ));
    assert!(
        lifecycle.numeric_signaling_retired(),
        "a successful wait permanently retires numeric leader signaling"
    );

    drop(lifecycle);
    assert!(directory.exists(), "failed containment retains its root");
    assert!(config.exists(), "failed containment retains its config");
    fs::remove_file(config).expect("test removes the retained config");
    fs::remove_dir_all(directory).expect("test removes the retained root");
}
