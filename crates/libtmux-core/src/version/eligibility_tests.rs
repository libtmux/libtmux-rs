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
