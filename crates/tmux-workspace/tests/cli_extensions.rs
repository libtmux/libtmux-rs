//! Extension selection and whole-input validation through the installed CLI.
#![cfg(all(
    feature = "cli",
    not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;
use std::process::Output;
use std::time::Duration;

use libtmux::test::TestServer;
use serde_json::{Value, json};

fn write_workspace(directory: &Path, file: &str, extensions: &Value) {
    let mut source = json!({
        "session_name": "extension-workspace", "before_script": "touch script-ran",
        "options": {"@extension-changed": "yes"},
        "windows": [{"window_name": "extension-window", "panes": ["blank"]}]
    });
    source
        .as_object_mut()
        .unwrap()
        .extend(extensions.as_object().unwrap().clone());
    std::fs::write(directory.join(file), source.to_string()).unwrap();
}

async fn load(directory: &Path, guard: &TestServer, files: &[&str], pane: Option<&str>) -> Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_tmux-workspace"));
    command
        .arg("load")
        .args(files)
        .arg(if pane.is_some() { "--append" } else { "-d" })
        .arg("-S")
        .arg(guard.socket_path())
        .arg("--json")
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("LIBTMUX_TEST_TMUX", guard.server().tmux_executable())
        .env("HOME", directory)
        .env("SHELL", "/bin/sh")
        .env("TMUXP_CONFIGDIR", directory.join(".tmuxp"))
        .env("XDG_CONFIG_HOME", directory.join(".config"))
        .env("TMUX_WORKSPACE_PYTHON", directory.join("missing-python"))
        .kill_on_drop(true);
    if let Some(pane) = pane {
        command.env("TMUX_PANE", pane).env(
            "TMUX",
            format!("{},{},0", guard.socket_path().display(), guard.daemon_pid()),
        );
    }
    tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("CLI extension load exceeded its deadline")
        .unwrap()
}

async fn keeper_pane(keeper: &libtmux::Session) -> String {
    keeper.panes().await.unwrap()[0].id().to_string()
}

async fn assert_keeper(guard: &TestServer, keeper: &libtmux::Session, pane: &str) {
    let sessions = guard.server().sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id(), keeper.id());
    assert_eq!(keeper.windows().await.unwrap().len(), 1);
    assert_eq!(keeper.panes().await.unwrap().len(), 1);
    assert_eq!(keeper_pane(keeper).await, pane);
    assert!(
        keeper
            .get_option("@extension-changed")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn neutral_extensions_load_natively_without_python() {
    let guard = TestServer::new().await.unwrap();
    let keeper = guard.session("extension-keeper").await.unwrap();
    let pane = keeper_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    for extensions in [
        json!({}),
        json!({"plugins": []}),
        json!({"workspace_builder": null}),
        json!({"workspace_builder": ""}),
        json!({"plugins": [], "workspace_builder": null}),
        json!({"plugins": [], "workspace_builder": ""}),
    ] {
        write_workspace(directory.path(), "workspace.json", &extensions);
        let output = load(directory.path(), &guard, &["workspace.json"], None).await;
        assert!(output.status.success(), "{extensions}: {output:?}");
        assert!(directory.path().join("script-ran").exists());
        std::fs::remove_file(directory.path().join("script-ran")).unwrap();
        let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(summary["status"], "ok", "{summary}");
        let loaded = guard
            .server()
            .session("extension-workspace")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.windows().await.unwrap().len(), 1);
        assert_eq!(loaded.panes().await.unwrap().len(), 1);
        assert_eq!(
            loaded
                .get_option("@extension-changed")
                .await
                .unwrap()
                .unwrap()
                .to_string_lossy(),
            "yes"
        );
        loaded.kill().await.unwrap();
        assert_keeper(&guard, &keeper, &pane).await;
    }
    let output = load(directory.path(), &guard, &["workspace.json"], Some(&pane)).await;
    assert!(output.status.success(), "{output:?}");
    assert!(directory.path().join("script-ran").exists());
    assert_eq!(guard.server().sessions().await.unwrap().len(), 1);
    assert_eq!(
        guard.server().sessions().await.unwrap()[0].id(),
        keeper.id()
    );
    assert_eq!(keeper.windows().await.unwrap().len(), 2);
    let panes = keeper.panes().await.unwrap();
    assert_eq!(panes.len(), 2);
    assert!(panes.iter().any(|current| current.id().to_string() == pane));
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_extensions_keep_the_explicit_python_bridge() {
    let guard = TestServer::new().await.unwrap();
    let keeper = guard.session("extension-keeper").await.unwrap();
    let pane = keeper_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    for extensions in [
        json!({"plugins": ["extension.Plugin"]}),
        json!({"plugins": [""]}),
        json!({"workspace_builder": "classic"}),
        json!({"workspace_builder": "extension:Builder"}),
        json!({"workspace_builder": " "}),
        json!({"plugins": [], "workspace_builder": "extension.Builder"}),
        json!({"plugins": ["extension.Plugin"], "workspace_builder": null}),
    ] {
        write_workspace(directory.path(), "workspace.json", &extensions);
        let output = load(directory.path(), &guard, &["workspace.json"], None).await;
        assert_eq!(output.status.code(), Some(1), "{extensions}: {output:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["code"], "python_runtime", "{extensions}: {error}");
        assert!(output.stdout.is_empty());
        assert!(!directory.path().join("script-ran").exists());
        assert_keeper(&guard, &keeper, &pane).await;
    }
    guard.shutdown().await.unwrap();
}

fn invalid_extensions() -> Vec<Value> {
    let mut cases = Vec::new();
    for plugins in [json!(null), json!(false), json!(0), json!(""), json!({})] {
        cases.push(json!({"plugins": plugins}));
    }
    for plugin in [json!(null), json!(false), json!(0), json!([]), json!({})] {
        cases.push(json!({"plugins": ["extension.Plugin", plugin]}));
    }
    for builder in [
        json!(false),
        json!(0),
        json!([]),
        json!({}),
        json!(["classic"]),
    ] {
        cases.push(json!({"workspace_builder": builder}));
    }
    cases.push(json!({"plugins": ["extension.Plugin"], "workspace_builder": false}));
    cases.push(json!({"plugins": false, "workspace_builder": "extension:Builder"}));
    cases
}

#[tokio::test]
async fn invalid_extensions_refuse_all_inputs_before_scripts_or_append() {
    let guard = TestServer::new().await.unwrap();
    let keeper = guard.session("extension-keeper").await.unwrap();
    let pane = keeper_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    write_workspace(directory.path(), "first.json", &json!({}));
    for extensions in invalid_extensions() {
        write_workspace(directory.path(), "second.json", &extensions);
        for current in [None, Some(pane.as_str())] {
            let output = load(
                directory.path(),
                &guard,
                &["first.json", "second.json"],
                current,
            )
            .await;
            assert!(!directory.path().join("script-ran").exists());
            assert_keeper(&guard, &keeper, &pane).await;
            assert_eq!(output.status.code(), Some(1), "{extensions}: {output:?}");
            let error: Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(error["code"], "invalid_config", "{extensions}: {error}");
            assert!(output.stdout.is_empty());
        }
    }
    guard.shutdown().await.unwrap();
}
