//! Foreground commands that become background work.
//!
//! These tests drive the MCP tools against real tmux servers. Channel and
//! hook gates put each request at the ownership boundary under test.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Pane, Server, Session};
use tmux_mcp::TmuxTools;
use tokio_util::sync::CancellationToken;

mod support;

use support::{args, bare_tools, json};

async fn wait_for_prompt(pane: &Pane) {
    for _ in 0..600 {
        let lines = pane.capture().await.expect("pane captures");
        if lines
            .iter()
            .any(|line| matches!(line.as_bytes().last(), Some(b'$' | b'#')))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the pane never drew a prompt");
}

async fn fixture(name: &str) -> (TestServer, Session, Pane, TmuxTools) {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session(name)
        .await
        .expect("session starts");
    let pane = session.panes().await.expect("panes list").remove(0);
    wait_for_prompt(&pane).await;
    let tools = bare_tools(guard.server());
    (guard, session, pane, tools)
}

async fn wait_for_job_output(tools: &TmuxTools, job: &str, marker: &str) {
    libtmux::test::retry_until(Duration::from_secs(5), async || {
        tools
            .job_status(args(serde_json::json!({"job": job})))
            .await
            .ok()
            .is_some_and(|answer| {
                json(answer)["output"]
                    .as_str()
                    .is_some_and(|output| output.contains(marker))
            })
    })
    .await
    .expect("the owned reader retains the command output");
}

async fn forget_and_release(tools: &TmuxTools, guard: &TestServer, job: &str, release: &str) {
    let forgotten = json(
        tools
            .forget_job(args(serde_json::json!({"job": job})))
            .await
            .expect("the retained job can be forgotten"),
    );
    assert_eq!(forgotten["job"], job);
    guard
        .server()
        .signal_channel(release)
        .await
        .expect("the command gate is released");
}

#[tokio::test]
async fn uncertain_foreground_dispatch_names_the_owned_job() {
    let (guard, session, pane, original_tools) = fixture("foreground-unknown").await;
    let short = Server::builder()
        .socket_path(guard.server().socket_path())
        .config_file(guard.server().config_file().expect("the fixture config"))
        .tmux_executable(guard.server().tmux_executable())
        .default_timeout(Duration::from_secs(2))
        .build()
        .expect("a short-deadline handle builds");
    let tools = bare_tools(&short);
    drop(original_tools);
    let sent = "foreground-unknown-sent";
    let release = "foreground-unknown-release";
    session
        .set_hook(
            "after-send-keys",
            format!("if-shell -F '#{{hook_flag_l}}' 'wait-for -S {sent}; wait-for {release}'"),
        )
        .await
        .expect("the dispatch gate is installed");

    let Err(error) = tools
        .run_command(
            args(serde_json::json!({
                "pane": pane.id().to_string(),
                "command": "sleep 60",
                "seconds": 20,
            })),
            CancellationToken::new(),
            tmux_mcp::Reporter::none(),
        )
        .await
    else {
        panic!("the blocked dispatch reply was reported as confirmed");
    };

    let data = error.data.expect("the error carries recovery data");
    assert_eq!(data["kind"], "dispatch_unknown");
    assert_eq!(data["retryable"], false);
    let job = data["job"]
        .as_str()
        .expect("the uncertain dispatch names its owner")
        .to_owned();
    assert!(
        json(tools.list_jobs().await.expect("jobs list"))["jobs"]
            .as_array()
            .expect("jobs is an array")
            .iter()
            .any(|view| view["job"] == job),
        "the named job is discoverable",
    );
    assert_eq!(
        guard
            .server()
            .wait_for_channel(sent, Duration::from_secs(5))
            .await
            .expect("the dispatch gate can be read"),
        libtmux::ChannelWait::Signalled,
        "the literal send did not reach the hook",
    );

    guard
        .server()
        .signal_channel(release)
        .await
        .expect("the blocked dispatch reply is released");
    let status = json(
        tools
            .job_status(args(serde_json::json!({"job": job})))
            .await
            .expect("the uncertain job remains inspectable"),
    );
    assert_eq!(status["job"], job);
    assert_eq!(status["state"], "dispatch_unknown");
    let forgotten = json(
        tools
            .forget_job(args(serde_json::json!({"job": job})))
            .await
            .expect("the uncertain job can be forgotten"),
    );
    assert_eq!(forgotten["job"], job);

    drop(tools);
    short
        .shutdown()
        .await
        .expect("the short executor shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_foreground_deadline_returns_a_recoverable_job() {
    let (guard, _session, pane, tools) = fixture("foreground-deadline").await;
    let marker = "foreground-deadline-output";
    let started = "foreground-deadline-started";
    let release = "foreground-deadline-release";

    let result = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane.id().to_string(),
                    "command": format!(
                        "printf '{marker}\\n'; tmux wait-for -S {started}; tmux wait-for {release}"
                    ),
                    "seconds": 1,
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("the deadline is an answer"),
    );

    assert_eq!(result["outcome"], "deadline");
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|text| text.contains(marker))
    );
    let job = result["job"]
        .as_str()
        .expect("the unfinished run names its retained job")
        .to_owned();
    wait_for_job_output(&tools, &job, marker).await;
    forget_and_release(&tools, &guard, &job, release).await;

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn cancelling_a_foreground_request_returns_a_recoverable_job() {
    let (guard, _session, pane, tools) = fixture("foreground-cancelled").await;
    let marker = "foreground-cancelled-output";
    let started = "foreground-cancelled-started";
    let release = "foreground-cancelled-release";
    let cancelled = CancellationToken::new();
    let running = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.id().to_string();
        let cancelled = cancelled.clone();
        async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": format!(
                            "printf '{marker}\\n'; tmux wait-for -S {started}; tmux wait-for {release}"
                        ),
                        "seconds": 20,
                    })),
                    cancelled,
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });

    assert_eq!(
        guard
            .server()
            .wait_for_channel(started, Duration::from_secs(5))
            .await
            .expect("the command gate can be read"),
        libtmux::ChannelWait::Signalled,
        "the command did not start",
    );
    cancelled.cancel();
    let result = json(
        running
            .await
            .expect("the request task stays healthy")
            .expect("cancellation is an answer"),
    );

    assert_eq!(result["outcome"], "cancelled");
    assert!(
        result["output"]
            .as_str()
            .is_some_and(|text| text.contains(marker))
    );
    let job = result["job"]
        .as_str()
        .expect("the cancelled run names its retained job")
        .to_owned();
    wait_for_job_output(&tools, &job, marker).await;
    forget_and_release(&tools, &guard, &job, release).await;

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn dropping_a_foreground_future_leaves_a_discoverable_job() {
    let (guard, _session, pane, tools) = fixture("foreground-dropped").await;
    let marker = "foreground-dropped-output";
    let started = "foreground-dropped-started";
    let release = "foreground-dropped-release";
    let command =
        format!("printf '{marker}\\n'; tmux wait-for -S {started}; tmux wait-for {release}");
    let running = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.id().to_string();
        let command = command.clone();
        async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": command,
                        "seconds": 20,
                    })),
                    CancellationToken::new(),
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });

    assert_eq!(
        guard
            .server()
            .wait_for_channel(started, Duration::from_secs(5))
            .await
            .expect("the command gate can be read"),
        libtmux::ChannelWait::Signalled,
        "the command did not start",
    );
    running.abort();
    let Err(dropped) = running.await else {
        panic!("the dropped request future returned");
    };
    assert!(dropped.is_cancelled());

    let jobs = json(tools.list_jobs().await.expect("jobs list"));
    let owned = jobs["jobs"]
        .as_array()
        .expect("jobs is an array")
        .iter()
        .find(|view| view["command"] == command)
        .expect("the dropped request left a discoverable owner");
    let job = owned["job"]
        .as_str()
        .expect("the owner has an id")
        .to_owned();
    wait_for_job_output(&tools, &job, marker).await;
    forget_and_release(&tools, &guard, &job, release).await;

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn forgetting_a_foreground_job_wakes_its_waiter() {
    let (guard, _session, pane, tools) = fixture("foreground-forgotten").await;
    let started = "foreground-forgotten-started";
    let release = "foreground-forgotten-release";
    let running = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.id().to_string();
        async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": format!(
                            "tmux wait-for -S {started}; tmux wait-for {release}"
                        ),
                        "seconds": 20,
                    })),
                    CancellationToken::new(),
                    tmux_mcp::Reporter::none(),
                )
                .await
        }
    });

    assert_eq!(
        guard
            .server()
            .wait_for_channel(started, Duration::from_secs(5))
            .await
            .expect("the command gate can be read"),
        libtmux::ChannelWait::Signalled,
        "the command did not start",
    );
    let jobs = json(tools.list_jobs().await.expect("jobs list"));
    let job = jobs["jobs"]
        .as_array()
        .expect("jobs is an array")
        .first()
        .and_then(|job| job["job"].as_str())
        .expect("the foreground run is visible")
        .to_owned();
    tools
        .forget_job(args(serde_json::json!({"job": job})))
        .await
        .expect("the foreground owner can be forgotten");
    guard
        .server()
        .signal_channel(release)
        .await
        .expect("the command gate is released");

    let answer = tokio::time::timeout(Duration::from_secs(2), running)
        .await
        .expect("forgetting the owner wakes the foreground waiter")
        .expect("the foreground task stays healthy");
    let Err(error) = answer else {
        panic!("a forgotten owner returned a stale recovery result");
    };
    let data = error.data.expect("the error carries a stable kind");
    assert_eq!(data["kind"], "startup_stopped");
    assert!(data.get("job").is_none(), "no stale job id is advertised");

    guard.shutdown().await.expect("tmux fixture shuts down");
}
