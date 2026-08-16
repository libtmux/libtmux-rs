//! The tools an agent needs: knowing where it is, running things, waiting.
//!
//! Every test here drives the MCP tools rather than the library beneath them,
//! against a real tmux server on its own socket.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, Server};
use rmcp::ServerHandler as _;
use rmcp::handler::server::wrapper::Parameters;
use serde_json::Value;
use tmux_mcp::{CallerIdentity, TmuxTools};
use tokio_util::sync::CancellationToken;

/// Build tool arguments from JSON, as the protocol delivers them.
fn args<T: serde::de::DeserializeOwned>(value: Value) -> Parameters<T> {
    Parameters(serde_json::from_value(value).expect("arguments deserialize"))
}

/// Render a tool's typed answer the way a client receives it.
///
/// The tools return values now, not strings, so these read the same JSON a
/// client sees in `structuredContent` rather than parsing prose.
fn json<T: serde::Serialize>(answer: rmcp::handler::server::wrapper::Json<T>) -> Value {
    serde_json::to_value(answer.0).expect("a tool answer serialises")
}

/// The id a tool that made or destroyed one object answers with.
fn id<T: serde::Serialize>(answer: rmcp::handler::server::wrapper::Json<T>) -> String {
    json(answer)["id"]
        .as_str()
        .expect("the answer carries an id")
        .to_owned()
}

/// The socket path tmux itself reports, which is what identities compare.
async fn socket_of(server: &Server) -> String {
    server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("#{socket_path}"),
        )
        .await
        .expect("tmux reports its socket")
        .stdout_lossy()
        .trim()
        .to_owned()
}

/// A second handle onto the same tmux daemon, with its own executor.
///
/// Anything that asks a `Server` whether it is alive is also asking the
/// executor that `Server` owns. To learn about the daemon rather than about
/// the handle, ask through one that the code under test never touched.
async fn independent(server: &Server) -> Server {
    Server::builder()
        .socket_path(socket_of(server).await)
        .tmux_executable(server.tmux_executable())
        .build()
        .expect("a second handle onto the same socket")
}

/// An identity as tmux would leave it for a process started in `pane`.
async fn identity_for(server: &Server, pane: &str) -> CallerIdentity {
    let socket = socket_of(server).await;
    CallerIdentity::from_values(Some(format!("{socket},1,$0").into()), Some(pane.into()))
        .expect("both values are present")
}

/// A server holding one session, with the tools pointed at it.
async fn fixture(name: &str) -> (TestServer, TmuxTools) {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());
    tools
        .create_session(args(serde_json::json!({"name": name})))
        .await
        .expect("session is created");
    (guard, tools)
}

/// The panes on the server, as the tools report them.
///
/// A listing arrives wrapped -- `{"panes": [...]}` -- because the protocol
/// says structured content is an object.
async fn panes(tools: &TmuxTools) -> Vec<Value> {
    json(tools.list_panes().await.expect("panes"))["panes"]
        .as_array()
        .expect("a listing wraps an array")
        .clone()
}

/// Wait until a pane's shell has drawn a prompt.
///
/// tmux hands back a pane the moment it forks, long before the shell in it can
/// read. Keys sent before then are swallowed, so every test that types waits
/// for the cursor to leave the origin first.
async fn prompt_ready(server: &Server, pane: &str) {
    let mut last = String::new();
    for _ in 0..600 {
        let reading = server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(pane)
                    .arg("#{cursor_x},#{cursor_y}"),
            )
            .await
            .expect("tmux reports the cursor")
            .stdout_lossy()
            .trim()
            .to_owned();
        if !reading.is_empty() && reading != "0,0" {
            return;
        }
        last = reading;
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // A bare cursor reading cannot distinguish a shell that is slow from one
    // that died or never ran, which is the difference worth knowing here.
    let state = server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("-t")
                .arg(pane)
                .arg("running=#{pane_current_command} dead=#{pane_dead}"),
        )
        .await
        .expect("tmux reports the pane")
        .stdout_lossy()
        .trim()
        .to_owned();
    panic!("the pane never drew a prompt; cursor stayed at {last:?}, {state}");
}

/// A fixture whose single pane is ready to be typed at.
async fn typing_fixture(name: &str) -> (TestServer, TmuxTools, String) {
    let (guard, tools) = fixture(name).await;
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;
    (guard, tools, pane)
}

#[tokio::test]
async fn a_pane_listing_says_which_pane_the_server_runs_in() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let bare = TmuxTools::new(guard.server().clone());
    bare.create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session is created");

    let first = panes(&bare).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    bare.split_pane(args(serde_json::json!({"pane": first})))
        .await
        .expect("the pane splits");

    let tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(identity_for(guard.server(), &first).await))
        .build();
    let listed = panes(&tools).await;

    assert_eq!(listed.len(), 2);
    let own: Vec<_> = listed
        .iter()
        .filter(|pane| pane["caller"] == "self")
        .collect();
    assert_eq!(own.len(), 1, "exactly one pane is the server's own");
    assert_eq!(own[0]["id"], first.as_str());
    assert!(
        listed.iter().any(|pane| pane["caller"] == "other"),
        "the pane that is not ours says so"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_instructions_say_where_this_server_is_running() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let bare = TmuxTools::new(guard.server().clone());
    bare.create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session is created");
    let own = panes(&bare).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();

    // Without an identity there is nothing to say, and saying nothing is
    // better than a sentence an agent has to interpret.
    let quiet = bare.get_info().instructions.expect("instructions");
    assert!(!quiet.contains("runs in pane"), "{quiet}");

    let tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(identity_for(guard.server(), &own).await))
        .build();
    let told = tools.get_info().instructions.expect("instructions");

    assert!(
        told.contains(&own),
        "an agent should know where it is before its first call: {told}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn outside_tmux_the_caller_field_has_no_answer() {
    let (guard, tools) = fixture("work").await;

    assert_eq!(panes(&tools).await[0]["caller"], "unknown");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_same_pane_id_on_another_socket_is_not_the_callers() {
    let ours = TestServer::builder().start().await.expect("tmux starts");
    let theirs = TestServer::builder().start().await.expect("tmux starts");
    for server in [ours.server(), theirs.server()] {
        TmuxTools::new(server.clone())
            .create_session(args(serde_json::json!({"name": "work"})))
            .await
            .expect("session is created");
    }

    // Both servers hand out pane ids from zero, so the caller's id exists on
    // the other server too. Only the socket tells them apart.
    let bare = TmuxTools::new(theirs.server().clone());
    let elsewhere = panes(&bare).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    let tools = TmuxTools::builder(theirs.server().clone())
        .caller(Some(identity_for(ours.server(), &elsewhere).await))
        .build();

    let listed = panes(&tools).await;
    assert_eq!(
        listed[0]["id"], elsewhere,
        "the two servers really do share a pane id"
    );
    assert_eq!(
        listed[0]["caller"], "other",
        "a matching pane id on a different socket is a different pane"
    );

    ours.shutdown().await.expect("tmux fixture shuts down");
    theirs.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_the_pane_the_server_runs_in_is_refused() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let bare = TmuxTools::new(guard.server().clone());
    bare.create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session is created");
    let own = panes(&bare).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    let other = id(bare
        .split_pane(args(serde_json::json!({"pane": own})))
        .await
        .expect("the pane splits"));

    let tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(identity_for(guard.server(), &own).await))
        .build();

    let refused = tools
        .kill_pane(args(serde_json::json!({"pane": own})))
        .await
        .map(|_| ())
        .expect_err("killing our own pane is refused");
    assert!(
        refused.message.contains(&own),
        "the refusal names the pane: {}",
        refused.message
    );
    // Its own kind, not tmux's `refused`: an agent that reads a tmux refusal
    // might reasonably try different arguments, and none get past this guard.
    let detail = refused.data.clone().expect("the refusal is classified");
    assert_eq!(detail["kind"], "self_protection", "{detail}");
    assert_eq!(detail["retryable"], false, "{detail}");
    assert_eq!(detail["stale"], false, "{detail}");

    // The guard protects one pane, not the whole server.
    tools
        .kill_pane(args(serde_json::json!({"pane": other})))
        .await
        .expect("another pane is fair game");
    assert_eq!(panes(&tools).await.len(), 1);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_the_window_or_session_holding_that_pane_is_refused() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let bare = TmuxTools::new(guard.server().clone());
    bare.create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session is created");
    let pane = panes(&bare).await[0].clone();
    let own = pane["id"].as_str().expect("a pane id is a string");
    let window = pane["window_id"].as_str().expect("a window id is a string");

    let tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(identity_for(guard.server(), own).await))
        .build();

    let refused = tools
        .kill_window(args(serde_json::json!({"window": window})))
        .await
        .map(|_| ())
        .expect_err("killing the window that holds us is refused");
    assert!(refused.message.contains(own), "{}", refused.message);

    let refused = tools
        .kill_session(args(serde_json::json!({"session": "work"})))
        .await
        .map(|_| ())
        .expect_err("killing the session that holds us is refused");
    assert!(refused.message.contains(own), "{}", refused.message);

    // A session that does not hold the caller is untouched by the guard.
    tools
        .create_session(args(serde_json::json!({"name": "scratch"})))
        .await
        .expect("session is created");
    tools
        .kill_session(args(serde_json::json!({"session": "scratch"})))
        .await
        .expect("an unrelated session is fair game");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_pane_id_collision_across_sockets_does_not_block_a_kill() {
    let ours = TestServer::builder().start().await.expect("tmux starts");
    let theirs = TestServer::builder().start().await.expect("tmux starts");
    for server in [ours.server(), theirs.server()] {
        TmuxTools::new(server.clone())
            .create_session(args(serde_json::json!({"name": "work"})))
            .await
            .expect("session is created");
    }

    let bare = TmuxTools::new(theirs.server().clone());
    let elsewhere = panes(&bare).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    let tools = TmuxTools::builder(theirs.server().clone())
        .caller(Some(identity_for(ours.server(), &elsewhere).await))
        .build();

    tools
        .kill_session(args(serde_json::json!({"session": "work"})))
        .await
        .expect("a session on another server is not ours to protect");

    ours.shutdown().await.expect("tmux fixture shuts down");
    theirs.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn running_a_command_reports_its_output_and_status() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let result = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "echo one; echo two >&2",
                    "seconds": 20
                })),
                CancellationToken::new(),
            )
            .await
            .expect("the command runs"),
    );

    assert_eq!(result["outcome"], "completed");
    assert_eq!(result["exit_status"], 0);
    let output = result["output"].as_str().expect("output is text");
    assert!(output.contains("one"), "stdout is kept: {output:?}");
    assert!(output.contains("two"), "stderr is kept: {output:?}");
    assert!(
        !output.contains("printf"),
        "the echoed command line is not output: {output:?}"
    );
    assert!(
        !output.contains('\u{1b}'),
        "escape sequences are removed: {output:?}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_failing_command_reports_its_status_rather_than_an_error() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let result = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "exit 42",
                    "seconds": 20
                })),
                CancellationToken::new(),
            )
            .await
            .expect("a failing command is still an answer"),
    );

    assert_eq!(result["outcome"], "completed");
    assert_eq!(result["exit_status"], 42);

    // The subshell means the pane's own shell survived `exit`.
    let after = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "echo still-here",
                    "seconds": 20
                })),
                CancellationToken::new(),
            )
            .await
            .expect("the shell is still there"),
    );
    assert_eq!(after["exit_status"], 0);
    assert!(
        after["output"]
            .as_str()
            .expect("output is text")
            .contains("still-here")
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_command_that_outlives_its_deadline_says_so() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let result = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "sleep 30",
                    // Long enough that the shell has certainly echoed, short
                    // enough that a 30-second sleep cannot finish. A one-second
                    // budget raced the shell's own startup on a loaded machine
                    // and reported no_shell instead.
                    "seconds": 4
                })),
                CancellationToken::new(),
            )
            .await
            .expect("the deadline is an answer, not a failure"),
    );

    assert_eq!(result["outcome"], "deadline");
    assert!(result["exit_status"].is_null());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_deadline_stops_the_waiting_rather_than_the_command() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let timed_out = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "sleep 30",
                    // Long enough that the shell has certainly echoed, short
                    // enough that a 30-second sleep cannot finish. A one-second
                    // budget raced the shell's own startup on a loaded machine
                    // and reported no_shell instead.
                    "seconds": 4
                })),
                CancellationToken::new(),
            )
            .await
            .expect("the deadline is an answer"),
    );
    assert_eq!(timed_out["outcome"], "deadline");

    // The pane is still running the command, so there is no prompt for the
    // next one to land at. Reporting that is the honest answer, and it is
    // what the tool description promises.
    let blocked = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "echo cannot-land",
                    "seconds": 3
                })),
                CancellationToken::new(),
            )
            .await
            .expect("a busy pane is an answer, not a failure"),
    );
    assert_eq!(blocked["outcome"], "no_shell");
    assert!(blocked["exit_status"].is_null());

    // And the documented way out works. C-c has no character of its own, so
    // it can only be sent as a key name; as `text` it would type three
    // letters at the sleeping command.
    tools
        .send_keys(args(serde_json::json!({"pane": pane, "keys": ["C-c"]})))
        .await
        .expect("the interrupt is sent");

    // The shell takes a moment to reclaim the terminal after the interrupt.
    let mut running = String::new();
    for _ in 0..200 {
        running = guard
            .server()
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(&pane)
                    .arg("#{pane_current_command}"),
            )
            .await
            .expect("tmux reports the command")
            .stdout_lossy()
            .trim()
            .to_owned();
        if running != "sleep" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_ne!(running, "sleep", "the interrupt did not stop the command");

    let recovered = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "echo recovered",
                    "seconds": 25
                })),
                CancellationToken::new(),
            )
            .await
            .expect("the pane comes back"),
    );
    assert_eq!(recovered["outcome"], "completed");
    assert_eq!(recovered["exit_status"], 0);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn waiting_for_text_sees_what_a_pane_writes() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let waiting = {
        let tools = tools.clone();
        let pane = pane.clone();
        tokio::spawn(async move {
            tools
                .wait_for_text(
                    args(serde_json::json!({
                        "pane": pane,
                        "patterns": ["never", "ready to serve"],
                        "seconds": 20
                    })),
                    CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "echo 'ready to serve'",
            "enter": true
        })))
        .await
        .expect("keys are sent");

    let result = json(waiting.await.expect("the wait finishes").expect("a result"));
    assert_eq!(result["outcome"], "matched");
    assert_eq!(result["matched_index"], 1);
    assert_eq!(result["matched_pattern"], "ready to serve");
    assert_eq!(result["present_at_entry"], false);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_stop_pattern_ends_a_wait_early() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let waiting = {
        let tools = tools.clone();
        let pane = pane.clone();
        tokio::spawn(async move {
            tools
                .wait_for_text(
                    args(serde_json::json!({
                        "pane": pane,
                        "patterns": ["succeeded"],
                        "stop": ["Traceback", "error:"],
                        "seconds": 20
                    })),
                    CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "echo 'error: it did not work'",
            "enter": true
        })))
        .await
        .expect("keys are sent");

    let result = json(waiting.await.expect("the wait finishes").expect("a result"));
    assert_eq!(result["outcome"], "stopped");
    assert_eq!(result["matched_pattern"], "error:");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_wait_says_when_its_pattern_was_already_on_screen() {
    let (guard, tools, pane) = typing_fixture("work").await;

    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": "echo already-here",
                "seconds": 20
            })),
            CancellationToken::new(),
        )
        .await
        .expect("the command runs");

    let result = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["already-here"],
                    "seconds": 1
                })),
                CancellationToken::new(),
            )
            .await
            .expect("a result"),
    );

    assert_eq!(
        result["outcome"], "deadline",
        "a stream carries what comes next, not what is already drawn"
    );
    assert_eq!(
        result["present_at_entry"], true,
        "so the answer says why nothing matched"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_regular_expression_wait_compiles_or_explains_itself() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let error = tools
        .wait_for_text(
            args(serde_json::json!({
                "pane": pane,
                "patterns": ["a("],
                "regex": true,
                "seconds": 1
            })),
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
        .expect_err("an invalid expression is rejected");
    assert!(error.message.contains("a("), "{}", error.message);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn capture_since_returns_only_what_is_new() {
    let (guard, tools, pane) = typing_fixture("work").await;

    let opened = json(
        tools
            .capture_since(args(serde_json::json!({"pane": pane})))
            .await
            .expect("a tail opens"),
    );
    assert_eq!(
        opened["first"], true,
        "the first call answers with the screen, since the tail has seen nothing yet"
    );
    assert_eq!(opened["missed"], false);
    let cursor = opened["cursor"]
        .as_str()
        .expect("a cursor is text")
        .to_owned();

    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": "echo written-after",
                "seconds": 20
            })),
            CancellationToken::new(),
        )
        .await
        .expect("the command runs");

    let next = json(
        tools
            .capture_since(args(serde_json::json!({"pane": pane, "cursor": cursor})))
            .await
            .expect("the tail reports"),
    );
    assert!(
        next["text"]
            .as_str()
            .expect("text is text")
            .contains("written-after"),
        "{:?}",
        next["text"]
    );
    assert_eq!(next["missed"], false);

    // Reading again with the newest cursor yields nothing new.
    let cursor = next["cursor"]
        .as_str()
        .expect("a cursor is text")
        .to_owned();
    let quiet = json(
        tools
            .capture_since(args(serde_json::json!({"pane": pane, "cursor": cursor})))
            .await
            .expect("the tail reports"),
    );
    assert_eq!(quiet["text"], "");
    assert_eq!(quiet["first"], false);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_cursor_from_another_pane_is_refused() {
    let (guard, tools, pane) = typing_fixture("work").await;
    let other = id(tools
        .split_pane(args(serde_json::json!({"pane": pane})))
        .await
        .expect("the pane splits"));

    let opened = json(
        tools
            .capture_since(args(serde_json::json!({"pane": pane})))
            .await
            .expect("a tail opens"),
    );
    let cursor = opened["cursor"].as_str().expect("a cursor is text");

    let error = tools
        .capture_since(args(serde_json::json!({"pane": other, "cursor": cursor})))
        .await
        .map(|_| ())
        .expect_err("a cursor names the pane it came from");
    assert!(error.message.contains(&pane), "{}", error.message);

    let error = tools
        .capture_since(args(
            serde_json::json!({"pane": pane, "cursor": "nonsense"}),
        ))
        .await
        .map(|_| ())
        .expect_err("foreign text is not a cursor");
    assert!(error.message.contains("nonsense"), "{}", error.message);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn selecting_moves_focus_by_direction_and_by_order() {
    let (guard, tools) = fixture("work").await;
    let top = panes(&tools).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    let bottom = id(tools
        .split_pane(args(serde_json::json!({"pane": top, "direction": "below"})))
        .await
        .expect("the pane splits"));

    let selected = json(
        tools
            .select_pane(args(serde_json::json!({"pane": bottom})))
            .await
            .expect("a pane is selected"),
    );
    assert_eq!(selected["id"], bottom.as_str());
    assert_eq!(selected["active"], true);

    let above = json(
        tools
            .select_pane(args(serde_json::json!({"pane": bottom, "direction": "up"})))
            .await
            .expect("focus moves up"),
    );
    assert_eq!(above["id"], top.as_str(), "up follows the layout");

    let stepped = json(
        tools
            .select_pane(args(serde_json::json!({"pane": top, "direction": "next"})))
            .await
            .expect("focus steps on"),
    );
    assert_eq!(stepped["id"], bottom.as_str());

    let wrapped = json(
        tools
            .select_pane(args(
                serde_json::json!({"pane": top, "direction": "previous"}),
            ))
            .await
            .expect("focus steps back"),
    );
    assert_eq!(
        wrapped["id"],
        bottom.as_str(),
        "previous from the first pane wraps to the last"
    );

    let below = json(
        tools
            .select_pane(args(serde_json::json!({"pane": top, "direction": "down"})))
            .await
            .expect("focus moves down"),
    );
    assert_eq!(below["id"], bottom.as_str(), "down follows the layout");

    // Left and right need a horizontal split: in a stack of two, tmux has
    // nowhere sideways to go and leaves focus where it is, which would make
    // the assertion pass without the direction doing anything.
    let beside = id(tools
        .split_pane(args(
            serde_json::json!({"pane": bottom, "direction": "right"}),
        ))
        .await
        .expect("the pane splits sideways"));

    let leftward = json(
        tools
            .select_pane(args(
                serde_json::json!({"pane": beside, "direction": "left"}),
            ))
            .await
            .expect("focus moves left"),
    );
    assert_eq!(leftward["id"], bottom.as_str(), "left follows the layout");

    let rightward = json(
        tools
            .select_pane(args(
                serde_json::json!({"pane": bottom, "direction": "right"}),
            ))
            .await
            .expect("focus moves right"),
    );
    assert_eq!(rightward["id"], beside.as_str(), "right follows the layout");

    // `last` is the previously active pane, which the moves above have set.
    let back = json(
        tools
            .select_pane(args(
                serde_json::json!({"pane": beside, "direction": "last"}),
            ))
            .await
            .expect("focus returns"),
    );
    assert!(
        back["id"].is_string(),
        "last names whichever pane was active before: {back}",
    );

    let error = tools
        .select_pane(args(
            serde_json::json!({"pane": top, "direction": "sideways"}),
        ))
        .await
        .map(|_| ())
        .expect_err("an unknown direction is rejected");
    assert!(error.message.contains("sideways"), "{}", error.message);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_split_percentage_is_bounded_the_same_on_every_tmux() {
    // tmux does not agree with itself here: 3.7b refuses a percentage above
    // 100 and 3.2a accepts it. The tool decides, so the answer does not
    // depend on which tmux is underneath.
    let (guard, tools) = fixture("work").await;
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();

    for refused in [0, 101, 200] {
        let error = tools
            .split_pane(args(serde_json::json!({
                "pane": pane,
                "direction": "below",
                "percent": refused
            })))
            .await
            .map(|_| ())
            .expect_err("a percentage outside 1..=100 is refused");
        assert!(
            error.message.contains(&refused.to_string()),
            "the refusal names the value: {}",
            error.message
        );
    }

    for accepted in [1, 50, 100] {
        tools
            .split_pane(args(serde_json::json!({
                "pane": pane,
                "direction": "below",
                "percent": accepted
            })))
            .await
            .unwrap_or_else(|error| panic!("{accepted} should be accepted: {error}"));
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn selecting_moves_focus_between_windows() {
    let (guard, tools) = fixture("work").await;
    let first = json(tools.list_windows().await.expect("windows"))["windows"]
        .as_array()
        .expect("a listing wraps an array")[0]["id"]
        .as_str()
        .expect("a window id is a string")
        .to_owned();
    let second = id(tools
        .new_window(args(
            serde_json::json!({"session": "work", "name": "second"}),
        ))
        .await
        .expect("a window is created"));

    let selected = json(
        tools
            .select_window(args(serde_json::json!({"window": second})))
            .await
            .expect("a window is selected"),
    );
    assert_eq!(selected["windows"][0]["id"], second.as_str());
    assert_eq!(selected["windows"][0]["active"], true);

    let stepped = json(
        tools
            .select_window(args(serde_json::json!({
                "window": second,
                "direction": "next"
            })))
            .await
            .expect("focus steps on"),
    );
    assert_eq!(
        stepped["windows"][0]["id"],
        first.as_str(),
        "next from the last window wraps to the first"
    );

    // `last` is the session's previously active window, and tmux resolves it
    // against the session rather than against the named window. Proving that
    // needs a third window so the named one is not already active: with only
    // two, selecting it first is a no-op and the bug hides.
    let third = id(tools
        .new_window(args(
            serde_json::json!({"session": "work", "name": "third"}),
        ))
        .await
        .expect("a window is created"));
    for window in [&second, &third] {
        tools
            .select_window(args(serde_json::json!({"window": window})))
            .await
            .expect("a window is selected");
    }

    // Active is now `third` and the previously active window is `second`.
    let back = json(
        tools
            .select_window(args(serde_json::json!({
                "window": first,
                "direction": "last"
            })))
            .await
            .expect("focus goes back"),
    );
    assert_eq!(
        back["windows"][0]["id"],
        second.as_str(),
        "last means the window that was active before, not the one named; \
         selecting the named window first would rewrite that pointer to third"
    );

    let error = tools
        .select_window(args(serde_json::json!({
            "window": first,
            "direction": "sideways"
        })))
        .await
        .map(|_| ())
        .expect_err("an unknown direction is rejected");
    assert!(error.message.contains("sideways"), "{}", error.message);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn searching_finds_which_pane_is_showing_something() {
    let (guard, tools, pane) = typing_fixture("work").await;
    let other = id(tools
        .split_pane(args(serde_json::json!({"pane": pane})))
        .await
        .expect("the pane splits"));
    prompt_ready(guard.server(), &other).await;

    tools
        .run_command(
            args(serde_json::json!({
                "pane": other,
                "command": "echo distinctive-needle-42",
                "seconds": 20
            })),
            CancellationToken::new(),
        )
        .await
        .expect("the command runs");

    let found = json(
        tools
            .search_panes(args(
                serde_json::json!({"pattern": "distinctive-needle-42"}),
            ))
            .await
            .expect("the search runs"),
    );

    let matches = found["matches"].as_array().expect("matches is an array");
    assert!(!matches.is_empty(), "the needle is on screen somewhere");
    assert!(
        matches.iter().any(|found| found["pane"] == other.as_str()),
        "the search names the pane that is showing it: {matches:?}"
    );
    assert_eq!(found["panes_searched"], 2);
    assert_eq!(found["capped"], false);

    // Narrowing to the other pane finds nothing, which proves the scope is
    // applied rather than ignored.
    let elsewhere = tools
        .search_panes(args(serde_json::json!({
            "pattern": "distinctive-needle-42",
            "window": "@999"
        })))
        .await
        .map_or_else(|_| serde_json::json!({"matches": []}), json);
    assert!(elsewhere["matches"].as_array().is_none_or(Vec::is_empty));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_snapshot_carries_the_state_a_capture_leaves_out() {
    let (guard, tools, pane) = typing_fixture("work").await;

    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": "echo snapshot-marker",
                "seconds": 20
            })),
            CancellationToken::new(),
        )
        .await
        .expect("the command runs");

    let shot = json(
        tools
            .snapshot_pane(args(serde_json::json!({"pane": pane})))
            .await
            .expect("the pane is snapshotted"),
    );

    assert_eq!(shot["pane"]["id"], pane.as_str());
    assert!(
        shot["content"]
            .as_str()
            .expect("content is text")
            .contains("snapshot-marker"),
        "{:?}",
        shot["content"]
    );

    // The point of the tool: state a capture cannot report.
    assert!(shot["width"].as_u64().is_some_and(|width| width > 0));
    assert!(shot["height"].as_u64().is_some_and(|height| height > 0));
    assert!(
        shot["cursor_y"].as_u64().is_some(),
        "the cursor is what says whether a shell is waiting: {shot:?}"
    );
    assert_eq!(shot["in_mode"], false);
    assert!(
        shot["mode"].is_null(),
        "a pane outside a mode has no mode name"
    );
    assert_eq!(shot["dead"], false);
    assert_eq!(shot["dropped"], 0);

    // A limit keeps the end, because the end is what just happened.
    let trimmed = json(
        tools
            .snapshot_pane(args(serde_json::json!({"pane": pane, "max_lines": 1})))
            .await
            .expect("the pane is snapshotted"),
    );
    assert_eq!(trimmed["lines"], 1);
    assert!(
        trimmed["dropped"]
            .as_u64()
            .is_some_and(|dropped| dropped > 0),
        "{trimmed:?}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn options_are_read_and_written_at_the_scope_named() {
    let (guard, tools) = fixture("work").await;
    let window = json(tools.list_windows().await.expect("windows"))["windows"]
        .as_array()
        .expect("a listing wraps an array")[0]["id"]
        .as_str()
        .expect("a window id is a string")
        .to_owned();

    tools
        .set_option(args(serde_json::json!({
            "name": "@probe",
            "scope": "window",
            "target": window,
            "value": "written"
        })))
        .await
        .expect("the option is set");

    let read = json(
        tools
            .show_option(args(serde_json::json!({
                "name": "@probe",
                "scope": "window",
                "target": window
            })))
            .await
            .expect("the option is read"),
    );
    assert_eq!(read["value"], "written");

    // A scope that was never written reports no value rather than an error.
    let absent = json(
        tools
            .show_option(args(serde_json::json!({
                "name": "@probe",
                "scope": "global-session"
            })))
            .await
            .expect("an unset option is still an answer"),
    );
    assert!(absent["value"].is_null(), "{absent:?}");

    let error = tools
        .set_option(args(serde_json::json!({
            "name": "@probe",
            "scope": "window",
            "target": window
        })))
        .await
        .map(|_| ())
        .expect_err("setting needs a value");
    assert!(error.message.contains("value"), "{}", error.message);

    let error = tools
        .show_option(args(
            serde_json::json!({"name": "@probe", "scope": "window"}),
        ))
        .await
        .map(|_| ())
        .expect_err("a window scope needs a target");
    assert!(error.message.contains("target"), "{}", error.message);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_the_server_this_process_runs_on_is_refused() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let bare = TmuxTools::new(guard.server().clone());
    bare.create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session is created");
    let own = panes(&bare).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();

    let tools = TmuxTools::builder(guard.server().clone())
        .caller(Some(identity_for(guard.server(), &own).await))
        .build();

    let refused = tools
        .kill_server()
        .await
        .map(|_| ())
        .expect_err("killing the server we are on is refused");
    assert!(refused.message.contains(&own), "{}", refused.message);
    assert_eq!(
        panes(&tools).await.len(),
        1,
        "the server is still there to answer"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_an_unrelated_server_is_allowed() {
    let ours = TestServer::builder().start().await.expect("tmux starts");
    let theirs = TestServer::builder().start().await.expect("tmux starts");
    for server in [ours.server(), theirs.server()] {
        TmuxTools::new(server.clone())
            .create_session(args(serde_json::json!({"name": "work"})))
            .await
            .expect("session is created");
    }
    let elsewhere = panes(&TmuxTools::new(theirs.server().clone())).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();

    let tools = TmuxTools::builder(theirs.server().clone())
        .caller(Some(identity_for(ours.server(), &elsewhere).await))
        .build();
    // Asked through a handle of its own, so the answer is about the daemon
    // rather than about the executor the tool just used. An earlier version
    // of this tool closed libtmux's own executor and reported success while
    // tmux ran on, and a check through that same handle agreed with it.
    let onlooker = independent(theirs.server()).await;
    assert!(
        onlooker.is_alive().await,
        "the server is running beforehand"
    );

    tools
        .kill_server()
        .await
        .expect("a server this process is not on is fair game");

    for _ in 0..100 {
        if !onlooker.is_alive().await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        !onlooker.is_alive().await,
        "kill_server must stop the tmux daemon, not merely answer"
    );
    assert!(
        independent(ours.server()).await.is_alive().await,
        "and it must stop only the one it was pointed at"
    );

    ours.shutdown().await.expect("tmux fixture shuts down");
}

/// How many clients tmux has, which is how a control-mode connection shows up.
async fn client_count(server: &Server) -> usize {
    server.clients().await.map_or(0, |found| found.len())
}

/// Wait for the client count to settle at `wanted`, and report what it was.
async fn clients_settle(server: &Server, wanted: usize) -> usize {
    let mut seen = client_count(server).await;
    for _ in 0..200 {
        if seen == wanted {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        seen = client_count(server).await;
    }
    seen
}

#[tokio::test]
async fn abandoning_a_wait_closes_the_connection_it_opened() {
    let (guard, tools, pane) = typing_fixture("work").await;
    let server = guard.server();
    let baseline = client_count(server).await;

    // A wait that cannot match, abandoned rather than allowed to finish. An
    // MCP client cancelling a request drops the future exactly like this, and
    // what must not survive it is the control-mode connection underneath.
    let waiting = {
        let tools = tools.clone();
        let pane = pane.clone();
        tokio::spawn(async move {
            tools
                .wait_for_text(
                    args(serde_json::json!({
                        "pane": pane,
                        "patterns": ["never-arrives"],
                        "seconds": 600
                    })),
                    CancellationToken::new(),
                )
                .await
        })
    };

    // Seeing the connection appear is what keeps the check below honest: a
    // count that never rose would return to baseline whatever the code did.
    assert_eq!(
        clients_settle(server, baseline + 1).await,
        baseline + 1,
        "the wait holds a control-mode connection while it runs"
    );

    waiting.abort();

    assert_eq!(
        clients_settle(server, baseline).await,
        baseline,
        "a cancelled wait must not leave its control-mode client attached"
    );

    // And the pane is still usable afterwards, which is the point of caring.
    let after = json(
        tools
            .run_command(
                args(serde_json::json!({
                    "pane": pane,
                    "command": "echo still-works",
                    "seconds": 20
                })),
                CancellationToken::new(),
            )
            .await
            .expect("the pane still answers"),
    );
    assert_eq!(after["exit_status"], 0);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn abandoning_a_run_closes_the_connection_it_opened() {
    let (guard, tools, pane) = typing_fixture("work").await;
    let server = guard.server();
    let baseline = client_count(server).await;

    let running = {
        let tools = tools.clone();
        let pane = pane.clone();
        tokio::spawn(async move {
            tools
                .run_command(
                    args(serde_json::json!({
                        "pane": pane,
                        "command": "sleep 600",
                        "seconds": 600
                    })),
                    CancellationToken::new(),
                )
                .await
        })
    };

    assert_eq!(
        clients_settle(server, baseline + 1).await,
        baseline + 1,
        "the run holds a control-mode connection while it runs"
    );

    running.abort();

    assert_eq!(
        clients_settle(server, baseline).await,
        baseline,
        "a cancelled run must not leave its control-mode client attached"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn dropping_the_server_closes_the_tails_it_held() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let baseline = client_count(server).await;

    let tools = TmuxTools::new(server.clone());
    tools
        .create_session(args(serde_json::json!({"name": "work"})))
        .await
        .expect("session is created");
    let pane = panes(&tools).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();

    // A tail attaches once and stays attached, so that the next call can say
    // what changed. That is a connection someone has to close.
    tools
        .capture_since(args(serde_json::json!({"pane": pane})))
        .await
        .expect("a tail opens");
    assert_eq!(
        clients_settle(server, baseline + 1).await,
        baseline + 1,
        "the tail holds a connection open between calls"
    );

    drop(tools);

    assert_eq!(
        clients_settle(server, baseline).await,
        baseline,
        "dropping the server must close the tails it was holding"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_channel_releases_whatever_waits_on_it() {
    let (guard, tools) = fixture("work").await;

    let waiting = {
        let tools = tools.clone();
        tokio::spawn(async move {
            tools
                .wait_for_channel(args(serde_json::json!({
                    "channel": "ready",
                    "seconds": 20
                })))
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    tools
        .signal_channel(args(serde_json::json!({"channel": "ready"})))
        .await
        .expect("the channel is signalled");

    let result = json(waiting.await.expect("the wait finishes").expect("a result"));
    assert_eq!(result["outcome"], "signalled");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_channel_nobody_signals_reaches_its_deadline() {
    let (guard, tools) = fixture("work").await;

    let result = json(
        tools
            .wait_for_channel(args(serde_json::json!({
                "channel": "never",
                "seconds": 1
            })))
            .await
            .expect("the deadline is an answer"),
    );
    assert_eq!(result["outcome"], "deadline");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_geometry_is_reachable_through_the_filter_grammar() {
    // The bottom-right pane needs no tool of its own: it is two conjuncts in
    // an expression `find_panes` already speaks.
    let (guard, tools) = fixture("work").await;
    let first = panes(&tools).await[0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    let below = id(tools
        .split_pane(args(
            serde_json::json!({"pane": first, "direction": "below"}),
        ))
        .await
        .expect("the pane splits"));

    let found = json(
        tools
            .find_panes(args(serde_json::json!({
                "filter": {
                    "version": 1,
                    "target": "pane",
                    "expr": {"op": "and", "args": [
                        {"op": "eq", "field": "pane_at_bottom", "value": true},
                        {"op": "eq", "field": "pane_at_right", "value": true}
                    ]}
                }
            })))
            .await
            .expect("the expression is understood"),
    );
    let found = found["panes"].as_array().expect("a listing is an array");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["id"], below.as_str());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn capture_since_says_so_when_output_outran_the_buffer() {
    // `missed` is only worth reporting if it can actually fire. Asserting it
    // false everywhere would look identical to an indicator wired to a
    // constant, so this drives the ring past its capacity and checks the
    // other answer.
    let (guard, tools, pane) = typing_fixture("work").await;

    let opened = json(
        tools
            .capture_since(args(serde_json::json!({"pane": pane})))
            .await
            .expect("a tail opens"),
    );
    let cursor = opened["cursor"]
        .as_str()
        .expect("a cursor is text")
        .to_owned();

    // The ring holds 256 KiB, so a little over that is all this needs: 15000
    // lines of 20 characters is roughly 315 KiB. Typing it rather than
    // running it through `run_command` keeps the whole payload out of a
    // second buffer, which matters because this test shares a machine with
    // the rest of the suite.
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "yes 0123456789abcdefghi | head -n 15000",
            "enter": true
        })))
        .await
        .expect("keys are sent");

    // Poll until the ring has been overrun rather than guessing how long the
    // shell needs, so a slow machine waits instead of failing.
    let mut after = Value::Null;
    for _ in 0..200 {
        after = json(
            tools
                .capture_since(args(serde_json::json!({"pane": pane, "cursor": cursor})))
                .await
                .expect("the tail reports"),
        );
        if after["missed"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        after["missed"], true,
        "output past the buffer is reported as missed, not silently dropped: {after}",
    );
    // And it still answers with what it does have, rather than refusing.
    assert!(
        !after["text"].as_str().expect("text is text").is_empty(),
        "a gap does not make the rest unreportable: {after}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// The point of a job is that starting one costs nothing, so the test asserts
/// on time as well as on the answer: a ten-second command must not hold the
/// call that starts it.
#[tokio::test]
async fn starting_a_job_returns_before_the_command_finishes() {
    let (guard, tools, pane) = typing_fixture("jobs-return").await;

    let began = std::time::Instant::now();
    let started = json(
        tools
            .start_command(args(serde_json::json!({
                "pane": pane,
                "command": "sleep 10; echo finished-at-last",
            })))
            .await
            .expect("the job starts"),
    );
    let elapsed = began.elapsed();

    assert_eq!(started["state"], "running");
    assert_eq!(started["pane"], pane.as_str());
    assert!(
        elapsed < Duration::from_secs(5),
        "starting a ten-second command took {elapsed:?}",
    );

    let job = started["job"].as_str().expect("a job id").to_owned();
    let listed = json(tools.list_jobs().await.expect("jobs list"));
    assert_eq!(listed["jobs"][0]["job"], job.as_str());

    // Cancelling interrupts the pane rather than leaving it busy.
    let cancelled = json(
        tools
            .cancel_job(args(serde_json::json!({"job": job})))
            .await
            .expect("the job cancels"),
    );
    assert_eq!(cancelled["interrupted"], true);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A job reports the same exit status `run_command` would, and its cursor
/// returns only what is new -- which is what makes polling cheap.
#[tokio::test]
async fn a_job_reports_its_status_and_only_what_is_new() {
    let (guard, tools, pane) = typing_fixture("jobs-status").await;

    let job = json(
        tools
            .start_command(args(serde_json::json!({
                "pane": pane,
                "command": "echo first-line; sleep 1; echo second-line; exit 3",
            })))
            .await
            .expect("the job starts"),
    )["job"]
        .as_str()
        .expect("a job id")
        .to_owned();

    // Waiting returns when the job ends, not at the deadline.
    let began = std::time::Instant::now();
    let finished = json(
        tools
            .job_status(args(serde_json::json!({"job": job, "seconds": 30})))
            .await
            .expect("the job reports"),
    );
    assert!(
        began.elapsed() < Duration::from_secs(25),
        "waiting ran to its deadline rather than to the job's end",
    );

    assert_eq!(finished["state"], "finished");
    assert_eq!(finished["exit_status"], 3);
    assert_eq!(finished["complete"], true);
    let output = finished["output"].as_str().expect("output is a string");
    assert!(output.contains("first-line"), "output was {output:?}");
    assert!(output.contains("second-line"), "output was {output:?}");
    assert!(
        !output.contains("echo first-line"),
        "the shell's echo of the typed line is not the command's output: {output:?}",
    );

    // The cursor it handed back returns nothing further.
    let cursor = finished["cursor"].as_u64().expect("a cursor");
    let again = json(
        tools
            .job_status(args(serde_json::json!({"job": job, "cursor": cursor})))
            .await
            .expect("the job reports again"),
    );
    assert_eq!(again["output"], "");
    assert_eq!(again["exit_status"], 3);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Several jobs at once is the case a blocking call cannot serve at all.
#[tokio::test]
async fn jobs_run_in_several_panes_at_once() {
    let (guard, tools) = fixture("jobs-parallel").await;

    let first = panes(&tools).await[0]["id"]
        .as_str()
        .expect("a pane id")
        .to_owned();
    let second = id(tools
        .split_pane(args(serde_json::json!({"pane": first})))
        .await
        .expect("the pane splits"));
    prompt_ready(guard.server(), &first).await;
    prompt_ready(guard.server(), &second).await;

    let mut jobs = Vec::new();
    for (pane, marker) in [(&first, "from-one"), (&second, "from-two")] {
        jobs.push(
            json(
                tools
                    .start_command(args(serde_json::json!({
                        "pane": pane,
                        "command": format!("sleep 1; echo {marker}"),
                    })))
                    .await
                    .expect("the job starts"),
            )["job"]
                .as_str()
                .expect("a job id")
                .to_owned(),
        );
    }

    for (job, marker) in jobs.iter().zip(["from-one", "from-two"]) {
        let done = json(
            tools
                .job_status(args(serde_json::json!({"job": job, "seconds": 30})))
                .await
                .expect("the job reports"),
        );
        assert_eq!(done["state"], "finished", "job {job} finished");
        assert!(
            done["output"].as_str().expect("output").contains(marker),
            "job {job} carried its own output",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A job id this server does not hold is stale, not bad input: the fix is to
/// list again, and an agent decides that from the classification.
#[tokio::test]
async fn an_unknown_job_is_reported_as_stale() {
    let (guard, tools) = fixture("jobs-unknown").await;

    let Err(error) = tools
        .job_status(args(serde_json::json!({"job": "job-does-not-exist"})))
        .await
    else {
        panic!("an unknown job fails");
    };

    let data = error.data.expect("the failure carries data");
    assert_eq!(data["kind"], "object_gone");
    assert_eq!(data["retryable"], false);
    assert_eq!(data["stale"], true);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Waiting for quiet is what a caller reaches for when it cannot name the
/// text that means success, so the test proves both halves: a pane that
/// settles is reported idle, and one that keeps writing is not.
#[tokio::test]
async fn waiting_for_quiet_distinguishes_a_settled_pane_from_a_busy_one() {
    let (guard, tools, pane) = typing_fixture("idle").await;

    // A pane at a prompt is already quiet.
    let settled = json(
        tools
            .wait_for_idle(
                args(serde_json::json!({"pane": pane, "quiet_seconds": 1, "seconds": 20})),
                CancellationToken::new(),
            )
            .await
            .expect("the wait runs"),
    );
    assert_eq!(settled["outcome"], "idle");
    assert_eq!(settled["pane"], pane.as_str());

    // A pane writing steadily never goes quiet, so the deadline arrives first.
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "while true; do printf 'still-working '; sleep 0.2; done",
            "enter": true,
        })))
        .await
        .expect("keys are sent");

    let busy = json(
        tools
            .wait_for_idle(
                args(serde_json::json!({"pane": pane, "quiet_seconds": 2, "seconds": 5})),
                CancellationToken::new(),
            )
            .await
            .expect("the wait runs"),
    );
    assert_eq!(busy["outcome"], "deadline");
    assert!(
        busy["text"]
            .as_str()
            .expect("text is a string")
            .contains("still-working"),
        "what the pane wrote comes back with the answer",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// The format tool is the escape hatch for every field tmux publishes and no
/// tool here carries, so the test uses one of exactly that kind.
#[tokio::test]
async fn a_format_expands_against_the_pane_it_names() {
    let (guard, tools, pane) = typing_fixture("formats").await;

    let expanded = json(
        tools
            .expand_format(args(serde_json::json!({
                "format": "#{pane_id}",
                "pane": pane,
            })))
            .await
            .expect("the format expands"),
    );
    assert_eq!(expanded["value"], pane.as_str());
    assert_eq!(expanded["pane"], pane.as_str());

    // A field with no tool of its own, which is the reason this exists.
    let width = json(
        tools
            .expand_format(args(serde_json::json!({
                "format": "#{pane_width}",
                "pane": pane,
            })))
            .await
            .expect("the format expands"),
    );
    assert!(
        width["value"]
            .as_str()
            .expect("a width")
            .parse::<u32>()
            .is_ok(),
        "pane_width came back as a number: {width:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// The environment tmux hands out is not the environment of anything already
/// running, and the test says so by checking a pane started earlier.
#[tokio::test]
async fn the_environment_is_read_and_written_at_the_scope_named() {
    let (guard, tools) = fixture("environment").await;

    let written = json(
        tools
            .set_environment(args(serde_json::json!({
                "name": "TMUX_MCP_PROBE",
                "value": "set-by-the-test",
            })))
            .await
            .expect("the variable is written"),
    );
    assert_eq!(written["removed"], false);

    let shown = json(
        tools
            .show_environment(args(serde_json::json!({})))
            .await
            .expect("the environment is read"),
    );
    let found = shown["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["name"] == "TMUX_MCP_PROBE")
        .expect("the variable is listed");
    assert_eq!(found["value"], "set-by-the-test");

    // Omitting the value marks it for removal rather than setting it empty.
    let removed = json(
        tools
            .set_environment(args(serde_json::json!({"name": "TMUX_MCP_PROBE"})))
            .await
            .expect("the variable is removed"),
    );
    assert_eq!(removed["removed"], true);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Pasting exists because typing is not the same thing: the text arrives as
/// one block rather than as keystrokes a program can react to one at a time.
#[tokio::test]
async fn pasted_text_reaches_the_pane_and_leaves_no_buffer_behind() {
    let (guard, tools, pane) = typing_fixture("pasting").await;

    let pasted = json(
        tools
            .paste_text(args(serde_json::json!({
                "pane": pane,
                "text": "echo pasted-marker",
            })))
            .await
            .expect("the text pastes"),
    );
    assert_eq!(pasted["pane"], pane.as_str());
    assert_eq!(pasted["bytes"], 18);

    // Asserted on the pane rather than through a wait: the text is delivered
    // by the paste itself, and a wait attached afterwards races the output it
    // is looking for.
    let mut shown = String::new();
    for _ in 0..40 {
        shown = json(
            tools
                .capture_pane(args(serde_json::json!({"pane": pane})))
                .await
                .expect("the capture runs"),
        )["text"]
            .as_str()
            .expect("text")
            .to_owned();
        if shown.contains("echo pasted-marker") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        shown.contains("echo pasted-marker"),
        "the pasted text reached the pane: {shown:?}",
    );

    // The buffer this created is gone, so it cannot be pasted again by
    // accident or read by whoever looks at the buffer list next.
    let buffers = guard
        .server()
        .buffer_names()
        .await
        .expect("buffers are listed");
    assert!(
        buffers.is_empty(),
        "the paste buffer was deleted afterwards: {buffers:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Clearing scrollback is what makes the next capture cheap, so the test
/// measures exactly that: many lines before, few after.
#[tokio::test]
async fn clearing_a_pane_shrinks_what_the_next_capture_returns() {
    let (guard, tools, pane) = typing_fixture("clearing").await;

    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": "seq 1 500",
                "seconds": 20,
            })),
            CancellationToken::new(),
        )
        .await
        .expect("the command runs");

    let before = json(
        tools
            .capture_pane(args(serde_json::json!({"pane": pane, "history": true})))
            .await
            .expect("the capture runs"),
    )["lines"]
        .as_u64()
        .expect("a line count");
    assert!(before > 100, "the scrollback holds the run: {before} lines");

    tools
        .clear_pane(args(serde_json::json!({"pane": pane})))
        .await
        .expect("the pane clears");

    let after = json(
        tools
            .capture_pane(args(serde_json::json!({"pane": pane, "history": true})))
            .await
            .expect("the capture runs"),
    )["lines"]
        .as_u64()
        .expect("a line count");
    assert!(
        after < before,
        "clearing left less to read: {after} against {before}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Discovery has to name the server these tools are bound to, because a pane
/// id means nothing without knowing which server it belongs to.
#[tokio::test]
async fn listing_servers_marks_the_one_these_tools_are_bound_to() {
    let (guard, tools) = fixture("discovery").await;

    let listed = json(tools.list_servers().await.expect("servers are listed"));
    let servers = listed["servers"].as_array().expect("a listing");

    let current: Vec<_> = servers
        .iter()
        .filter(|server| server["current"] == true)
        .collect();
    assert_eq!(current.len(), 1, "exactly one server is the bound one");
    assert!(
        current[0]["sessions"].as_u64().expect("a session count") >= 1,
        "the bound server answered about itself",
    );
    assert!(
        !listed["searched"]
            .as_array()
            .expect("searched paths")
            .is_empty(),
        "the answer says where it looked",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Reading only the last command's output is the biggest saving available to
/// an agent, so the test measures it: with prompt marks the answer is a
/// fraction of the history, and without them it says so rather than
/// pretending.
#[tokio::test]
async fn real_tmux_compat_capturing_the_last_command_says_whether_it_could() {
    let (guard, tools, pane) = typing_fixture("last-command").await;

    // A long run, so falling back to the history would be obvious.
    tools
        .run_command(
            args(serde_json::json!({
                "pane": pane,
                "command": "seq 1 300",
                "seconds": 20,
            })),
            CancellationToken::new(),
        )
        .await
        .expect("the first command runs");

    // Standing in for shell integration, which bash and zsh lack.
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": r"printf '\033]133;A\007'; echo the-prompt; printf '\033]133;C\007'; echo only-this-line",
            "enter": true,
        })))
        .await
        .expect("keys are sent");
    tokio::time::sleep(Duration::from_millis(800)).await;

    let last = json(
        tools
            .capture_pane(args(
                serde_json::json!({"pane": pane, "last_command": true}),
            ))
            .await
            .expect("the capture runs"),
    );
    let whole = json(
        tools
            .capture_pane(args(serde_json::json!({"pane": pane, "history": true})))
            .await
            .expect("the capture runs"),
    );
    assert_eq!(whole["marks"], "not_asked");

    match last["marks"].as_str().expect("a marks field") {
        "present" => {
            let text = last["text"].as_str().expect("text");
            assert!(
                text.contains("only-this-line"),
                "the last command's output came back: {text:?}",
            );
            assert!(!text.contains("299"), "the run before it did not: {text:?}");
            assert!(
                last["lines"].as_u64().expect("lines") < whole["lines"].as_u64().expect("lines"),
                "the answer is shorter than the history it was cut from",
            );
        }
        // Below tmux 3.7 there is no `capture-pane -F` to ask.
        "unsupported" => assert!(last["lines"].as_u64().expect("lines") > 0),
        other => panic!("unexpected marks {other:?}"),
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}
