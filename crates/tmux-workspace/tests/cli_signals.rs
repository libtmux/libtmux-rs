//! Native load cancellation preserves acknowledged effects and owned processes.
#![cfg(feature = "cli")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::{path::Path, process::Stdio, time::Duration};

use rustix::process::{Pid, Signal};
use serde_json::{Value, json};

#[tokio::test]
async fn load_signal_int_retains_known_effects() {
    for append in [false, true] {
        for mode in ["--ndjson", "--json", "human"] {
            check_signal(Signal::INT, append, mode).await;
        }
    }
}

#[tokio::test]
async fn load_signal_term_retains_known_effects() {
    for append in [false, true] {
        for mode in ["--ndjson", "--json", "human"] {
            check_signal(Signal::TERM, append, mode).await;
        }
    }
}

#[tokio::test]
async fn interrupted_mutation_reports_unknown_unacknowledged_effects() {
    for signal in [Signal::INT, Signal::TERM] {
        let guard = libtmux::test::TestServer::new().await.unwrap();
        let keeper = guard.session("mutation-keeper").await.unwrap();
        let initial = topology(&keeper).await;
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
        for name in ["first", "second"] {
            std::fs::write(
                directory.path().join(format!("{name}.json")),
                json!({"session_name":name,"windows":[{"panes":["blank"]}]}).to_string(),
            )
            .unwrap();
        }
        let wrapper = delaying_wrapper(directory.path());
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
            .args(["load", "-d", "--ndjson", "-S"])
            .arg(guard.socket_path())
            .args(["first.json", "second.json"])
            .current_dir(directory.path())
            .env("HOME", directory.path())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("LIBTMUX_TEST_TMUX", &wrapper)
            .env("REAL_SIGNAL_TMUX", guard.server().tmux_executable())
            .env("SIGNAL_DIR", directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pending_pid = marker(&directory.path().join("committed.pid")).await[0];
        let actual = guard.server().session("second").await.unwrap().unwrap();
        let actual_id = actual.id().to_string();
        assert!(alive(pending_pid));
        rustix::process::kill_process(
            Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()).unwrap(),
            signal,
        )
        .unwrap();
        let bounded = tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .is_ok();
        if !bounded {
            child.kill().await.unwrap();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while alive(pending_pid) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let survived = alive(pending_pid);
        if survived {
            let _ =
                rustix::process::kill_process(Pid::from_raw(pending_pid).unwrap(), Signal::KILL);
        }
        let output = tokio::time::timeout(Duration::from_secs(2), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(bounded);
        assert!(!survived, "pending native command survived cancellation");
        assert_eq!(output.status.code(), Some(130));
        assert_eq!(topology(&keeper).await, initial);
        assert_eq!(
            guard
                .server()
                .session("second")
                .await
                .unwrap()
                .unwrap()
                .id()
                .to_string(),
            actual_id
        );
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        let state = &diagnostic["retained_state"];
        assert_eq!(state["results"].as_array().unwrap().len(), 1);
        assert_eq!(state["errors"][0]["outcome_unknown"], true, "{state}");
        assert_eq!(state["errors"][0]["input_index"], 1);
        assert!(
            state["errors"][0]["effects"]["session_id"].is_null(),
            "unacknowledged session receipt was invented"
        );
        let records: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.last().unwrap()["event"], "failed");
        assert_eq!(records.last().unwrap()["errors"], state["errors"]);
        guard.shutdown().await.unwrap();
    }
}

fn delaying_wrapper(directory: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let wrapper = directory.join("tmux-wrapper");
    std::fs::write(
        &wrapper,
        r#"#!/bin/sh
new=0
second=0
for argument do
    [ "$argument" = new-session ] && new=1
    [ "$argument" = second ] && second=1
done
if [ "$new" = 1 ] && [ "$second" = 1 ]; then
    "$REAL_SIGNAL_TMUX" "$@" > "$SIGNAL_DIR/reply" || exit
    printf '%s\n' "$$" > "$SIGNAL_DIR/committed.pid"
    exec sleep 30
fi
exec "$REAL_SIGNAL_TMUX" "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    wrapper
}

struct ScriptGroup(Option<Pid>);

impl Drop for ScriptGroup {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = rustix::process::kill_process_group(group, Signal::KILL);
        }
    }
}

async fn marker(path: &Path) -> Vec<i32> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path) {
                let values: Vec<_> = text.split_whitespace().map(str::parse::<i32>).collect();
                if !values.is_empty() && values.iter().all(Result::is_ok) {
                    return values.into_iter().map(Result::unwrap).collect();
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("script readiness marker")
}

fn alive(pid: i32) -> bool {
    let result = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    assert!(result.status.success() || result.status.code() == Some(1));
    let state = String::from_utf8(result.stdout).unwrap();
    !state.trim().is_empty() && !state.trim().starts_with('Z')
}

async fn topology(session: &libtmux::Session) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    for window in session.windows().await.unwrap() {
        result.push((
            window.id().to_string(),
            window
                .panes()
                .await
                .unwrap()
                .iter()
                .map(|pane| pane.id().to_string())
                .collect(),
        ));
    }
    result
}

fn retained_summary(output: std::process::Output, mode: &str) -> Value {
    if mode == "human" {
        let stderr = String::from_utf8(output.stderr).unwrap();
        serde_json::from_str(
            stderr
                .lines()
                .find_map(|line| line.strip_prefix("Retained state: "))
                .expect("human retained-state diagnostic"),
        )
        .unwrap()
    } else {
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["code"], "interrupted");
        let summary = diagnostic["retained_state"].clone();
        assert!(
            summary.is_object(),
            "cancellation lost retained effects: {diagnostic}"
        );
        let records: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if mode == "--ndjson" {
            let finals: Vec<_> = records
                .iter()
                .filter(|record| record["event"] == "failed" || record["event"] == "completed")
                .collect();
            assert_eq!(finals.len(), 1);
            assert_eq!(finals[0]["event"], "failed");
            assert_eq!(finals[0]["results"], summary["results"]);
            assert_eq!(finals[0]["errors"], summary["errors"]);
        } else {
            assert_eq!(records, std::slice::from_ref(&summary));
        }
        summary
    }
}

fn assert_retained_input(summary: &Value, retained_id: &str, append: bool) {
    assert_eq!(summary["status"], "partial");
    assert_eq!(summary["results"].as_array().unwrap().len(), 1);
    let effects = &summary["errors"][0]["effects"];
    assert_eq!(summary["errors"][0]["input_index"], 1);
    assert_eq!(effects["session_id"], retained_id);
    assert_eq!(effects["owned_session"], !append);
    assert_eq!(effects["stage"], "before-script");
    assert_eq!(summary["errors"][0]["partial_effects"], true);
}

async fn check_signal(signal: Signal, append: bool, mode: &str) {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("signal-keeper").await.unwrap();
    let initial = topology(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(directory.path().join("ready.sh"),
        "printf '%s\\n' \"$$\" > parent.pid\nsleep 30 &\nprintf '%s %s\\n' \"$$\" \"$!\" > ready.tmp\nmv ready.tmp ready.pid\nwait\n").unwrap();
    for name in ["first", "second"] {
        let mut config =
            json!({"session_name":name,"windows":[{"window_name":name,"panes":["blank"]}]});
        if name == "second" {
            config["before_script"] = json!("/bin/sh ready.sh");
        }
        std::fs::write(
            directory.path().join(format!("{name}.json")),
            config.to_string(),
        )
        .unwrap();
    }
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_tmux-workspace"));
    command
        .args(["load", if append { "--append" } else { "-d" }, "-S"])
        .arg(guard.socket_path())
        .args(["first.json", "second.json"])
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .env("TMUXP_CONFIGDIR", directory.path().join(".tmuxp"))
        .env("XDG_CONFIG_HOME", directory.path().join(".config"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if mode != "human" {
        command.arg(mode);
    }
    if append {
        command.env("TMUX_PANE", &initial[0].1[0]).env(
            "TMUX",
            format!("{},{},0", guard.socket_path().display(), guard.daemon_pid()),
        );
    }
    let mut child = command.spawn().unwrap();
    let parent = marker(&directory.path().join("parent.pid")).await[0];
    let mut group = ScriptGroup(Some(Pid::from_raw(parent).unwrap()));
    let pids = marker(&directory.path().join("ready.pid")).await;
    assert_eq!(pids.len(), 2);
    assert_eq!(pids[0], parent);
    assert!(pids.iter().all(|pid| alive(*pid)));
    let before = if append {
        keeper.clone()
    } else {
        guard.server().session("second").await.unwrap().unwrap()
    };
    let retained_id = before.id().to_string();
    let retained_topology = topology(&before).await;
    rustix::process::kill_process(
        Pid::from_raw(i32::try_from(child.id().unwrap()).unwrap()).unwrap(),
        signal,
    )
    .unwrap();
    let bounded = tokio::time::timeout(Duration::from_secs(3), child.wait())
        .await
        .is_ok();
    if !bounded {
        child.kill().await.unwrap();
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while pids.iter().any(|pid| alive(*pid)) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let survivors: Vec<_> = pids.iter().copied().filter(|pid| alive(*pid)).collect();
    if survivors.is_empty() {
        group.0 = None;
    }
    drop(group);
    let output = tokio::time::timeout(Duration::from_secs(2), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(bounded, "CLI signal cleanup exceeded deadline");
    assert!(
        survivors.is_empty(),
        "owned script processes survived {signal:?}: {survivors:?}"
    );
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    assert_eq!(topology(&before).await, retained_topology);
    let keeper_after = topology(&keeper).await;
    if append {
        assert_eq!(keeper_after.len(), initial.len() + 1);
        assert_eq!(keeper_after[0], initial[0]);
    } else {
        assert_eq!(keeper_after, initial);
        assert_eq!(guard.server().sessions().await.unwrap().len(), 3);
    }
    assert_retained_input(&retained_summary(output, mode), &retained_id, append);
    guard.shutdown().await.unwrap();
}
