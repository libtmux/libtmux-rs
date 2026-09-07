//! Authenticated route, recovery-ledger, and lock contract tests.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::process::Command;

use mcp_swap::config::Scope;
use mcp_swap::fs::{ArtifactClaim, reject_aliases, resolve_config_route, stable_snapshot};
use mcp_swap::lock::TransactionLock;
use mcp_swap::recovery::{
    Ledger, RecoveryEntry, STATE_MAX_BYTES, ledger_bytes, load_ledger, state_key,
};
use tempfile::tempdir;

#[test]
fn config_route_rejects_a_replaced_symlink() {
    let root = tempdir().expect("temporary root");
    let first = root.path().join("first.json");
    let second = root.path().join("second.json");
    let config = root.path().join("config.json");
    fs::write(&first, b"{}\n").expect("first config");
    fs::write(&second, b"{}\n").expect("second config");
    symlink(&first, &config).expect("config link");
    let route = resolve_config_route(&config, 1024).expect("stable route");

    fs::remove_file(&config).expect("remove old link");
    symlink(&second, &config).expect("replacement link");

    assert!(
        route
            .verify()
            .expect_err("route changed")
            .to_string()
            .contains("changed")
    );
    assert_eq!(fs::read(&second).expect("replacement survives"), b"{}\n");
}

#[test]
fn relative_config_symlink_uses_its_physical_parent() {
    let root = tempdir().expect("temporary root");
    let logical_home = root.path().join("logical-home");
    let physical_home = root.path().join("physical-home");
    let physical_parent = physical_home.join("client");
    fs::create_dir_all(&logical_home).expect("logical home");
    fs::create_dir_all(&physical_parent).expect("physical parent");
    let logical_parent = logical_home.join("client");
    symlink(&physical_parent, &logical_parent).expect("logical parent link");
    let logical_sibling = logical_home.join("shared.json");
    let physical_sibling = physical_home.join("shared.json");
    fs::write(&logical_sibling, b"lexical\n").expect("lexical sibling");
    fs::write(&physical_sibling, b"physical\n").expect("physical sibling");
    let config = logical_parent.join("config.json");
    symlink("../shared.json", physical_parent.join("config.json")).expect("relative config link");

    let route = resolve_config_route(&config, 1024).expect("relative route");
    route.verify().expect("stable relative route");
    assert_eq!(route.links[0].path, config);
    assert_eq!(
        route.links[0].target,
        std::path::Path::new("../shared.json")
    );
    assert_eq!(route.anchors[0].logical, logical_parent);
    assert_eq!(
        route.anchors[0].physical,
        physical_parent.canonicalize().expect("physical parent")
    );

    fs::write(&route.target, b"updated\n").expect("write resolved target");
    assert_eq!(
        fs::read(&logical_sibling).expect("unchanged lexical sibling"),
        b"lexical\n"
    );
    assert_eq!(
        fs::read(&physical_sibling).expect("updated physical sibling"),
        b"updated\n"
    );
    // Canonical, like `anchors[0].physical` above: macOS resolves the
    // temporary root to `/private/var`, and resolving is the library's job.
    assert_eq!(
        route.target,
        physical_sibling.canonicalize().expect("physical sibling")
    );
}

#[test]
fn physical_aliases_are_refused_even_through_hard_links() {
    let root = tempdir().expect("temporary root");
    let first = root.path().join("first");
    let second = root.path().join("second");
    fs::write(&first, b"config").expect("config");
    fs::hard_link(&first, &second).expect("hard link");
    let first = resolve_config_route(&first, 1024).expect("first route");
    let second = resolve_config_route(&second, 1024).expect("second route");

    let error = reject_aliases(&[
        ArtifactClaim::from_route("first", &first),
        ArtifactClaim::from_route("second", &second),
    ])
    .expect_err("same inode must be rejected");

    assert!(error.to_string().contains("alias"));
}

#[test]
fn ledger_checksum_and_size_are_authenticated() {
    let root = tempdir().expect("temporary root");
    let state = root.path().join("state.json");
    write_ledger(&state, &Ledger::default());
    let original = fs::read(&state).expect("ledger bytes");

    let mut tampered = original.clone();
    let offset = tampered
        .iter()
        .position(|byte| *byte == b'0')
        .expect("version digit");
    tampered[offset] = b'1';
    fs::write(&state, tampered).expect("tampered ledger");
    assert!(
        load_ledger(&state)
            .expect_err("checksum mismatch")
            .to_string()
            .contains("checksum")
    );

    fs::write(&state, vec![b' '; STATE_MAX_BYTES + 1]).expect("oversize ledger");
    assert!(
        load_ledger(&state)
            .expect_err("oversize ledger")
            .to_string()
            .contains("exceeds")
    );
}

#[test]
fn ledger_rejects_duplicate_json_fields() {
    let root = tempdir().expect("temporary root");
    let state = root.path().join("state.json");
    write_ledger(&state, &Ledger::default());
    let original = fs::read_to_string(&state).expect("ledger text");
    let duplicate = original.replacen(
        "\"next_sequence\": 0,",
        "\"next_sequence\": 0,\n    \"next_sequence\": 0,",
        1,
    );
    fs::write(&state, duplicate).expect("duplicate ledger field");

    let error = load_ledger(&state).expect_err("duplicate recovery field");

    assert!(error.to_string().contains("duplicate"), "{error}");
}

#[test]
fn ledger_rejects_exhausted_sequence_space() {
    let ledger = Ledger {
        next_sequence: u64::MAX,
        ..Ledger::default()
    };

    let error = ledger_bytes(&ledger).expect_err("exhausted sequence space");

    assert!(error.to_string().contains("sequence"), "{error}");
}

#[test]
fn ledger_rejects_noncanonical_scope() {
    let root = tempdir().expect("temporary root");
    let entry = recovery_entry(root.path(), "cursor", Scope::Project, 0);
    let ledger = Ledger {
        next_sequence: 1,
        entries: [(state_key("cursor", Scope::Project), entry)]
            .into_iter()
            .collect(),
    };

    let scope_error = ledger_bytes(&ledger).expect_err("non-Claude project scope");
    assert!(scope_error.to_string().contains("scope"), "{scope_error}");
}

#[test]
fn ledger_rejects_noncanonical_backup_path() {
    let root = tempdir().expect("temporary root");
    let mut entry = recovery_entry(root.path(), "cursor", Scope::User, 0);
    entry.backup_path = root.path().join("config.json.bak.mcp-swap-legacy");
    let ledger = Ledger {
        next_sequence: 1,
        entries: [(state_key("cursor", Scope::User), entry)]
            .into_iter()
            .collect(),
    };

    let path_error = ledger_bytes(&ledger).expect_err("non-Rust backup name");
    assert!(
        path_error.to_string().contains("backup path"),
        "{path_error}"
    );
}

#[test]
fn ledger_refuses_wrong_mode_and_symlink() {
    let root = tempdir().expect("temporary root");
    let state = root.path().join("state.json");
    write_ledger(&state, &Ledger::default());
    fs::set_permissions(&state, fs::Permissions::from_mode(0o644)).expect("loosen mode");
    assert!(
        load_ledger(&state)
            .expect_err("public state")
            .to_string()
            .contains("0600")
    );

    let victim = root.path().join("victim");
    fs::write(&victim, b"unchanged").expect("victim");
    fs::remove_file(&state).expect("remove state");
    symlink(&victim, &state).expect("state symlink");
    assert!(
        load_ledger(&state)
            .expect_err("symlink state")
            .to_string()
            .contains("symlink")
    );
    assert_eq!(fs::read(&victim).expect("victim survives"), b"unchanged");
}

#[test]
fn lock_requires_private_directory_and_file_modes() {
    let root = tempdir().expect("temporary root");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private namespace");
    let state_dir = root.path().join("state");
    fs::create_dir(&state_dir).expect("state directory");
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o755)).expect("public directory");
    assert!(
        TransactionLock::acquire(&state_dir)
            .expect_err("public state directory")
            .to_string()
            .contains("0700")
    );

    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).expect("private directory");
    let lock_path = state_dir.join("state.lock");
    fs::write(&lock_path, b"").expect("lock file");
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).expect("public lock");
    assert!(
        TransactionLock::acquire(&state_dir)
            .expect_err("public lock")
            .to_string()
            .contains("0600")
    );
}

#[test]
fn lock_refuses_symlinks_and_detects_path_replacement() {
    let root = tempdir().expect("temporary root");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private namespace");
    let state_dir = root.path().join("state");
    fs::create_dir(&state_dir).expect("state directory");
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).expect("private directory");
    let victim = root.path().join("victim");
    fs::write(&victim, b"victim").expect("victim");
    let lock_path = state_dir.join("state.lock");
    symlink(&victim, &lock_path).expect("lock symlink");
    assert!(
        TransactionLock::acquire(&state_dir)
            .expect_err("lock symlink")
            .to_string()
            .contains("symlink")
    );

    fs::remove_file(&lock_path).expect("remove link");
    let lock = TransactionLock::acquire(&state_dir).expect("private lock");
    let held_inode = fs::metadata(&lock_path).expect("held lock").ino();
    let replacement = state_dir.join("replacement");
    fs::write(&replacement, b"").expect("replacement");
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))
        .expect("private replacement");
    fs::rename(&replacement, &lock_path).expect("replace lock path");

    assert_ne!(
        fs::metadata(&lock_path)
            .expect("replacement metadata")
            .ino(),
        held_inode
    );
    let error = lock.verify().expect_err("lock path changed");
    assert!(error.to_string().contains("changed"), "{error}");
}

#[test]
fn lock_detects_its_directory_replacement_even_when_the_file_inode_survives() {
    let root = tempdir().expect("temporary root");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private namespace");
    let state_dir = root.path().join("state");
    fs::create_dir(&state_dir).expect("state directory");
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).expect("private directory");
    let lock = TransactionLock::acquire(&state_dir).expect("private lock");
    let displaced = root.path().join("displaced");

    fs::rename(&state_dir, &displaced).expect("displace lock directory");
    fs::create_dir(&state_dir).expect("replacement lock directory");
    fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).expect("replacement mode");
    fs::rename(displaced.join("state.lock"), state_dir.join("state.lock"))
        .expect("preserve lock inode");

    let error = lock.verify().expect_err("lock directory changed");
    assert!(error.to_string().contains("directory changed"), "{error}");
}

#[test]
fn lock_interoperates_with_posix_record_lock_clients() {
    let root = tempdir().expect("temporary root");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private namespace");
    let state_dir = root.path().join("state");
    let lock = TransactionLock::acquire(&state_dir).expect("native lock");
    assert_record_lock_held(lock.path());
}

#[test]
fn rejected_lock_alias_does_not_release_the_record_lock() {
    let root = tempdir().expect("temporary root");
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).expect("private namespace");
    let state_dir = root.path().join("state");
    let lock = TransactionLock::acquire(&state_dir).expect("native lock");
    let alias = root.path().join("config.json");
    fs::hard_link(lock.path(), &alias).expect("lock alias");

    let error = stable_snapshot(&alias, 1024).expect_err("lock alias must be rejected");
    assert!(error.to_string().contains("active state lock"), "{error}");
    assert_record_lock_held(lock.path());
}

fn assert_record_lock_held(path: &std::path::Path) {
    let probe = Command::new("python3")
        .args([
            "-c",
            "import fcntl, os, sys\nf = os.fdopen(os.open(sys.argv[1], os.O_RDWR), 'r+')\ntry:\n fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\nexcept BlockingIOError:\n sys.exit(0)\nsys.exit(1)",
        ])
        .arg(path)
        .status()
        .expect("POSIX record-lock probe");

    assert!(
        probe.success(),
        "external fcntl client acquired the shared lock"
    );
}

fn write_ledger(path: &std::path::Path, ledger: &Ledger) {
    fs::write(path, ledger_bytes(ledger).expect("ledger bytes")).expect("write ledger");
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("ledger mode");
}

fn recovery_entry(
    root: &std::path::Path,
    client: &str,
    scope: Scope,
    sequence: u64,
) -> RecoveryEntry {
    let config = root.join("config.json");
    fs::write(&config, b"{}\n").expect("config");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).expect("config mode");
    let route = resolve_config_route(&config, 1024).expect("config route");
    let backup_path = root.join(format!("config.json.bak.mcp-swap-rust-{sequence:020}"));
    fs::write(&backup_path, b"{}\n").expect("backup");
    fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600)).expect("backup mode");
    let backup = stable_snapshot(&backup_path, 1024).expect("backup snapshot");
    RecoveryEntry {
        client: client.into(),
        scope,
        sequence,
        server: "tmux".into(),
        config_path: route.logical,
        target_path: route.target,
        route_links: route.links,
        route_anchors: route.anchors,
        route_parent: route.parent,
        original_mode: route.file.identity.mode,
        backup_path,
        backup: backup.identity,
        expected_config: route.file.identity,
    }
}
