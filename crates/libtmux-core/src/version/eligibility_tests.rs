use super::TmuxVersion;

#[test]
fn managed_control_requires_the_qualified_daemon_and_native_compatibility() {
    for (raw, daemon_eligible) in [
        ("3.3a", false),
        ("3.6b", false),
        ("next-3.7", false),
        ("3.7-rc", false),
        ("3.7-rc2", false),
        ("3.7", true),
        ("3.7d", true),
        ("3.8", true),
        ("next-3.8", true),
        ("master", false),
    ] {
        let version = TmuxVersion::parse_output(format!("tmux {raw}\n").as_bytes()).unwrap();
        assert_eq!(
            version.managed_control_eligible(true),
            daemon_eligible,
            "{raw}"
        );
        assert!(!version.managed_control_eligible(false), "{raw}");
    }
}

#[test]
fn ordinary_version_qualification_keeps_the_3_3a_floor() {
    let version = TmuxVersion::parse_output(b"tmux 3.3a\n").unwrap();
    assert!(version.ensure_supported().is_ok());
    assert!(!version.managed_control_eligible(true));
}

#[test]
fn ordinary_fences_keep_global_no_start_and_literal_lowering() {
    use std::ffi::OsString;

    let version = TmuxVersion::parse_output(b"tmux 3.3a\n").unwrap();
    let arguments = ["-N", "new-session", "-s", "literal\\;"]
        .map(OsString::from)
        .into();
    let fenced = version.fence_ordinary_argv(arguments);
    assert_eq!(
        fenced,
        [
            "-N",
            "if-shell",
            "-F",
            "#{==:#{version},3.3a}",
            "",
            "}",
            ";",
            "new-session",
            "-s",
            "literal\\;"
        ]
        .map(OsString::from)
    );
}

#[test]
fn ordinary_daemon_probe_distinguishes_cold_from_unqualified_endpoints() {
    use crate::{CreationNonce, Error, OrdinaryVersionProbe};

    let probe = OrdinaryVersionProbe::new(CreationNonce::from_bytes([0xab; 16]));
    let report = b"unknown command: libtmux-version-abababababababababababababababab-3.3a\n";
    let version = probe.decode(Some(1), b"", report, false).unwrap().unwrap();
    assert_eq!(version.raw(), "3.3a");
    for cold in [
        b"no server running on /socket\n".as_slice(),
        b"error connecting to /socket (No such file or directory)\n",
        b"error connecting to /socket (Connection refused)\n",
        b"error connecting to /socket (Socket operation on non-socket)\n",
    ] {
        assert!(probe.decode(Some(1), b"", cold, false).unwrap().is_none());
    }
    for unrelated in [
        b"error connecting to /socket (Permission denied)\n".as_slice(),
        b"protocol version mismatch\n",
        b"server exited unexpectedly\n",
        b"no server running on /socket\npermission denied\n",
    ] {
        assert!(matches!(
            probe.decode(Some(1), b"", unrelated, false),
            Err(Error::VersionProbeFailed { .. })
        ));
    }
    assert!(matches!(
        probe.decode(Some(1), b"", report, true),
        Err(Error::VersionProbeFailed { late: true, .. })
    ));
}

#[test]
fn executable_reports_keep_the_configured_3_3a_basename() {
    use std::ffi::OsStr;

    let executable = OsStr::new("/private/bin/tmux-3.3a");
    assert_eq!(
        TmuxVersion::parse_executable_output(executable, b"tmux-3.3a 3.3a\n")
            .unwrap()
            .raw(),
        "3.3a"
    );
    assert_eq!(
        TmuxVersion::parse_executable_output(executable, b"tmux 3.7\n")
            .unwrap()
            .raw(),
        "3.7"
    );
    assert!(TmuxVersion::parse_executable_output(executable, b"unrelated 3.3a\n").is_err());
}
