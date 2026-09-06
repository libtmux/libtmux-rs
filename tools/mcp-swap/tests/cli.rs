//! Native command-line contract tests under isolated configuration roots.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use mcp_swap::catalog::{Paths, known_clients};
use mcp_swap::fs::{resolve_config_route, stable_snapshot};
use mcp_swap::recovery::load_ledger;
use mcp_swap::source::{Source, SourceOptions, resolve_repo_meta, source_spec};
use tempfile::TempDir;

struct CliFixture {
    _root: TempDir,
    home: PathBuf,
    config: PathBuf,
    state: PathBuf,
    repo: PathBuf,
    binary: PathBuf,
}

impl CliFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary root");
        let home = root.path().join("home");
        let config = root.path().join("config");
        let state = root.path().join("state");
        let repo = root.path().join("repo");
        for path in [&home, &config, &state, &repo] {
            fs::create_dir_all(path).expect("fixture directory");
        }
        fs::create_dir_all(repo.join("crates/tmux-mcp/src")).expect("crate directory");
        fs::write(
            repo.join("crates/tmux-mcp/Cargo.toml"),
            "[package]\nname = \"tmux-mcp\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("manifest");
        fs::write(repo.join("crates/tmux-mcp/src/main.rs"), "fn main() {}\n").expect("main source");
        let binary = root.path().join("server");
        fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).expect("binary mode");
        Self {
            _root: root,
            home,
            config,
            state,
            repo,
            binary,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mcp-swap"));
        command
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_STATE_HOME", &self.state)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&self.repo);
        command
    }

    fn seed(&self, names: &[&str]) {
        let paths = Paths::from_roots(&self.home, &self.config, &self.state).expect("paths");
        for client in known_clients(&paths) {
            if !names.contains(&client.name.as_str()) {
                continue;
            }
            fs::create_dir_all(client.config_path.parent().expect("config parent"))
                .expect("config directory");
            let bytes = if matches!(client.name.as_str(), "codex" | "grok") {
                b"# retained\n".as_slice()
            } else {
                b"{}\n".as_slice()
            };
            fs::write(client.config_path, bytes).expect("config");
        }
    }
}

#[test]
fn help_lists_commands_sources_and_selector_alias() {
    let fixture = CliFixture::new();
    let output = fixture
        .command()
        .arg("use")
        .arg("--help")
        .output()
        .expect("help");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 help");
    for text in [
        "debug",
        "release",
        "run",
        "path",
        "published",
        "--cli",
        "antigravity",
    ] {
        assert!(stdout.contains(text), "missing {text:?} from help");
    }
}

#[test]
fn retired_safety_environment_is_rejected_by_the_command_line() {
    let fixture = CliFixture::new();
    let output = fixture
        .command()
        .args(["use", "--env", "LIBTMUX_SAFETY=readonly"])
        .output()
        .expect("invalid environment");

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("LIBTMUX_SAFETY is retired")
    );
}

#[test]
fn source_specs_cover_all_five_modes() {
    let fixture = CliFixture::new();
    let metadata = resolve_repo_meta(&fixture.repo, "tmux-mcp").expect("repo metadata");
    assert_eq!(metadata.server, "tmux");
    assert_eq!(metadata.binary, "tmux-mcp");
    let mut options = SourceOptions {
        repo: fixture.repo.clone(),
        crate_name: "tmux-mcp".into(),
        binary: "tmux-mcp".into(),
        version: None,
        binary_path: None,
        releases_root: fixture.state.join("libtmux-mcp-dev/releases/rust"),
    };
    assert!(
        source_spec(Source::Debug, &options)
            .expect("debug")
            .command
            .ends_with("target/debug/tmux-mcp")
    );
    assert!(
        source_spec(Source::Release, &options)
            .expect("release")
            .command
            .ends_with("target/release/tmux-mcp")
    );
    let run = source_spec(Source::Run, &options).expect("run");
    assert_eq!(run.command, "cargo");
    assert!(run.args.iter().any(|argument| argument == "--locked"));
    options.binary_path = Some(fixture.binary.clone());
    assert_eq!(
        source_spec(Source::Path, &options).expect("path").command,
        fixture.binary.to_string_lossy()
    );
    options.version = Some("0.1.0-alpha.10".into());
    assert!(
        source_spec(Source::Published, &options)
            .expect("published")
            .command
            .contains("0.1.0-alpha.10")
    );
}

#[test]
fn source_specs_reject_traversing_binary_and_version_components() {
    let fixture = CliFixture::new();
    let mut options = SourceOptions {
        repo: fixture.repo.clone(),
        crate_name: "tmux-mcp".into(),
        binary: "../escape".into(),
        version: None,
        binary_path: None,
        releases_root: fixture.state.join("libtmux-mcp-dev/releases/rust"),
    };
    assert!(
        source_spec(Source::Debug, &options)
            .expect_err("traversing binary")
            .to_string()
            .contains("path component")
    );

    options.binary = "tmux-mcp".into();
    options.version = Some("../../escape".into());
    assert!(
        source_spec(Source::Published, &options)
            .expect_err("traversing version")
            .to_string()
            .contains("exact version")
    );
}

#[test]
fn repository_metadata_rejects_traversing_components() {
    let fixture = CliFixture::new();
    assert!(
        resolve_repo_meta(&fixture.repo, "../tmux-mcp")
            .expect_err("traversing crate name")
            .to_string()
            .contains("path component")
    );
    let manifest = fixture.repo.join("crates/tmux-mcp/Cargo.toml");
    fs::write(
        &manifest,
        "[package]\nname = \"../escape\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("traversing package manifest");
    assert!(
        resolve_repo_meta(&fixture.repo, "tmux-mcp")
            .expect_err("traversing package name")
            .to_string()
            .contains("path component")
    );
    fs::write(
        &manifest,
        "[package]\nname = \"tmux-mcp\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[[bin]]\nname = \"../escape\"\npath = \"src/main.rs\"\n",
    )
    .expect("traversing binary manifest");
    assert!(
        resolve_repo_meta(&fixture.repo, "tmux-mcp")
            .expect_err("traversing binary name")
            .to_string()
            .contains("path component")
    );
}

#[test]
fn published_install_waits_for_all_config_planning() {
    let fixture = CliFixture::new();
    fixture.seed(&["cursor", "pi"]);
    let paths = Paths::from_roots(&fixture.home, &fixture.config, &fixture.state).expect("paths");
    let pi = known_clients(&paths)
        .into_iter()
        .find(|client| client.name.as_str() == "pi")
        .expect("pi client");
    fs::write(&pi.config_path, b"{ not JSON").expect("malformed later config");

    let fake_bin = fixture.home.join("fake-bin");
    fs::create_dir(&fake_bin).expect("fake binary directory");
    let fake_cargo = fake_bin.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nset -eu\n: > \"$MCP_SWAP_CARGO_MARKER\"\nroot=\nwhile [ \"$#\" -gt 0 ]; do\n    if [ \"$1\" = \"--root\" ]; then\n        shift\n        root=$1\n        break\n    fi\n    shift\ndone\n[ -n \"$root\" ]\nmkdir -p \"$root/bin\"\n: > \"$root/bin/tmux-mcp\"\n",
    )
    .expect("fake cargo");
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o700)).expect("fake cargo mode");
    let marker = fixture.state.join("cargo-invoked");

    let output = fixture
        .command()
        .env("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()))
        .env("MCP_SWAP_CARGO_MARKER", &marker)
        .args([
            "use",
            "--repo",
            fixture.repo.to_str().expect("repo path"),
            "--source",
            "published",
            "--version",
            "0.1.0",
            "--cli",
            "cursor,pi",
            "--no-preflight",
        ])
        .output()
        .expect("published use");

    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .expect("UTF-8 error")
            .contains("pi")
    );
    assert!(!marker.exists(), "cargo install ran before config planning");
}

#[test]
fn dry_run_selectors_do_not_build_preflight_or_write() {
    let fixture = CliFixture::new();
    fixture.seed(&["claude", "agy", "pi"]);
    let paths = Paths::from_roots(&fixture.home, &fixture.config, &fixture.state).expect("paths");
    let clients = known_clients(&paths);
    let originals: Vec<_> = clients
        .iter()
        .filter(|client| client.config_path.exists())
        .map(|client| {
            (
                client.config_path.clone(),
                fs::read(&client.config_path).expect("bytes"),
            )
        })
        .collect();

    let output = fixture
        .command()
        .args([
            "use",
            "--repo",
            fixture.repo.to_str().expect("repo path"),
            "--source",
            "path",
            "--bin",
            "/missing/dry-run-server",
            "--cli",
            "pi,antigravity",
            "--cli",
            "agy",
            "--dry-run",
        ])
        .output()
        .expect("dry run");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for (path, bytes) in originals {
        assert_eq!(fs::read(path).expect("unchanged config"), bytes);
    }
    assert!(!paths.state_dir().exists());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("agy"));
    assert!(stdout.contains("pi"));
    assert!(!stdout.contains("claude"));
}

#[test]
fn selected_clients_use_and_revert_with_environment_overlay() {
    let fixture = CliFixture::new();
    fixture.seed(&["cursor", "pi"]);
    let paths = Paths::from_roots(&fixture.home, &fixture.config, &fixture.state).expect("paths");
    let clients = known_clients(&paths);
    let originals: Vec<_> = clients
        .iter()
        .filter(|client| client.config_path.exists())
        .map(|client| {
            (
                client.config_path.clone(),
                fs::read(&client.config_path).expect("bytes"),
            )
        })
        .collect();

    let use_output = fixture
        .command()
        .args([
            "use",
            "--repo",
            fixture.repo.to_str().expect("repo path"),
            "--source",
            "path",
            "--bin",
            fixture.binary.to_str().expect("binary path"),
            "--no-preflight",
            "--cli",
            "pi,cursor",
            "--env",
            "LIBTMUX_TOOLSETS=standard",
        ])
        .output()
        .expect("use");
    assert!(
        use_output.status.success(),
        "{}",
        String::from_utf8_lossy(&use_output.stderr)
    );

    let status = fixture
        .command()
        .args([
            "status",
            "--repo",
            fixture.repo.to_str().expect("repo path"),
            "--cli",
            "cursor,pi",
        ])
        .output()
        .expect("status");
    assert!(status.status.success());
    let stdout = String::from_utf8(status.stdout).expect("UTF-8 status");
    assert!(stdout.contains("LIBTMUX_TOOLSETS"));

    let revert = fixture
        .command()
        .args(["revert", "--cli", "cursor,pi"])
        .output()
        .expect("revert");
    assert!(
        revert.status.success(),
        "{}",
        String::from_utf8_lossy(&revert.stderr)
    );
    for (path, bytes) in originals {
        assert_eq!(fs::read(path).expect("restored config"), bytes);
    }
    assert!(!paths.state_file().exists());
}

#[test]
fn status_lists_environment_keys_without_values() {
    let fixture = CliFixture::new();
    fixture.seed(&["cursor"]);
    let secret = "status-must-not-print-this-secret";

    let use_output = fixture
        .command()
        .args([
            "use",
            "--repo",
            fixture.repo.to_str().expect("repo path"),
            "--source",
            "path",
            "--bin",
            fixture.binary.to_str().expect("binary path"),
            "--no-preflight",
            "--cli",
            "cursor",
            "--env",
            &format!("STATUS_SECRET={secret}"),
        ])
        .output()
        .expect("use");
    assert!(
        use_output.status.success(),
        "{}",
        String::from_utf8_lossy(&use_output.stderr)
    );

    let status = fixture
        .command()
        .args([
            "status",
            "--repo",
            fixture.repo.to_str().expect("repo path"),
            "--cli",
            "cursor",
        ])
        .output()
        .expect("status");
    assert!(status.status.success());
    let stdout = String::from_utf8(status.stdout).expect("UTF-8 status");
    let stderr = String::from_utf8(status.stderr).expect("UTF-8 status error");
    assert!(stdout.contains("STATUS_SECRET"));
    assert!(!stdout.contains(secret));
    assert!(!stderr.contains(secret));
}

#[test]
fn detect_and_doctor_are_read_only() {
    let fixture = CliFixture::new();
    fixture.seed(&["pi"]);
    let detect = fixture.command().arg("detect").output().expect("detect");
    assert!(detect.status.success());
    assert!(
        String::from_utf8(detect.stdout)
            .expect("UTF-8 detect")
            .contains("pi-mcp-adapter")
    );

    let doctor = fixture
        .command()
        .args([
            "doctor",
            "--repo",
            fixture.repo.to_str().expect("repo path"),
        ])
        .output()
        .expect("doctor");
    assert!(doctor.status.success());
    assert!(
        String::from_utf8(doctor.stdout)
            .expect("UTF-8 doctor")
            .contains("mcp-swap doctor")
    );
    assert!(!fixture.state.join("libtmux-mcp-dev/swap").exists());
}

#[test]
fn concurrent_processes_serialize_replan_and_revert_to_the_first_backup() {
    let fixture = CliFixture::new();
    fixture.seed(&["cursor"]);
    let paths = Paths::from_roots(&fixture.home, &fixture.config, &fixture.state).expect("paths");
    let client = known_clients(&paths)
        .into_iter()
        .find(|client| client.name.as_str() == "cursor")
        .expect("cursor client");
    let pristine = fs::read(&client.config_path).expect("pristine config");
    let second_binary = fixture.binary.with_file_name("server-two");
    fs::write(&second_binary, "#!/bin/sh\nexit 0\n").expect("second binary");
    fs::set_permissions(&second_binary, fs::Permissions::from_mode(0o700))
        .expect("second binary mode");

    let spawn_use = |binary: &std::path::Path| {
        let mut command = fixture.command();
        command
            .args([
                "use",
                "--repo",
                fixture.repo.to_str().expect("repo path"),
                "--source",
                "path",
                "--bin",
                binary.to_str().expect("binary path"),
                "--no-preflight",
                "--cli",
                "cursor",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn use")
    };
    let first = spawn_use(&fixture.binary);
    let second = spawn_use(&second_binary);
    let first = first.wait_with_output().expect("first use");
    let second = second.wait_with_output().expect("second use");
    assert!(
        first.status.success(),
        "first: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "second: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let ledger = load_ledger(&paths.state_file()).expect("replanned recovery state");
    let entry = ledger.entries.get("cursor:user").expect("cursor entry");
    assert_eq!(
        resolve_config_route(&client.config_path, 16 * 1024 * 1024)
            .expect("current route")
            .file
            .identity,
        entry.expected_config
    );
    assert_eq!(
        stable_snapshot(&entry.backup_path, 16 * 1024 * 1024)
            .expect("first backup")
            .bytes,
        pristine
    );

    let revert = fixture
        .command()
        .args(["revert", "--cli", "cursor"])
        .output()
        .expect("revert");
    assert!(
        revert.status.success(),
        "{}",
        String::from_utf8_lossy(&revert.stderr)
    );
    assert_eq!(
        fs::read(client.config_path).expect("restored config"),
        pristine
    );
}
