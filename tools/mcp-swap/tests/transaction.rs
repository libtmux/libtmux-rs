//! Multi-client transaction and exact rollback contract tests.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use mcp_swap::catalog::{Client, Paths, known_clients, select_clients};
use mcp_swap::config::{Scope, ServerSpec, read_server};
use mcp_swap::fs::{FsError, stable_snapshot};
use mcp_swap::recovery::{STATE_MAX_BYTES, ledger_bytes, load_ledger};
use mcp_swap::transaction::{
    RETIRED_SAFETY, RevertRequest, UseRequest, planned_use_specs, revert_clients,
    revert_clients_with_hook, use_clients, use_clients_preflighted, use_clients_with_hook,
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    paths: Paths,
    clients: Vec<Client>,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let home = root.path().join("home");
        let config = root.path().join("config");
        let state = root.path().join("state");
        let repo = root.path().join("repo");
        for path in [&home, &config, &state, &repo] {
            fs::create_dir_all(path).expect("fixture directory");
        }
        let paths = Paths::from_roots(&home, &config, &state).expect("absolute roots");
        let clients = known_clients(&paths);
        for client in &clients {
            let parent = client.config_path.parent().expect("config parent");
            fs::create_dir_all(parent).expect("config directory");
            let seed = if matches!(client.name.as_str(), "codex" | "grok") {
                b"# retained header\n".as_slice()
            } else if matches!(client.name.as_str(), "opencode" | "pi") {
                b"{\n  // retained comment\n}\n".as_slice()
            } else {
                b"{\n  \"retained\": \"value\"\n}\n".as_slice()
            };
            fs::write(&client.config_path, seed).expect("seed config");
            fs::set_permissions(&client.config_path, fs::Permissions::from_mode(0o640))
                .expect("config mode");
        }
        Self {
            root,
            paths,
            clients,
            repo,
        }
    }

    fn request(&self, command: &str) -> UseRequest {
        UseRequest {
            repo: self.repo.clone(),
            server: "tmux".into(),
            scope: Scope::Project,
            spec: ServerSpec {
                command: command.into(),
                args: vec!["--stdio".into()],
                env: BTreeMap::from([("LIBTMUX_TOOLSETS".into(), "standard".into())]),
            },
        }
    }

    fn selected(&self, names: &[&str]) -> Vec<&Client> {
        select_clients(&self.clients, names).expect("known clients")
    }

    fn backup_files(&self) -> Vec<PathBuf> {
        let mut backups = Vec::new();
        for client in &self.clients {
            let parent = client.config_path.parent().expect("config parent");
            let prefix = format!(
                "{}.bak.mcp-swap-rust-",
                client
                    .config_path
                    .file_name()
                    .expect("config name")
                    .to_string_lossy()
            );
            for entry in fs::read_dir(parent).expect("config directory") {
                let path = entry.expect("directory entry").path();
                if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                {
                    backups.push(path);
                }
            }
        }
        backups.sort();
        backups.dedup();
        backups
    }

    fn stage_files(&self) -> Vec<PathBuf> {
        let mut directories: Vec<_> = self
            .clients
            .iter()
            .map(|client| {
                client
                    .config_path
                    .parent()
                    .expect("config parent")
                    .to_path_buf()
            })
            .collect();
        directories.push(self.paths.state_dir());
        directories.sort();
        directories.dedup();
        let mut stages = Vec::new();
        for directory in directories {
            for entry in fs::read_dir(directory).expect("artifact directory") {
                let path = entry.expect("directory entry").path();
                if path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".mcp-swap-stage-"))
                {
                    stages.push(path);
                }
            }
        }
        stages.sort();
        stages
    }
}

#[test]
fn all_clients_use_and_revert_as_one_exact_transaction() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&[]);
    let original: Vec<_> = selected
        .iter()
        .map(|client| {
            (
                client.config_path.clone(),
                fs::read(&client.config_path).expect("original bytes"),
                fs::metadata(&client.config_path)
                    .expect("original metadata")
                    .permissions()
                    .mode(),
            )
        })
        .collect();

    let changed = use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("/repo/tmux-mcp"),
        false,
    )
    .expect("all-client use");
    assert_eq!(changed.len(), 8);
    for client in &selected {
        assert!(
            read_server(
                &client.config,
                &fs::read(&client.config_path).expect("changed config"),
                "tmux",
                &fixture.repo,
                Scope::Project,
            )
            .expect("read changed entry")
            .is_some(),
            "{}",
            client.name.as_str()
        );
    }

    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
    )
    .expect("all-client revert");
    for (path, bytes, mode) in original {
        assert_eq!(
            fs::read(&path).expect("restored bytes"),
            bytes,
            "{}",
            path.display()
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("restored metadata")
                .permissions()
                .mode(),
            mode,
            "{}",
            path.display()
        );
    }
    assert!(!fixture.paths.state_file().exists());
    assert!(fixture.backup_files().is_empty());
}

#[test]
fn planned_use_rejects_explicit_retired_safety_aliases() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);

    for name in ["LIBTMUX_SAFETY", "TMUX_MCP_SAFETY"] {
        let mut request = fixture.request("new");
        request.spec.env = BTreeMap::from([(name.into(), "readonly".into())]);

        let error = planned_use_specs(&fixture.paths, &selected, &request)
            .expect_err("retired safety request");

        assert!(error.to_string().contains(name), "{error}");
        assert!(error.to_string().contains("LIBTMUX_TOOLSETS"), "{error}");
    }
}

#[test]
fn use_preserves_safety_aliases_until_explicit_toolset_replacement() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    fs::write(
        &selected[0].config_path,
        br#"{"mcpServers":{"tmux":{"command":"old","args":[],"env":{"KEEP":"yes","LIBTMUX_SAFETY":"readonly","TMUX_MCP_SAFETY":"read-only"}}}}"#,
    )
    .expect("legacy config");
    let mut request = fixture.request("new");

    let specs =
        planned_use_specs(&fixture.paths, &selected, &request).expect("explicit replacement plan");
    assert_eq!(specs.len(), 1);
    assert!(!specs[0].env.contains_key("LIBTMUX_SAFETY"));
    assert!(!specs[0].env.contains_key("TMUX_MCP_SAFETY"));
    use_clients(&fixture.paths, &selected, &request, false).expect("explicit replacement");
    let changed = fs::read(&selected[0].config_path).expect("changed config");
    let spec = read_server(
        &selected[0].config,
        &changed,
        "tmux",
        &fixture.repo,
        Scope::User,
    )
    .expect("read changed entry")
    .expect("changed entry");
    assert_eq!(spec.env.get("KEEP").map(String::as_str), Some("yes"));
    assert_eq!(
        spec.env.get("LIBTMUX_TOOLSETS").map(String::as_str),
        Some("standard")
    );
    assert!(!spec.env.contains_key("LIBTMUX_SAFETY"));
    assert!(!spec.env.contains_key("TMUX_MCP_SAFETY"));

    request.spec.env = BTreeMap::from([("NEW".into(), "value".into())]);
    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
    )
    .expect("restore legacy config");
    let original = fs::read(&selected[0].config_path).expect("legacy config restored");
    let original = String::from_utf8(original).expect("UTF-8 legacy config");
    assert!(original.contains("LIBTMUX_SAFETY"));
    assert!(original.contains("TMUX_MCP_SAFETY"));

    let specs =
        planned_use_specs(&fixture.paths, &selected, &request).expect("inherited safety plan");
    assert_eq!(specs.len(), 1);
    assert_eq!(
        specs[0].env.get("LIBTMUX_SAFETY").map(String::as_str),
        Some("readonly")
    );
    assert_eq!(
        specs[0].env.get("TMUX_MCP_SAFETY").map(String::as_str),
        Some("read-only")
    );
    use_clients(&fixture.paths, &selected, &request, false)
        .expect("safety aliases are preserved without an explicit replacement");
    let changed = fs::read(&selected[0].config_path).expect("changed config");
    let spec = read_server(
        &selected[0].config,
        &changed,
        "tmux",
        &fixture.repo,
        Scope::User,
    )
    .expect("read changed entry")
    .expect("changed entry");
    assert_eq!(
        spec.env.get("LIBTMUX_SAFETY").map(String::as_str),
        Some("readonly")
    );
    assert_eq!(
        spec.env.get("TMUX_MCP_SAFETY").map(String::as_str),
        Some("read-only")
    );
    assert_eq!(spec.env.get("NEW").map(String::as_str), Some("value"));
}

#[test]
fn preflight_plans_include_each_clients_merged_environment() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor", "gemini"]);
    for (client, value) in selected.iter().zip(["cursor-value", "gemini-value"]) {
        fs::write(
            &client.config_path,
            format!(
                r#"{{"mcpServers":{{"tmux":{{"command":"old","args":[],"env":{{"CLIENT":"{value}"}}}}}}}}"#
            ),
        )
        .expect("existing client entry");
    }

    let specs = planned_use_specs(&fixture.paths, &selected, &fixture.request("tmux-mcp"))
        .expect("per-client preflight plans");

    assert_eq!(specs.len(), 2);
    assert_eq!(
        specs[0].env.get("CLIENT").map(String::as_str),
        Some("cursor-value")
    );
    assert_eq!(
        specs[1].env.get("CLIENT").map(String::as_str),
        Some("gemini-value")
    );
    assert!(
        specs
            .iter()
            .all(|spec| spec.env.get("LIBTMUX_TOOLSETS").map(String::as_str) == Some("standard"))
    );
}

#[test]
fn locked_replan_must_match_the_preflighted_definitions() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let request = fixture.request("tmux-mcp");
    let planned = planned_use_specs(&fixture.paths, &selected, &request).expect("initial plan");
    let late = br#"{"mcpServers":{"tmux":{"command":"human","args":[],"env":{"LATE":"yes"}}}}"#;
    fs::write(&selected[0].config_path, late).expect("late human edit");

    let error = use_clients_preflighted(&fixture.paths, &selected, &request, &planned)
        .expect_err("late environment change invalidates preflight");

    assert!(error.to_string().contains("changed after preflight"));
    assert_eq!(
        fs::read(&selected[0].config_path).expect("human edit survives"),
        late
    );
    assert!(!fixture.paths.state_file().exists());
}

#[test]
fn malformed_later_config_aborts_before_any_artifact_write() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor", "gemini"]);
    let cursor = selected[0].config_path.clone();
    let before = fs::read(&cursor).expect("cursor bytes");
    fs::write(&selected[1].config_path, b"{ not JSON").expect("malformed gemini");

    let error = use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
    )
    .expect_err("later malformed config");

    assert!(error.to_string().contains("gemini"));
    assert_eq!(fs::read(cursor).expect("cursor survives"), before);
    assert!(!fixture.paths.state_file().exists());
    assert!(fixture.backup_files().is_empty());
}

#[test]
fn staging_failure_removes_every_owned_stage() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor", "gemini"]);
    let original: Vec<_> = selected
        .iter()
        .map(|client| {
            (
                client.config_path.clone(),
                fs::read(&client.config_path).expect("original config"),
            )
        })
        .collect();
    let mut request = fixture.request("tmux-mcp");
    request.server = "x".repeat(STATE_MAX_BYTES);

    let error = use_clients(&fixture.paths, &selected, &request, false)
        .expect_err("oversized recovery state");

    assert!(error.to_string().contains("recovery state exceeds"));
    for (path, bytes) in original {
        assert_eq!(fs::read(path).expect("unchanged config"), bytes);
    }
    assert!(!fixture.paths.state_file().exists());
    assert!(fixture.backup_files().is_empty());
    assert!(fixture.stage_files().is_empty(), "owned stages leaked");
}

#[test]
fn unselected_config_alias_aborts_before_any_artifact_write() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let cursor = selected[0].config_path.clone();
    let before = fs::read(&cursor).expect("cursor bytes");
    let opencode = fixture
        .clients
        .iter()
        .find(|client| client.name.as_str() == "opencode")
        .expect("opencode")
        .config_path
        .clone();
    fs::remove_file(&opencode).expect("remove opencode config");
    fs::hard_link(&cursor, &opencode).expect("alias unselected config");

    let error = use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
    )
    .expect_err("unselected alias");

    assert!(error.to_string().contains("artifact alias"), "{error}");
    assert_eq!(fs::read(cursor).expect("cursor survives"), before);
    assert!(!fixture.paths.state_file().exists());
    assert!(fixture.backup_files().is_empty());
}

#[test]
fn malformed_utf8_in_every_config_format_is_refused_without_writes() {
    for name in ["cursor", "opencode", "codex"] {
        let fixture = Fixture::new();
        let selected = fixture.selected(&[]);
        let malformed = fixture
            .clients
            .iter()
            .find(|client| client.name.as_str() == name)
            .expect("representative client");
        fs::write(&malformed.config_path, b"configuration \xff").expect("malformed UTF-8");
        let before: Vec<_> = selected
            .iter()
            .map(|client| {
                (
                    client.config_path.clone(),
                    fs::read(&client.config_path).expect("config bytes"),
                )
            })
            .collect();

        let error = use_clients(
            &fixture.paths,
            &selected,
            &fixture.request("tmux-mcp"),
            false,
        )
        .expect_err("malformed UTF-8 must fail closed");

        assert!(error.to_string().contains(name), "{name}: {error}");
        assert!(error.to_string().contains("not UTF-8"), "{name}: {error}");
        for (path, bytes) in before {
            assert_eq!(fs::read(&path).expect("unchanged config"), bytes, "{name}");
        }
        assert!(!fixture.paths.state_file().exists(), "{name}");
        assert!(fixture.backup_files().is_empty(), "{name}");
    }
}

#[test]
fn duplicate_physical_config_target_is_rejected() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor", "gemini"]);
    fs::remove_file(&selected[1].config_path).expect("remove gemini config");
    fs::hard_link(&selected[0].config_path, &selected[1].config_path).expect("alias configs");
    let before = fs::read(&selected[0].config_path).expect("aliased bytes");

    let error = use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
    )
    .expect_err("physical alias");

    assert!(error.to_string().contains("alias"));
    assert_eq!(
        fs::read(&selected[0].config_path).expect("first survives"),
        before
    );
    assert!(!fixture.paths.state_file().exists());
}

#[test]
fn repeat_use_keeps_first_backup_and_revert_restores_pristine_bytes() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let pristine = fs::read(&selected[0].config_path).expect("pristine config");
    use_clients(&fixture.paths, &selected, &fixture.request("first"), false).expect("first use");
    let ledger = load_ledger(&fixture.paths.state_file()).expect("first ledger");
    let backup = ledger
        .entries
        .values()
        .next()
        .expect("entry")
        .backup_path
        .clone();
    let backup_bytes = fs::read(&backup).expect("first backup");

    use_clients(&fixture.paths, &selected, &fixture.request("second"), false).expect("repeat use");
    let repeated = load_ledger(&fixture.paths.state_file()).expect("repeat ledger");
    assert_eq!(
        repeated.entries.values().next().expect("entry").backup_path,
        backup
    );
    assert_eq!(fs::read(&backup).expect("retained backup"), backup_bytes);

    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
    )
    .expect("revert");
    assert_eq!(
        fs::read(&selected[0].config_path).expect("restored config"),
        pristine
    );
}

#[test]
fn recovery_records_the_selected_server_name() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let mut request = fixture.request("tmux-mcp");
    request.server = "named-server".into();

    use_clients(&fixture.paths, &selected, &request, false).expect("named use");
    let ledger = load_ledger(&fixture.paths.state_file()).expect("recovery state");

    assert_eq!(
        ledger.entries.values().next().expect("entry").server,
        "named-server"
    );
}

#[test]
fn failure_after_first_publish_rolls_back_prior_config_state_and_backup() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor", "gemini"]);
    let before: Vec<_> = selected
        .iter()
        .map(|client| fs::read(&client.config_path).expect("before"))
        .collect();
    let mut hook = |boundary: &str| {
        if boundary == "before-config-gemini-take" {
            return Err(FsError::new("injected second-config failure"));
        }
        Ok(())
    };

    let error = use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("injected failure");

    assert!(error.to_string().contains("injected"));
    for (client, bytes) in selected.iter().zip(before) {
        assert_eq!(fs::read(&client.config_path).expect("rolled back"), bytes);
    }
    assert!(!fixture.paths.state_file().exists());
    assert!(fixture.backup_files().is_empty());
}

#[test]
fn final_verification_failure_rolls_back_every_published_artifact() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let before = fs::read(&selected[0].config_path).expect("original config");
    let mut hook = |boundary: &str| {
        if boundary == "before-config-cursor-finish" {
            return Err(FsError::new("injected verification failure"));
        }
        Ok(())
    };

    let error = use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("final verification failure");

    assert!(error.to_string().contains("injected verification failure"));
    assert_eq!(
        fs::read(&selected[0].config_path).expect("restored config"),
        before
    );
    assert!(!fixture.paths.state_file().exists());
    assert!(fixture.backup_files().is_empty());
}

#[test]
fn late_config_replacement_survives_and_blocks_publication() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let config = selected[0].config_path.clone();
    let late = b"{\"human\":true}\n";
    let mut injected = false;
    let mut hook = |boundary: &str| {
        if boundary == "before-config-cursor-take" && !injected {
            injected = true;
            let replacement = config.with_extension("late");
            fs::write(&replacement, late).map_err(|error| FsError::new(error.to_string()))?;
            fs::rename(&replacement, &config).map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("late config");

    assert_eq!(fs::read(config).expect("late config survives"), late);
}

#[test]
fn late_state_file_survives_create_new_publication() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let state = fixture.paths.state_file();
    let late = b"human state\n";
    let mut hook = |boundary: &str| {
        if boundary == "before-state-publish" && !state.exists() {
            fs::write(&state, late).map_err(|error| FsError::new(error.to_string()))?;
            fs::set_permissions(&state, fs::Permissions::from_mode(0o600))
                .map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("late state");

    assert_eq!(fs::read(state).expect("late state survives"), late);
}

#[test]
fn recovery_parse_and_transaction_snapshot_are_one_read() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    use_clients(&fixture.paths, &selected, &fixture.request("first"), false).expect("initial use");
    let state = fixture.paths.state_file();
    let config = selected[0].config_path.clone();
    let before = fs::read(&config).expect("swapped config");
    let mut replacement = load_ledger(&state).expect("recovery state");
    replacement.next_sequence += 1;
    let replacement = ledger_bytes(&replacement).expect("replacement state");
    let mut injected = false;
    let mut hook = |boundary: &str| {
        if boundary == "after-recovery-state-snapshot" && !injected {
            injected = true;
            fs::write(&state, &replacement).map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    let error = use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("second"),
        false,
        &mut hook,
    )
    .expect_err("replacement state must block publication");

    assert!(
        error
            .to_string()
            .contains("state changed before take-aside")
    );
    assert_eq!(fs::read(state).expect("replacement survives"), replacement);
    assert_eq!(fs::read(config).expect("config unchanged"), before);
}

#[test]
fn late_backup_survives_create_new_publication() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let config = selected[0].config_path.clone();
    let before = fs::read(&config).expect("original config");
    let backup = config.with_file_name(format!(
        "{}.bak.mcp-swap-rust-{:020}",
        config.file_name().expect("config name").to_string_lossy(),
        0
    ));
    let late = b"human backup\n";
    let mut hook = |boundary: &str| {
        if boundary == "before-backup-cursor-publish" && !backup.exists() {
            fs::write(&backup, late).map_err(|error| FsError::new(error.to_string()))?;
            fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))
                .map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("late backup");

    assert_eq!(fs::read(&backup).expect("late backup survives"), late);
    assert_eq!(fs::read(config).expect("config unchanged"), before);
    assert!(!fixture.paths.state_file().exists());
}

#[test]
fn replaced_lock_survives_and_aborts_before_config_publication() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let config = selected[0].config_path.clone();
    let before = fs::read(&config).expect("original config");
    let lock = fixture.paths.lock_file();
    let replacement = fixture.paths.lock_dir().join("replacement.lock");
    let late = b"late lock\n";
    let mut hook = |boundary: &str| {
        if boundary == "before-state-publish" {
            fs::write(&replacement, late).map_err(|error| FsError::new(error.to_string()))?;
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))
                .map_err(|error| FsError::new(error.to_string()))?;
            fs::rename(&replacement, &lock).map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("replaced lock");

    assert_eq!(fs::read(&lock).expect("late lock survives"), late);
    assert_eq!(fs::read(config).expect("config unchanged"), before);
    assert!(!fixture.paths.state_file().exists());
    assert!(fixture.backup_files().is_empty());
}

#[test]
fn late_stage_replacement_is_retained_with_the_original_recovery() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let config = selected[0].config_path.clone();
    let original = fs::read(&config).expect("original config");
    let late = b"{\"late\":true}\n";
    let mut hook = |boundary: &str| {
        if boundary == "before-config-cursor-publish" {
            let prefix = format!(
                ".{}.mcp-swap-stage-",
                config.file_name().expect("config name").to_string_lossy()
            );
            let stage = fs::read_dir(config.parent().expect("config parent"))
                .map_err(|error| FsError::new(error.to_string()))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
                })
                .ok_or_else(|| FsError::new("config stage not found"))?;
            fs::remove_file(&stage).map_err(|error| FsError::new(error.to_string()))?;
            fs::write(&stage, late).map_err(|error| FsError::new(error.to_string()))?;
            fs::set_permissions(&stage, fs::Permissions::from_mode(0o640))
                .map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    let error = use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("late stage replacement");

    assert!(error.to_string().contains("rollback incomplete"), "{error}");
    assert_eq!(fs::read(&config).expect("late publication survives"), late);
    let recovered_original = fs::read_dir(config.parent().expect("config parent"))
        .expect("config directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .contains("mcp-swap-config-cursor-recovery")
            })
        })
        .any(|path| fs::read(path).is_ok_and(|bytes| bytes == original));
    assert!(recovered_original, "original recovery artifact retained");
}

#[test]
fn replacement_after_publication_retains_the_original_recovery() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let config = selected[0].config_path.clone();
    let original = fs::read(&config).expect("original config");
    let late = b"{\"human\":true}\n";
    let mut injected = false;
    let mut hook = |boundary: &str| {
        if boundary == "before-config-cursor-finish" && !injected {
            injected = true;
            let replacement = config.with_extension("late");
            fs::write(&replacement, late).map_err(|error| FsError::new(error.to_string()))?;
            fs::rename(&replacement, &config).map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("late post-publication replacement");

    assert_eq!(fs::read(&config).expect("late config survives"), late);
    let recovered_original = fs::read_dir(config.parent().expect("config parent"))
        .expect("config directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .contains("mcp-swap-config-cursor-recovery")
            })
        })
        .any(|path| fs::read(path).is_ok_and(|bytes| bytes == original));
    assert!(recovered_original, "original recovery artifact retained");
}

#[test]
fn late_backup_replacement_blocks_revert_and_rolls_back_prior_steps() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
    )
    .expect("initial use");
    let swapped = fs::read(&selected[0].config_path).expect("swapped config");
    let state = fs::read(fixture.paths.state_file()).expect("recovery state");
    let ledger = load_ledger(&fixture.paths.state_file()).expect("ledger");
    let backup = ledger.entries["cursor:user"].backup_path.clone();
    let late = b"late backup\n";
    let mut hook = |boundary: &str| {
        if boundary == "before-backup-cursor-take" {
            let replacement = backup.with_extension("late");
            fs::write(&replacement, late).map_err(|error| FsError::new(error.to_string()))?;
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))
                .map_err(|error| FsError::new(error.to_string()))?;
            fs::rename(&replacement, &backup).map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    revert_clients_with_hook(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
        &mut hook,
    )
    .expect_err("late backup blocks revert");

    assert_eq!(fs::read(&backup).expect("late backup survives"), late);
    assert_eq!(
        fs::read(&selected[0].config_path).expect("swapped config restored"),
        swapped
    );
    assert_eq!(
        fs::read(fixture.paths.state_file()).expect("state restored"),
        state
    );
}

#[test]
fn later_revert_failure_rolls_back_every_prior_artifact_exactly() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor", "gemini"]);
    use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
    )
    .expect("initial use");
    let configs: Vec<_> = selected
        .iter()
        .map(|client| stable_snapshot(&client.config_path, 16 * 1024 * 1024).expect("config"))
        .collect();
    let state = stable_snapshot(&fixture.paths.state_file(), 256 * 1024).expect("state");
    let backups: Vec<_> = fixture
        .backup_files()
        .into_iter()
        .map(|path| stable_snapshot(&path, 16 * 1024 * 1024).expect("backup"))
        .collect();
    let mut hook = |boundary: &str| {
        if boundary == "before-config-cursor-take" {
            return Err(FsError::new("injected later revert failure"));
        }
        Ok(())
    };

    revert_clients_with_hook(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
        &mut hook,
    )
    .expect_err("later revert failure");

    for snapshot in configs {
        assert_eq!(
            stable_snapshot(&snapshot.path, 16 * 1024 * 1024)
                .expect("restored config")
                .identity,
            snapshot.identity
        );
    }
    assert_eq!(
        stable_snapshot(&state.path, 256 * 1024)
            .expect("restored state")
            .identity,
        state.identity
    );
    for snapshot in backups {
        assert_eq!(
            stable_snapshot(&snapshot.path, 16 * 1024 * 1024)
                .expect("restored backup")
                .identity,
            snapshot.identity
        );
    }
}

#[test]
fn corrupt_recovery_state_blocks_use_and_revert_without_touching_artifacts() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
    )
    .expect("initial use");
    let config = stable_snapshot(&selected[0].config_path, 16 * 1024 * 1024).expect("config");
    let ledger = load_ledger(&fixture.paths.state_file()).expect("ledger");
    let backup_path = ledger.entries["cursor:user"].backup_path.clone();
    let backup = stable_snapshot(&backup_path, 16 * 1024 * 1024).expect("backup");
    let mut state = fs::read(fixture.paths.state_file()).expect("state bytes");
    let checksum = state
        .windows(b"\"checksum\": \"".len())
        .position(|window| window == b"\"checksum\": \"")
        .expect("checksum field")
        + b"\"checksum\": \"".len();
    state[checksum] = if state[checksum] == b'0' { b'1' } else { b'0' };
    fs::write(fixture.paths.state_file(), &state).expect("corrupt state");

    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
    )
    .expect_err("corrupt state blocks revert");
    use_clients(&fixture.paths, &selected, &fixture.request("second"), false)
        .expect_err("corrupt state blocks use");

    assert_eq!(
        stable_snapshot(&config.path, 16 * 1024 * 1024)
            .expect("config survives")
            .identity,
        config.identity
    );
    assert_eq!(
        stable_snapshot(&backup.path, 16 * 1024 * 1024)
            .expect("backup survives")
            .identity,
        backup.identity
    );
    assert_eq!(
        fs::read(fixture.paths.state_file()).expect("corrupt state survives"),
        state
    );
}

#[test]
fn claude_user_and_project_layers_restore_in_lifo_order() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["claude"]);
    let pristine = fs::read(&selected[0].config_path).expect("pristine Claude config");
    let mut project = fixture.request("project");
    project.scope = Scope::Project;
    use_clients(&fixture.paths, &selected, &project, false).expect("project use");
    let project_bytes = fs::read(&selected[0].config_path).expect("project bytes");
    let mut user = fixture.request("user");
    user.scope = Scope::User;
    use_clients(&fixture.paths, &selected, &user, false).expect("user use");

    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest {
            scope: Some(Scope::User),
        },
        false,
    )
    .expect("user revert");
    assert_eq!(
        fs::read(&selected[0].config_path).expect("project restored"),
        project_bytes
    );
    let remaining = load_ledger(&fixture.paths.state_file()).expect("project recovery state");
    let current = stable_snapshot(&selected[0].config_path, 16 * 1024 * 1024)
        .expect("restored project identity");
    assert_eq!(
        remaining
            .entries
            .get("claude:project")
            .expect("project recovery entry")
            .expected_config,
        current.identity
    );

    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest {
            scope: Some(Scope::Project),
        },
        false,
    )
    .expect("project revert");
    assert_eq!(
        fs::read(&selected[0].config_path).expect("pristine restored"),
        pristine
    );
}

#[test]
fn unscoped_revert_unwinds_all_claude_layers_in_lifo_order() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["claude"]);
    let pristine = fs::read(&selected[0].config_path).expect("pristine Claude config");
    let mut project = fixture.request("project");
    project.scope = Scope::Project;
    use_clients(&fixture.paths, &selected, &project, false).expect("project use");
    let mut user = fixture.request("user");
    user.scope = Scope::User;
    use_clients(&fixture.paths, &selected, &user, false).expect("user use");

    let changes = revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
    )
    .expect("unscoped LIFO revert");

    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].scope, Scope::User);
    assert_eq!(changes[1].scope, Scope::Project);
    assert_eq!(
        fs::read(&selected[0].config_path).expect("pristine restored"),
        pristine
    );
    assert!(!fixture.paths.state_file().exists());
}

#[test]
fn failed_unscoped_claude_revert_restores_every_layer_artifact() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["claude"]);
    let mut project = fixture.request("project");
    project.scope = Scope::Project;
    use_clients(&fixture.paths, &selected, &project, false).expect("project use");
    let mut user = fixture.request("user");
    user.scope = Scope::User;
    use_clients(&fixture.paths, &selected, &user, false).expect("user use");
    let config = stable_snapshot(&selected[0].config_path, 16 * 1024 * 1024).expect("config");
    let state = stable_snapshot(&fixture.paths.state_file(), 256 * 1024).expect("state");
    let backups: Vec<_> = fixture
        .backup_files()
        .into_iter()
        .map(|path| stable_snapshot(&path, 16 * 1024 * 1024).expect("backup"))
        .collect();
    let mut takes = 0;
    let mut hook = |boundary: &str| {
        if boundary == "before-config-claude-take" {
            takes += 1;
            if takes == 2 {
                return Err(FsError::new("injected older-layer failure"));
            }
        }
        Ok(())
    };

    revert_clients_with_hook(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
        &mut hook,
    )
    .expect_err("later layer failure");

    for snapshot in std::iter::once(config)
        .chain(std::iter::once(state))
        .chain(backups)
    {
        assert_eq!(
            stable_snapshot(&snapshot.path, 16 * 1024 * 1024)
                .expect("artifact restored")
                .identity,
            snapshot.identity
        );
    }
}

#[test]
fn repeat_older_claude_layer_rewrites_the_newer_recovery_chain() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["claude"]);
    let pristine = fs::read(&selected[0].config_path).expect("pristine Claude config");
    let mut user = fixture.request("user-one");
    user.scope = Scope::User;
    use_clients(&fixture.paths, &selected, &user, false).expect("user use");
    let mut project = fixture.request("project");
    project.scope = Scope::Project;
    use_clients(&fixture.paths, &selected, &project, false).expect("project use");

    user.spec.command = "user-two".into();
    use_clients(&fixture.paths, &selected, &user, false).expect("repeat older user layer");
    let state = load_ledger(&fixture.paths.state_file()).expect("recovery state");
    assert!(state.entries["claude:user"].sequence < state.entries["claude:project"].sequence);

    for scope in [Scope::Project, Scope::User] {
        revert_clients(
            &fixture.paths,
            &selected,
            RevertRequest { scope: Some(scope) },
            false,
        )
        .expect("LIFO revert");
    }
    assert_eq!(
        fs::read(&selected[0].config_path).expect("pristine restored"),
        pristine
    );
    assert!(!fixture.paths.state_file().exists());
}

#[test]
fn unscoped_revert_after_repeating_an_older_layer_uses_the_rewritten_chain() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["claude"]);
    let pristine = fs::read(&selected[0].config_path).expect("pristine Claude config");
    let mut user = fixture.request("user-one");
    user.scope = Scope::User;
    use_clients(&fixture.paths, &selected, &user, false).expect("user use");
    let mut project = fixture.request("project");
    project.scope = Scope::Project;
    use_clients(&fixture.paths, &selected, &project, false).expect("project use");

    user.spec.command = "user-two".into();
    use_clients(&fixture.paths, &selected, &user, false).expect("repeat older user layer");
    let state = load_ledger(&fixture.paths.state_file()).expect("recovery state");
    let user_entry = &state.entries["claude:user"];
    let project_entry = &state.entries["claude:project"];
    assert_eq!(user_entry.expected_config.mode, project_entry.original_mode);
    assert_eq!(user_entry.expected_config.size, project_entry.backup.size);
    assert_eq!(
        user_entry.expected_config.sha256,
        project_entry.backup.sha256
    );

    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
    )
    .expect("unscoped LIFO revert");

    assert_eq!(
        fs::read(&selected[0].config_path).expect("pristine restored"),
        pristine
    );
    assert!(!fixture.paths.state_file().exists());
}

#[test]
fn symlinked_config_keeps_topology_through_use_and_revert() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let logical = selected[0].config_path.clone();
    let target = fixture.root.path().join("dotfiles/cursor.json");
    fs::create_dir_all(target.parent().expect("target parent")).expect("dotfiles");
    let original = fs::read(&logical).expect("original config");
    fs::write(&target, &original).expect("target config");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).expect("target mode");
    fs::remove_file(&logical).expect("remove logical config");
    symlink(&target, &logical).expect("config symlink");

    use_clients(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
    )
    .expect("symlinked use");
    assert_eq!(fs::read_link(&logical).expect("link survives use"), target);
    revert_clients(
        &fixture.paths,
        &selected,
        RevertRequest { scope: None },
        false,
    )
    .expect("symlinked revert");
    assert_eq!(
        fs::read_link(&logical).expect("link survives revert"),
        target
    );
    assert_eq!(fs::read(&target).expect("target restored"), original);
}

#[test]
fn final_guard_rejects_a_symlink_route_change() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let logical = selected[0].config_path.clone();
    let original_target = fixture.root.path().join("dotfiles/original.json");
    let late_target = fixture.root.path().join("dotfiles/late.json");
    fs::create_dir_all(original_target.parent().expect("target parent")).expect("dotfiles");
    let original = fs::read(&logical).expect("original bytes");
    let late = b"{\"human\":true}\n";
    fs::write(&original_target, &original).expect("original target");
    fs::write(&late_target, late).expect("late target");
    fs::remove_file(&logical).expect("remove logical");
    symlink(&original_target, &logical).expect("original link");
    let mut hook = |boundary: &str| {
        if boundary == "before-config-cursor-take" {
            fs::remove_file(&logical).map_err(|error| FsError::new(error.to_string()))?;
            symlink(&late_target, &logical).map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("route replacement");

    assert_eq!(
        fs::read(&original_target).expect("original target survives"),
        original
    );
    assert_eq!(fs::read(&late_target).expect("late target survives"), late);
    assert_eq!(
        fs::read_link(&logical).expect("late link survives"),
        late_target
    );
}

#[test]
fn final_guard_rejects_a_symlink_logical_parent_replacement() {
    let fixture = Fixture::new();
    let selected = fixture.selected(&["cursor"]);
    let logical = selected[0].config_path.clone();
    let logical_parent = logical.parent().expect("logical parent").to_path_buf();
    let displaced_parent = fixture.root.path().join("displaced-cursor-parent");
    let target = fixture.root.path().join("dotfiles/cursor.json");
    fs::create_dir_all(target.parent().expect("target parent")).expect("dotfiles");
    let original = fs::read(&logical).expect("original bytes");
    fs::write(&target, &original).expect("target config");
    fs::remove_file(&logical).expect("remove logical config");
    symlink(&target, &logical).expect("config symlink");
    let mut hook = |boundary: &str| {
        if boundary == "before-config-cursor-take" {
            fs::rename(&logical_parent, &displaced_parent)
                .map_err(|error| FsError::new(error.to_string()))?;
            fs::create_dir(&logical_parent).map_err(|error| FsError::new(error.to_string()))?;
            fs::rename(displaced_parent.join("mcp.json"), &logical)
                .map_err(|error| FsError::new(error.to_string()))?;
        }
        Ok(())
    };

    use_clients_with_hook(
        &fixture.paths,
        &selected,
        &fixture.request("tmux-mcp"),
        false,
        &mut hook,
    )
    .expect_err("logical parent replacement");

    assert_eq!(fs::read(&target).expect("target survives"), original);
    assert_eq!(fs::read_link(&logical).expect("link survives"), target);
}

/// The swapper writes the configuration `tmux-mcp` starts from, so a name the
/// server rejects but the swapper accepts produces a server that cannot start.
/// Read the server's own constants rather than trusting a copied list.
#[test]
fn retired_safety_names_match_the_server_that_rejects_them() {
    let policy = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/tmux-mcp/src/policy.rs")
        .canonicalize()
        .expect("tmux-mcp policy source");
    let source = fs::read_to_string(&policy).expect("read policy source");

    let declared: Vec<String> = source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("pub const ")?;
            let (name, value) = rest.split_once(": &str = ")?;
            name.contains("SAFETY_ENV")
                .then(|| value.trim_end_matches(';').trim_matches('"').to_owned())
        })
        .collect();

    assert!(
        !declared.is_empty(),
        "no *SAFETY_ENV constant found in {}; the parser or the server moved",
        policy.display()
    );
    let mut expected = declared;
    expected.sort();
    let mut actual: Vec<String> = RETIRED_SAFETY
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    actual.sort();
    assert_eq!(
        actual, expected,
        "mcp-swap rejects {actual:?} but tmux-mcp rejects {expected:?}"
    );
}
