//! Scenario tests for the echo contract: `wait_for_text` versus the pane's
//! echo of text this MCP server itself typed with `send_keys`.
//!
//! Scenarios S1-S6 below are named to match the contract they were drafted
//! against. Every "matches real output" assertion pairs a floor on elapsed
//! time with the outcome: a match that lands before the pane's own `sleep 1`
//! can finish is a match on the echo, not on what the command produced.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::{Duration, Instant};

use libtmux::test::TestServer;
use tokio_util::sync::CancellationToken;

mod support;

use support::{args, bare_tools, json, prompt_ready};

/// S1: a short, unsubmitted answer must never mask later real output that
/// contains it as a substring, and must never mask a whole row.
///
/// `y` is typed and left pending (no Enter) while a command already
/// submitted earlier is still running in the background; when it finishes,
/// its output ends in the same letter the pending answer is. A wait for
/// `ready` must still see it.
#[tokio::test]
async fn s1_a_pending_answer_never_hides_real_output_containing_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "s1"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    // A real, independent producer of "ready" -- backgrounded so the prompt
    // returns immediately and the pending keystroke below lands on a fresh
    // line, not mid-dispatch of this one.
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "(sleep 1; echo ready) &",
            "enter": true
        })))
        .await
        .expect("the background job starts");

    // A short pending answer that is also the last letter of the word this
    // waits for: proof against masking by "contains this substring"
    // instead of by the submitted line it actually is.
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "y",
            "enter": false
        })))
        .await
        .expect("the pending keystroke is typed");

    let started = Instant::now();
    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["ready"],
                    "seconds": 5
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("wait completes"),
    );

    assert_eq!(view["outcome"], "matched", "{view}");
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "matched after only {:?}, too fast to be the background job's real output",
        started.elapsed()
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// S2 (wait started after the send): a line this server typed and submitted
/// is not the match; only the command's own output is.
#[tokio::test]
async fn s2_after_a_submitted_commands_echo_is_not_the_match() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "s2-after"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "sleep 1; echo MARKER",
            "enter": true
        })))
        .await
        .expect("the command is sent and this call returns only once it is");

    let started = Instant::now();
    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["MARKER"],
                    "seconds": 5
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("wait completes"),
    );

    assert_eq!(
        view["outcome"], "matched",
        "must not time out over a masked echo either: {view}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "matched after only {:?}, too fast to be sleep 1's real output -- the echo, not the \
         output, was matched",
        started.elapsed()
    );
    assert!(
        view["text"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .any(|line| line.trim() == "MARKER"),
        "the bare output line must be in the reported text: {view}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// S3: text typed but never submitted times out (reported here as `pending`,
/// this port's outcome for exactly this case) rather than matching, and does
/// so promptly rather than waiting out the deadline.
#[tokio::test]
async fn s3_unsubmitted_text_is_pending_not_matched() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "s3"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "echo MARKER",
            "enter": false
        })))
        .await
        .expect("the line is typed but not submitted");

    let started = Instant::now();
    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["MARKER"],
                    "seconds": 1
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("wait completes"),
    );

    assert_eq!(view["outcome"], "pending", "{view}");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "an unsubmitted pattern must not wait out the deadline: {:?}",
        started.elapsed()
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// S4: edits made before a line is submitted are applied, and none of the
/// key names used to make them -- `BSpace` here -- becomes part of the
/// submitted text. The precise claim that `BSpace` never becomes literal
/// text is checked at the unit level
/// (`crate::echo::tests::backspaces_reach_an_earlier_calls_pending_text`);
/// this is the end-to-end shape of the same workflow.
#[tokio::test]
async fn s4_edits_are_applied_before_a_line_is_submitted() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "s4"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "xMARKER",
            "enter": false
        })))
        .await
        .expect("the typo is typed");
    let backspaces: Vec<&str> = vec!["BSpace"; 7];
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "keys": backspaces
        })))
        .await
        .expect("the typo is erased in a separate call");
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "echo MARKER",
            "enter": true
        })))
        .await
        .expect("the corrected line is submitted");

    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["MARKER"],
                    "seconds": 5
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("wait completes"),
    );

    // No `sleep` in this scenario, so the correction, submission, and its
    // output can all land before `wait_for_text` even attaches; either
    // outcome below means the mask found the real row, not the echo.
    assert!(
        matches!(
            view["outcome"].as_str(),
            Some("matched" | "present_at_entry")
        ),
        "{view}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// S5: unsubmitted type-ahead into a pane whose shell has not drawn its
/// first prompt yet must never be reported as a match. No `prompt_ready`
/// wait here -- a cold shell is the point of this scenario.
#[tokio::test]
async fn s5_unsubmitted_type_ahead_into_a_cold_shell_is_never_matched() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    // A shell that reads nothing for a moment: this server's own type-ahead
    // can arrive on the live stream looking exactly like fresh output once
    // the pane's process starts reading it.
    guard
        .server()
        .cmd(
            libtmux::Command::new("set-option")
                .arg("-g")
                .arg("default-command")
                .arg("stty raw -echo; sleep 0.4; exec cat"),
        )
        .await
        .expect("the cold-shell fixture command is set");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "s5"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();

    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "MARKER",
            "enter": false
        })))
        .await
        .expect("type-ahead is queued before the shell reads it");

    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["MARKER"],
                    "seconds": 2
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("wait completes"),
    );

    assert_ne!(
        view["outcome"], "matched",
        "unsubmitted type-ahead must never read as a match: {view}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// S6: after a key this server cannot apply to its tracked text (an arrow
/// key here), it stops discounting that pane's current line rather than
/// keep masking text an edit it could not represent may have moved past.
/// bash runs the pane so `Left` has its ordinary readline meaning -- pure
/// cursor movement, no visible effect on the line -- rather than the
/// undefined one plain `/bin/sh` (this fixture's default) gives it.
#[tokio::test]
async fn s6_an_unrecognized_key_stops_discounting_the_line() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    guard
        .server()
        .cmd(
            libtmux::Command::new("set-option")
                .arg("-g")
                .arg("default-command")
                .arg("/bin/bash --noprofile --norc"),
        )
        .await
        .expect("bash is set as the pane's command");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "s6"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "MARKER",
            "enter": false
        })))
        .await
        .expect("the line is typed");
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "keys": ["Left"]
        })))
        .await
        .expect("an unmodelable key is sent");
    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "keys": ["Enter"]
        })))
        .await
        .expect("the line is submitted");

    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["MARKER"],
                    "seconds": 3
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("wait completes"),
    );

    // No `sleep` here either -- bash rejects the bare command fast enough
    // that this can resolve as `present_at_entry` as easily as `matched`.
    // The discriminator is the outcome family, not which of the two it is:
    // a stale mask would remove `MARKER` from both the echo and the error
    // line, since both are exactly that word, and time out instead.
    assert!(
        matches!(
            view["outcome"].as_str(),
            Some("matched" | "present_at_entry")
        ),
        "an unrecognized key must stop discounting the line rather than keep hiding a real \
         error message that happens to repeat it: {view}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Resize constraint: a window resize between a command being submitted and
/// its output arriving must not defeat the mask. This mask is keyed to the
/// submitted line's own text, not to a remembered row, so a resize is not
/// expected to change the outcome here -- this is the no-resize control
/// (`s2_after_a_submitted_commands_echo_is_not_the_match`) run again with a
/// resize between submit and wait, to check that stays true.
#[tokio::test]
async fn a_resize_between_submit_and_output_does_not_defeat_the_mask() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = bare_tools(guard.server());
    tools
        .create_session(args(serde_json::json!({"name": "resize"})))
        .await
        .expect("session starts");
    let pane = json(tools.list_panes().await.expect("panes"))["panes"][0]["id"]
        .as_str()
        .expect("pane id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    tools
        .send_keys(args(serde_json::json!({
            "pane": pane,
            "text": "sleep 1; echo MARKER",
            "enter": true
        })))
        .await
        .expect("the command is sent");

    // Reflow the pane's rows before the real output arrives.
    guard
        .server()
        .cmd(
            libtmux::Command::new("resize-window")
                .arg("-t")
                .arg(&pane)
                .arg("-x")
                .arg("50")
                .arg("-y")
                .arg("20"),
        )
        .await
        .expect("the window resizes");

    let started = Instant::now();
    let view = json(
        tools
            .wait_for_text(
                args(serde_json::json!({
                    "pane": pane,
                    "patterns": ["MARKER"],
                    "seconds": 5
                })),
                CancellationToken::new(),
                tmux_mcp::Reporter::none(),
            )
            .await
            .expect("wait completes"),
    );

    assert_eq!(view["outcome"], "matched", "{view}");
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "matched after only {:?}, too fast to be real output",
        started.elapsed()
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
