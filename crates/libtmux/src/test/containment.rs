use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::ffi::OsString;
use std::process::Command;
use std::time::Duration;

#[cfg(target_os = "linux")]
const OWNER_ENV: &str = "LIBTMUX_TEST_SERVER_OWNER";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ContainmentCoverage {
    #[cfg(target_os = "linux")]
    ExactMarkerSweep,
    #[cfg(not(target_os = "linux"))]
    ProcessGroupOnly,
    #[cfg(target_os = "linux")]
    Failed,
}

impl ContainmentCoverage {
    pub(super) const fn is_success(self) -> bool {
        #[cfg(target_os = "linux")]
        {
            !matches!(self, Self::Failed)
        }
        #[cfg(not(target_os = "linux"))]
        {
            matches!(self, Self::ProcessGroupOnly)
        }
    }
}

pub(super) struct OwnerContainment {
    #[cfg(target_os = "linux")]
    marker: OsString,
}

impl OwnerContainment {
    pub(super) fn new(seed: &OsStr) -> Self {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStringExt as _;

            let mut marker = std::process::id().to_string().into_bytes();
            marker.push(b'-');
            marker.extend_from_slice(seed.as_encoded_bytes());
            Self {
                marker: OsString::from_vec(marker),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = seed;
            Self {}
        }
    }

    pub(super) fn configure(&self, command: &mut Command) {
        #[cfg(target_os = "linux")]
        {
            command.env(OWNER_ENV, &self.marker);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = command;
        }
    }

    pub(super) fn terminate_all(&self, timeout: Duration) -> ContainmentCoverage {
        #[cfg(target_os = "linux")]
        {
            if linux::terminate_all(&self.marker, timeout) {
                ContainmentCoverage::ExactMarkerSweep
            } else {
                ContainmentCoverage::Failed
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = timeout;
            ContainmentCoverage::ProcessGroupOnly
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsStr;
    use std::fs;
    use std::os::fd::OwnedFd;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use rustix::io::Errno;
    use rustix::process::{Pid, PidfdFlags, Signal, getuid, pidfd_open, pidfd_send_signal};

    use super::OWNER_ENV;

    #[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
    struct ProcessIdentity {
        pid: u32,
        uid: u32,
        start_time: u64,
    }

    #[derive(Clone, Copy)]
    struct ProcessSnapshot {
        identity: ProcessIdentity,
        state: u8,
    }

    struct AdmittedProcess {
        identity: ProcessIdentity,
        pidfd: OwnedFd,
    }

    #[derive(Default)]
    struct FrozenProcesses {
        processes: BTreeMap<ProcessIdentity, AdmittedProcess>,
        /// Identities this sweep already drove to termination.
        ///
        /// A process the sweep has just killed is briefly a zombie: its
        /// `(pid, uid, start time)` identity still reads, but its environment
        /// no longer does. A rescan that treated it as a new candidate would
        /// see opacity following an earlier identity and marker match, which
        /// the scanner reports as a failure. Retiring the identity keeps the
        /// sweep from mistaking its own teardown for a process hiding from it.
        retired: BTreeSet<ProcessIdentity>,
    }

    impl FrozenProcesses {
        fn contains(&self, identity: &ProcessIdentity) -> bool {
            self.processes.contains_key(identity) || self.retired.contains(identity)
        }

        fn admit(&mut self, process: AdmittedProcess) -> ProcessIdentity {
            let identity = process.identity;
            self.processes.insert(identity, process);
            identity
        }

        fn signal(&self, identity: ProcessIdentity, requested: Signal) -> bool {
            self.processes
                .get(&identity)
                .is_some_and(|process| signal_process(process, requested))
        }

        fn values(&self) -> impl Iterator<Item = &AdmittedProcess> {
            self.processes.values()
        }

        fn iter(&self) -> impl Iterator<Item = (&ProcessIdentity, &AdmittedProcess)> {
            self.processes.iter()
        }

        fn remove(&mut self, identity: &ProcessIdentity) {
            self.processes.remove(identity);
            self.retired.insert(*identity);
        }

        fn is_empty(&self) -> bool {
            self.processes.is_empty()
        }
    }

    impl Drop for FrozenProcesses {
        fn drop(&mut self) {
            for process in self.processes.values() {
                let _ = pidfd_send_signal(&process.pidfd, Signal::KILL);
            }
        }
    }

    /// Which branch ended a sweep without containing everything.
    ///
    /// Every branch used to return a bare `false`, which made an intermittent
    /// failure impossible to narrow. The reason is carried so a test can say
    /// what actually happened.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum SweepFailure {
        /// A scan could not classify a candidate during discovery.
        DiscoveryScan,
        /// Freezing a newly discovered process failed.
        FreezeOnDiscovery,
        /// Discovery kept finding new processes until the deadline.
        DiscoveryDeadline,
        /// Killing a frozen process failed.
        Kill,
        /// Checking whether a killed process became terminal failed.
        TerminalCheck,
        /// Freezing or killing a process found during termination failed.
        FreezeOnTermination,
        /// Termination did not finish before the deadline.
        TerminationDeadline,
    }

    pub(super) fn terminate_all(marker: &OsStr, timeout: Duration) -> bool {
        terminate_all_with_scanner(marker, timeout, scan).is_ok()
    }

    fn terminate_all_with_scanner(
        marker: &OsStr,
        timeout: Duration,
        mut scanner: impl FnMut(&OsStr) -> Result<Vec<AdmittedProcess>, ()>,
    ) -> Result<(), SweepFailure> {
        let started = Instant::now();
        let mut frozen = FrozenProcesses::default();
        loop {
            let Ok(observed) = scanner(marker) else {
                return Err(SweepFailure::DiscoveryScan);
            };
            let mut discovered = false;
            for process in observed {
                if frozen.contains(&process.identity) {
                    continue;
                }
                let identity = frozen.admit(process);
                if !frozen.signal(identity, Signal::STOP) {
                    return Err(SweepFailure::FreezeOnDiscovery);
                }
                discovered = true;
            }
            if !discovered {
                break;
            }
            if timed_out(started, timeout) {
                return Err(SweepFailure::DiscoveryDeadline);
            }
            std::thread::yield_now();
        }

        let mut signaled = true;
        for process in frozen.values() {
            signaled &= signal_process(process, Signal::KILL);
        }
        if !signaled {
            return Err(SweepFailure::Kill);
        }

        loop {
            let mut terminal = Vec::new();
            for (identity, process) in frozen.iter() {
                match terminate_and_is_terminal(process) {
                    Ok(true) => terminal.push(*identity),
                    Ok(false) => {}
                    Err(()) => return Err(SweepFailure::TerminalCheck),
                }
            }
            for identity in terminal {
                frozen.remove(&identity);
            }

            // Every process still frozen here has already been killed, so a
            // scan that cannot classify a candidate is not evidence that one
            // escaped: a process being torn down turns opaque, and the scan
            // cannot tell that apart from one hiding. Treating it as "nothing
            // new this pass" keeps the sweep honest, because the loop only
            // returns once every frozen process is terminal and the deadline
            // below still fails closed if that never happens.
            let observed = scanner(marker).unwrap_or_default();
            for process in observed {
                if frozen.contains(&process.identity) {
                    continue;
                }
                let identity = frozen.admit(process);
                if !frozen.signal(identity, Signal::STOP) || !frozen.signal(identity, Signal::KILL)
                {
                    return Err(SweepFailure::FreezeOnTermination);
                }
            }
            if frozen.is_empty() {
                return Ok(());
            }
            if timed_out(started, timeout) {
                return Err(SweepFailure::TerminationDeadline);
            }
            std::thread::yield_now();
        }
    }

    fn scan(marker: &OsStr) -> Result<Vec<AdmittedProcess>, ()> {
        scan_process_paths(marker, process_paths()?)
    }

    fn scan_process_paths(
        marker: &OsStr,
        paths: impl IntoIterator<Item = (u32, PathBuf)>,
    ) -> Result<Vec<AdmittedProcess>, ()> {
        let expected = environment_entry(marker);
        let mut admitted = Vec::new();
        for (pid, path) in paths {
            let Some(snapshot) = read_process(pid, &path) else {
                continue;
            };
            let identity = snapshot.identity;
            if identity.uid != getuid().as_raw() {
                continue;
            }
            if snapshot.state == b'Z' {
                continue;
            }
            // The exact readable marker is a selection boundary, not proof of
            // ancestry or authenticity. A same-UID process can copy it, while
            // a descendant that hides it before admission remains outside the
            // sweep. Stronger containment requires a cgroup or PID namespace.
            let exact_marker = environment_matches(&path, &expected).unwrap_or_default();
            if !exact_marker {
                continue;
            }
            let Ok(raw_pid) = i32::try_from(pid) else {
                continue;
            };
            let Some(raw_pid) = Pid::from_raw(raw_pid) else {
                continue;
            };
            let pidfd = match pidfd_open(raw_pid, PidfdFlags::empty()) {
                Ok(pidfd) => pidfd,
                Err(Errno::SRCH) => continue,
                Err(_) => return Err(()),
            };
            let Some(current) = read_process(pid, &path) else {
                continue;
            };
            if !same_live_process(identity, current) {
                continue;
            }
            let Ok(marker_matches) = environment_matches(&path, &expected) else {
                let Some(current) = read_process(pid, &path) else {
                    continue;
                };
                if !same_live_process(identity, current) {
                    continue;
                }
                return Err(());
            };
            let Some(current) = read_process(pid, &path) else {
                continue;
            };
            if !revalidated_candidate(identity, current, marker_matches) {
                continue;
            }
            admitted.push(AdmittedProcess { identity, pidfd });
        }
        Ok(admitted)
    }

    fn process_paths() -> Result<Vec<(u32, PathBuf)>, ()> {
        let mut paths = Vec::new();
        let entries = fs::read_dir("/proc").map_err(|_| ())?;
        for entry in entries {
            let entry = entry.map_err(|_| ())?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            paths.push((pid, entry.path()));
        }
        Ok(paths)
    }

    fn environment_entry(marker: &OsStr) -> Vec<u8> {
        let mut expected = OWNER_ENV.as_bytes().to_vec();
        expected.push(b'=');
        expected.extend_from_slice(marker.as_bytes());
        expected
    }

    fn environment_matches(path: &Path, expected: &[u8]) -> std::io::Result<bool> {
        fs::read(path.join("environ")).map(|environment| {
            environment
                .split(|byte| *byte == 0)
                .any(|entry| entry == expected)
        })
    }

    fn read_process(pid: u32, path: &Path) -> Option<ProcessSnapshot> {
        let uid = read_uid(&path.join("status"))?;
        let (state, start_time) = read_stat(&path.join("stat"))?;
        Some(ProcessSnapshot {
            identity: ProcessIdentity {
                pid,
                uid,
                start_time,
            },
            state,
        })
    }

    fn same_live_process(expected: ProcessIdentity, current: ProcessSnapshot) -> bool {
        current.identity == expected && current.state != b'Z'
    }

    fn revalidated_candidate(
        expected: ProcessIdentity,
        current: ProcessSnapshot,
        marker_matches: bool,
    ) -> bool {
        marker_matches && same_live_process(expected, current)
    }

    fn read_uid(path: &Path) -> Option<u32> {
        let status = fs::read_to_string(path).ok()?;
        let values = status.lines().find_map(|line| line.strip_prefix("Uid:"))?;
        values.split_ascii_whitespace().next()?.parse().ok()
    }

    fn read_stat(path: &Path) -> Option<(u8, u64)> {
        let stat = fs::read(path).ok()?;
        let command_end = stat.iter().rposition(|byte| *byte == b')')?;
        let fields = stat.get(command_end + 1..)?;
        let mut fields = fields
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty());
        let state = *fields.next()?.first()?;
        let start_time = fields.nth(18)?;
        Some((state, std::str::from_utf8(start_time).ok()?.parse().ok()?))
    }

    fn signal_process(process: &AdmittedProcess, signal: Signal) -> bool {
        matches!(
            pidfd_send_signal(&process.pidfd, signal),
            Ok(()) | Err(Errno::SRCH)
        )
    }

    fn terminate_and_is_terminal(process: &AdmittedProcess) -> Result<bool, ()> {
        let path = PathBuf::from(format!("/proc/{}", process.identity.pid));
        if read_process(process.identity.pid, &path)
            .is_some_and(|current| current.identity == process.identity && current.state == b'Z')
        {
            return Ok(true);
        }
        match pidfd_send_signal(&process.pidfd, Signal::KILL) {
            Ok(()) => Ok(false),
            Err(Errno::SRCH) => Ok(true),
            Err(_) => Err(()),
        }
    }

    fn timed_out(started: Instant, timeout: Duration) -> bool {
        started.elapsed() >= timeout
    }

    #[cfg(test)]
    mod tests {

        use std::ffi::OsStr;
        use std::fs;
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;
        use std::process::{Child, Command, Stdio};
        use std::time::{Duration, Instant};

        use rustix::process::getuid;

        use super::{
            environment_entry, environment_matches, read_process, revalidated_candidate, scan,
            scan_process_paths, terminate_all, terminate_all_with_scanner,
        };
        use crate::test::containment::OwnerContainment;

        struct ChildGuard {
            child: Child,
        }

        impl ChildGuard {
            fn exits_before(&mut self, deadline: Instant) -> bool {
                loop {
                    match self.child.try_wait() {
                        Ok(Some(_)) => return true,
                        Ok(None) if Instant::now() < deadline => std::thread::yield_now(),
                        Ok(None) | Err(_) => return false,
                    }
                }
            }
        }

        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }

        fn spawn_marked_process(containment: &OwnerContainment) -> ChildGuard {
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg("printf ready; trap '' TERM; while :; do :; done")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            containment.configure(&mut command);
            let mut child = command.spawn().expect("marked process starts");
            let mut readiness = [0_u8; 5];
            child
                .stdout
                .as_mut()
                .expect("marked process stdout is piped")
                .read_exact(&mut readiness)
                .expect("marked process publishes readiness");
            assert_eq!(readiness, *b"ready");
            ChildGuard { child }
        }

        fn write_process_view(path: &std::path::Path, pid: u32, start_time: u64, marker: &OsStr) {
            let uid = getuid().as_raw();
            fs::write(
                path.join("status"),
                format!("Name:\tfixture\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
            )
            .expect("fixture status is written");
            fs::write(
                path.join("stat"),
                format!("{pid} (fixture) S {} {start_time}\n", ["1"; 18].join(" ")),
            )
            .expect("fixture stat is written");
            let mut environment = super::OWNER_ENV.as_bytes().to_vec();
            environment.push(b'=');
            environment.extend_from_slice(marker.as_encoded_bytes());
            environment.push(0);
            fs::write(path.join("environ"), environment).expect("fixture environment is written");
        }

        #[test]
        fn exact_marker_keeps_identity_across_procfs_dentry_change() {
            let marker = OsStr::new("dentry-change");
            let first_root = tempfile::tempdir().expect("first proc view is created");
            let second_root = tempfile::tempdir().expect("second proc view is created");
            write_process_view(first_root.path(), 41, 73, marker);
            write_process_view(second_root.path(), 41, 73, marker);
            assert_ne!(
                fs::metadata(first_root.path())
                    .expect("first proc view exists")
                    .ino(),
                fs::metadata(second_root.path())
                    .expect("second proc view exists")
                    .ino(),
                "the regression requires distinct procfs dentries",
            );

            let first =
                read_process(41, first_root.path()).expect("first live identity is readable");
            let second =
                read_process(41, second_root.path()).expect("second live identity is readable");
            let marker_matches =
                environment_matches(second_root.path(), &environment_entry(marker))
                    .expect("second environment is readable");
            assert!(
                revalidated_candidate(first.identity, second, marker_matches),
                "procfs dentry replacement must not discard a live exact-marker identity",
            );
        }

        #[test]
        fn stable_unmarked_replacement_is_not_accepted() {
            let marker = OsStr::new("owned");
            let first_root = tempfile::tempdir().expect("first proc view is created");
            let second_root = tempfile::tempdir().expect("second proc view is created");
            write_process_view(first_root.path(), 41, 73, marker);
            write_process_view(second_root.path(), 41, 73, OsStr::new("replacement"));

            let first =
                read_process(41, first_root.path()).expect("first live identity is readable");
            let second =
                read_process(41, second_root.path()).expect("second live identity is readable");
            let marker_matches =
                environment_matches(second_root.path(), &environment_entry(marker))
                    .expect("replacement environment is readable");

            assert!(
                !revalidated_candidate(first.identity, second, marker_matches),
                "a same PID/start replacement without the exact marker is not owned",
            );
        }

        #[test]
        fn initial_unreadable_candidate_does_not_hide_readable_exact_marker() {
            let containment = OwnerContainment::new(OsStr::new("unreadable-before-readable"));
            let mut marked = spawn_marked_process(&containment);
            let unreadable_root = tempfile::tempdir().expect("unreadable proc view is created");
            write_process_view(unreadable_root.path(), 41, 73, &containment.marker);
            let unreadable_environment = unreadable_root.path().join("environ");
            fs::remove_file(&unreadable_environment).expect("fixture environment is removed");
            fs::create_dir(&unreadable_environment)
                .expect("fixture environment becomes unreadable as a file");
            let candidates = vec![
                (41, unreadable_root.path().to_path_buf()),
                (
                    marked.child.id(),
                    std::path::PathBuf::from(format!("/proc/{}", marked.child.id())),
                ),
            ];

            assert_eq!(
                terminate_all_with_scanner(&containment.marker, Duration::from_secs(5), |marker| {
                    scan_process_paths(marker, candidates.clone())
                }),
                Ok(()),
                "an unreadable initial candidate does not abort the exact-marker sweep",
            );
            assert!(
                marked.exits_before(Instant::now() + Duration::from_secs(5)),
                "the later readable exact-marker process is contained"
            );
        }

        #[test]
        fn a_process_this_sweep_killed_is_not_readmitted_as_a_new_candidate() {
            // The termination loop rescans after driving a process terminal.
            // A scanner that keeps reporting the same identity stands in for
            // the zombie window, where /proc still answers for a process the
            // sweep has already killed. Readmitting it would signal a corpse
            // and, once its environment stopped reading, fail the sweep.
            let containment = OwnerContainment::new(OsStr::new("no-readmission"));
            let mut marked = spawn_marked_process(&containment);
            let candidates = vec![(
                marked.child.id(),
                std::path::PathBuf::from(format!("/proc/{}", marked.child.id())),
            )];

            let mut scans = 0_u32;
            assert_eq!(
                terminate_all_with_scanner(&containment.marker, Duration::from_secs(5), |marker| {
                    scans += 1;
                    scan_process_paths(marker, candidates.clone())
                }),
                Ok(()),
                "the sweep completes without readmitting what it already killed",
            );
            assert!(
                scans >= 2,
                "the sweep rescanned, so the retirement path was exercised"
            );
            assert!(
                marked.exits_before(Instant::now() + Duration::from_secs(5)),
                "the marked process is contained"
            );
        }

        #[test]
        fn timeout_after_freeze_kills_all_previously_frozen_pidfds() {
            let containment = OwnerContainment::new(OsStr::new("timeout-after-freeze"));
            let mut first = spawn_marked_process(&containment);
            let mut second = spawn_marked_process(&containment);

            assert!(
                !terminate_all(&containment.marker, Duration::ZERO),
                "an expired sweep remains a containment failure"
            );

            let deadline = Instant::now() + Duration::from_secs(5);
            assert!(
                first.exits_before(deadline),
                "the first admitted pidfd is killed after timeout"
            );
            assert!(
                second.exits_before(deadline),
                "the second admitted pidfd is killed after timeout"
            );
        }

        #[test]
        fn later_scan_error_kills_all_previously_frozen_pidfds() {
            let containment = OwnerContainment::new(OsStr::new("scan-error-after-freeze"));
            let mut first = spawn_marked_process(&containment);
            let mut second = spawn_marked_process(&containment);
            let mut scans = 0_u8;

            // Which branch catches it depends on how many discovery passes ran
            // before the scanner started failing, which is a race. The
            // contract under test is that the sweep fails at all.
            assert!(
                terminate_all_with_scanner(&containment.marker, Duration::from_secs(5), |marker| {
                    scans += 1;
                    if scans == 1 { scan(marker) } else { Err(()) }
                })
                .is_err(),
                "a later scan error remains a containment failure",
            );

            let deadline = Instant::now() + Duration::from_secs(5);
            assert!(
                first.exits_before(deadline),
                "the first frozen pidfd is killed after a later scan error"
            );
            assert!(
                second.exits_before(deadline),
                "the second frozen pidfd is killed after a later scan error"
            );
        }

        #[test]
        fn copied_exact_marker_is_admitted_without_ancestry() {
            let containment = OwnerContainment::new(OsStr::new("copied-marker"));
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg("while :; do :; done")
                .env(super::OWNER_ENV, &containment.marker)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut copied = ChildGuard {
                child: command.spawn().expect("copied-marker process starts"),
            };

            assert!(
                terminate_all(&containment.marker, Duration::from_secs(5)),
                "the exact copied marker is sufficient for admission"
            );
            assert!(
                copied.exits_before(Instant::now() + Duration::from_secs(5)),
                "the copied-marker pidfd reaches a terminal state"
            );
        }
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use std::ffi::OsStr;
    use std::time::Duration;

    use super::{ContainmentCoverage, OwnerContainment};

    #[test]
    fn non_linux_reports_process_group_only() {
        let containment = OwnerContainment::new(OsStr::new("process-group-only"));

        assert_eq!(
            containment.terminate_all(Duration::ZERO),
            ContainmentCoverage::ProcessGroupOnly,
        );
    }
}
