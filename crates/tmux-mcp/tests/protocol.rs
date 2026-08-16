//! The tools as an agent reaches them: over the wire, through the schemas.
//!
//! The other suites call the handler methods directly, which skips the layer
//! an agent actually uses. A tool whose arguments cannot be deserialized from
//! the JSON its own schema advertises passes every method-level test and fails
//! the first real call. So every tool here is called the way a client calls
//! it, with arguments built as JSON.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, Server};
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _, serve_server};
use serde_json::{Value, json};
use tmux_mcp::{Safety, TmuxTools};

/// A client and server talking over an in-memory duplex.
struct Wire {
    client: RunningService<RoleClient, ()>,
    server: tokio::task::JoinHandle<()>,
}

impl Wire {
    async fn connect(tools: TmuxTools) -> Self {
        let (client_transport, server_transport) = tokio::io::duplex(1 << 20);
        let server = tokio::spawn(async move {
            let service = serve_server(tools, server_transport)
                .await
                .expect("server starts");
            let _ = service.waiting().await;
        });
        let client = ().serve(client_transport).await.expect("client connects");
        Self { client, server }
    }

    /// Call a tool with JSON arguments, as a client does.
    ///
    /// Answers come back as `structuredContent`, which is where a typed tool
    /// puts its value; the text block carries the same thing for clients that
    /// predate structured output.
    async fn call(&self, name: &'static str, arguments: Value) -> Result<String, String> {
        let mut params = CallToolRequestParams::default();
        params.name = name.into();
        params.arguments = arguments.as_object().cloned();
        let answer = self
            .client
            .call_tool(params)
            .await
            .map_err(|error| format!("transport: {error}"))?;

        let text = answer
            .content
            .iter()
            .filter_map(|part| part.as_text().map(|text| text.text.clone()))
            .collect::<String>();
        if answer.is_error == Some(true) {
            return Err(text);
        }
        Ok(text)
    }

    /// Call a tool and read the structured value it answered with.
    async fn json(&self, name: &'static str, arguments: Value) -> Value {
        let mut params = CallToolRequestParams::default();
        params.name = name.into();
        params.arguments = arguments.as_object().cloned();
        let answer = self
            .client
            .call_tool(params)
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"));
        assert_ne!(answer.is_error, Some(true), "{name} refused the call");

        // Every tool is typed now, so an answer without structured content is
        // a tool that lost its shape somewhere.
        answer
            .structured_content
            .unwrap_or_else(|| panic!("{name} answered without structured content"))
    }

    async fn shutdown(self) {
        self.client.cancel().await.expect("client shuts down");
        let _ = self.server.await;
    }
}

/// The classification an error carries alongside its message.
///
/// A tool that rejects its own arguments answers with a protocol error.
/// Arguments that never deserialize are refused a layer earlier, by rmcp, and
/// come back as an ordinary result marked `is_error` — which the tool never saw
/// and so cannot classify. That case is called out separately, because reading
/// it as "the call succeeded" would hide a malformed request.
async fn detail(wire: &Wire, name: &'static str, arguments: Value) -> Value {
    let mut params = CallToolRequestParams::default();
    params.name = name.into();
    params.arguments = arguments.as_object().cloned();
    match wire.client.call_tool(params).await {
        Err(rmcp::service::ServiceError::McpError(data)) => data
            .data
            .unwrap_or_else(|| panic!("{name} carried no detail")),
        Err(other) => panic!("{name} failed in the wrong way: {other}"),
        Ok(answer) if answer.is_error == Some(true) => {
            panic!("{name} was refused before it ran, so its arguments do not fit its schema")
        }
        Ok(_) => panic!("{name} should have failed"),
    }
}

/// Wait until a pane's shell has drawn a prompt.
async fn prompt_ready(server: &Server, pane: &str) {
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
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the pane never drew a prompt");
}

#[tokio::test]
async fn every_tool_advertises_a_description_and_a_schema() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    let listed = wire
        .client
        .list_all_tools()
        .await
        .expect("tools are listed");

    assert!(!listed.is_empty(), "the server advertises tools");
    for tool in &listed {
        let name = tool.name.as_ref();
        assert!(
            tool.description
                .as_ref()
                .is_some_and(|text| text.len() > 20),
            "{name} needs a description an agent can choose from",
        );
        // An agent picks tools by reading these, so an empty object schema on
        // a tool that takes arguments is a silent trap.
        let schema = serde_json::to_value(&tool.input_schema).expect("a schema serialises");
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{name} advertises an object schema",
        );
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// One call per tool, with arguments as a client would send them.
///
/// Kept beside the test rather than inside it so the list stays readable as
/// tools are added; the test checks it covers everything the server offers.
#[allow(
    clippy::too_many_lines,
    reason = "one call per tool, and the list is the point"
)]
fn every_call(
    job: &str,
    pane: &str,
    window: &str,
    spare: &str,
    spare_live: &str,
    doomed_window: &str,
) -> Vec<(&'static str, Value)> {
    // Every tool, called as a client calls it. The point is the arguments:
    // each one has to survive JSON, the advertised schema, and serde.
    vec![
        // A plan the wire has to carry intact: a creation, a forward
        // reference to what it makes, and typing into that.
        (
            "run_plan",
            json!({
                "plan": [
                    {"NewSession": {"name": "wired-plan", "start_directory": null,
                                    "window_name": null}},
                    {"SendKeys": {"target": {"Slot": {"index": 0, "part": "FirstPane"}},
                                  "text": "# wired", "keys": [], "enter": true}}
                ],
                "grouping": "sequential"
            }),
        ),
        ("list_sessions", json!({})),
        ("list_windows", json!({})),
        ("list_panes", json!({})),
        ("describe", json!({})),
        ("list_session_windows", json!({"session": "wire"})),
        ("list_window_panes", json!({"window": window})),
        (
            "capture_pane",
            json!({"pane": pane, "history": true, "start": 0, "end": 2}),
        ),
        (
            "snapshot_pane",
            json!({"pane": pane, "max_lines": 4, "history": false}),
        ),
        (
            "search_panes",
            json!({"pattern": "a", "regex": false, "match_case": false, "history": false, "session": "wire"}),
        ),
        (
            "find_panes",
            json!({
                "filter": {"version": 1, "target": "pane",
                           "expr": {"op": "eq", "field": "pane_at_top", "value": true}},
                "session": "wire"
            }),
        ),
        (
            "find_sessions",
            json!({"filter": {"version": 1, "target": "session_tree",
                          "expr": {"op": "relation", "field": "windows", "quantifier": "any",
                                   "expr": {"op": "eq", "field": "window_name", "value": "doomed"}}}}),
        ),
        ("select_pane", json!({"pane": pane, "direction": "next"})),
        (
            "select_window",
            json!({"window": window, "direction": "last"}),
        ),
        (
            "resize_pane",
            json!({"pane": pane, "direction": "up", "cells": 1}),
        ),
        (
            "send_keys",
            json!({"pane": pane, "text": "true", "keys": ["Escape"], "enter": true}),
        ),
        (
            "run_command",
            json!({"pane": pane, "command": "true", "seconds": 25, "suppress_history": true}),
        ),
        (
            "wait_for_text",
            json!({"pane": pane, "patterns": ["never"], "stop": ["nope"], "regex": false, "match_case": true, "seconds": 1}),
        ),
        (
            "watch_pane",
            json!({"pane": pane, "seconds": 1, "max_bytes": 32}),
        ),
        ("capture_since", json!({"pane": pane})),
        (
            "set_option",
            json!({"name": "@wire", "scope": "pane", "target": pane, "value": "v"}),
        ),
        (
            "show_option",
            json!({"name": "@wire", "scope": "pane", "target": pane}),
        ),
        ("signal_channel", json!({"channel": "wire-chan"})),
        (
            "wait_for_channel",
            json!({"channel": "wire-chan", "seconds": 1}),
        ),
        ("rename", json!({"target": window, "name": "renamed"})),
        (
            "split_pane",
            json!({"pane": pane, "direction": "right", "percent": 40, "command": "sleep 60"}),
        ),
        ("kill_pane", json!({"pane": spare})),
        ("kill_window", json!({"window": doomed_window})),
        ("kill_session", json!({"session": "spare-session"})),
        ("new_window", json!({"session": "wire", "name": "last"})),
        (
            "create_session",
            json!({"name": "another", "start_directory": "/tmp"}),
        ),
        (
            "start_command",
            json!({"pane": pane, "command": "sleep 30"}),
        ),
        ("list_servers", json!({})),
        (
            "expand_format",
            json!({"format": "#{pane_id}", "pane": pane}),
        ),
        ("show_environment", json!({})),
        (
            "set_environment",
            json!({"name": "WIRE_VAR", "value": "wire-value"}),
        ),
        ("show_hooks", json!({})),
        ("pipe_pane", json!({"pane": pane})),
        (
            "select_layout",
            json!({"window": window, "layout": "even-vertical"}),
        ),
        ("clear_pane", json!({"pane": pane})),
        (
            "respawn_pane",
            json!({"pane": spare_live, "command": "sleep 60", "kill_first": true}),
        ),
        ("paste_text", json!({"pane": pane, "text": "pasted"})),
        ("job_status", json!({"job": job, "cursor": 0, "seconds": 0})),
        ("list_jobs", json!({})),
        ("cancel_job", json!({"job": job})),
        (
            "wait_for_idle",
            json!({"pane": pane, "quiet_seconds": 1, "seconds": 2}),
        ),
        ("kill_server", json!({})),
    ]
}

#[tokio::test]
async fn every_tool_accepts_the_arguments_its_schema_describes() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    wire.json("create_session", json!({"name": "wire"})).await;
    let panes = wire.json("list_panes", json!({})).await;
    let panes = panes["panes"].as_array().expect("a listing").clone();
    let pane = panes[0]["id"].as_str().expect("a pane id").to_owned();
    let window = panes[0]["window_id"]
        .as_str()
        .expect("a window id")
        .to_owned();
    prompt_ready(guard.server(), &pane).await;

    // These tools answer with the object they made, so the id comes out of it.
    let spare = wire
        .json("split_pane", json!({"pane": pane, "direction": "below"}))
        .await["id"]
        .as_str()
        .expect("a pane to destroy")
        .to_owned();
    let doomed_window = wire
        .json("new_window", json!({"session": "wire", "name": "doomed"}))
        .await["id"]
        .as_str()
        .expect("a window to destroy")
        .to_owned();
    wire.json("create_session", json!({"name": "spare-session"}))
        .await;
    let spare_live = wire
        .json("split_pane", json!({"pane": pane, "direction": "below"}))
        .await["id"]
        .as_str()
        .expect("a pane to respawn")
        .to_owned();

    // Started here rather than in the list, because the job tools need an id
    // that exists and the list is built before any of it runs.
    let job = wire
        .json(
            "start_command",
            json!({"pane": pane, "command": "sleep 30"}),
        )
        .await["job"]
        .as_str()
        .expect("a job id")
        .to_owned();

    let calls = every_call(&job, &pane, &window, &spare, &spare_live, &doomed_window);

    // A list of calls rots the moment a tool is added without one, and a
    // rotted list looks exactly like a passing test. So the list is checked
    // against what the server advertises before any of it runs.
    let advertised: Vec<String> = wire
        .client
        .list_all_tools()
        .await
        .expect("tools are listed")
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    let covered: Vec<&str> = calls.iter().map(|(name, _)| *name).collect();
    let missing: Vec<&String> = advertised
        .iter()
        .filter(|name| !covered.contains(&name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these tools are advertised but never called over the wire: {missing:?}",
    );

    for (name, arguments) in calls {
        let answer = wire.call(name, arguments.clone()).await;
        assert!(
            answer.is_ok(),
            "{name} rejected the arguments its own schema describes: {arguments} -> {answer:?}",
        );
    }

    wire.shutdown().await;
}

/// The tools that change nothing about the server.
const READING: &[&str] = &[
    "list_sessions",
    "list_windows",
    "list_panes",
    "describe",
    "list_session_windows",
    "list_window_panes",
    "capture_pane",
    "snapshot_pane",
    "search_panes",
    "find_panes",
    "find_sessions",
    "show_option",
    "capture_since",
    "watch_pane",
    "wait_for_text",
    "wait_for_idle",
    "wait_for_channel",
    "job_status",
    "list_jobs",
    "list_servers",
    "expand_format",
    "show_environment",
    "show_hooks",
];

/// The tools that destroy work.
const DESTRUCTIVE: &[&str] = &["kill_pane", "kill_window", "kill_session", "kill_server"];

/// The tools that put the caller's own payload into a live terminal.
const OPEN_WORLD: &[&str] = &[
    "send_keys",
    "run_command",
    "start_command",
    "pipe_pane",
    "respawn_pane",
    "paste_text",
];

#[tokio::test]
async fn every_tool_declares_what_it_does_to_the_server() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    let listed = wire
        .client
        .list_all_tools()
        .await
        .expect("tools are listed");

    for tool in &listed {
        let name = tool.name.as_ref();
        let hints = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{name} carries no annotations"));

        assert!(
            tool.title.is_some(),
            "{name} needs a title; clients show it instead of the bare name",
        );

        // A client decides what to run unattended and what to confirm from
        // these three. Leaving them unset on a server that can kill every
        // session on a machine makes `list_panes` and `kill_server` look
        // alike.
        let reads = READING.contains(&name);
        assert_eq!(
            hints.read_only_hint,
            Some(reads),
            "{name} should declare read_only_hint = {reads}",
        );
        let destroys = DESTRUCTIVE.contains(&name);
        assert_eq!(
            hints.destructive_hint,
            Some(destroys),
            "{name} should declare destructive_hint = {destroys}",
        );
        let open = OPEN_WORLD.contains(&name);
        assert_eq!(
            hints.open_world_hint,
            Some(open),
            "{name} should declare open_world_hint = {open}: the effects of \
             these tools reach into whatever the caller supplied",
        );
        assert!(
            hints.idempotent_hint.is_some(),
            "{name} should say whether calling it twice differs from once",
        );
    }

    // A tool added without a place in the taxonomy is a tool whose hints
    // nobody chose. Catch it here rather than shipping the macro's defaults.
    let known: Vec<&str> = READING
        .iter()
        .chain(DESTRUCTIVE)
        .chain(OPEN_WORLD)
        .copied()
        .collect();
    let unclassified: Vec<&str> = listed
        .iter()
        .map(|tool| tool.name.as_ref())
        .filter(|name| !known.contains(name))
        .collect();
    for name in &unclassified {
        let tool = listed
            .iter()
            .find(|tool| tool.name.as_ref() == *name)
            .expect("the tool was just listed");
        let hints = tool.annotations.as_ref().expect("annotations");
        assert_eq!(
            (
                hints.read_only_hint,
                hints.destructive_hint,
                hints.open_world_hint
            ),
            (Some(false), Some(false), Some(false)),
            "{name} is not in any named group, so it must be an additive, \
             closed-world change; if it is not, add it to the right list",
        );
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_recipes_teach_what_no_single_tool_can_say() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(TmuxTools::new(guard.server().clone())).await;

    let listed = wire
        .client
        .list_all_prompts()
        .await
        .expect("prompts are listed");
    let names: Vec<&str> = listed.iter().map(|prompt| &*prompt.name).collect();
    for expected in ["run_and_wait", "interrupt_gracefully", "diagnose_pane"] {
        assert!(
            names.contains(&expected),
            "{expected} is offered: {names:?}"
        );
    }
    for prompt in &listed {
        let name: &str = &prompt.name;
        assert!(prompt.title.is_some(), "{name} needs a title");
        assert!(prompt.description.is_some(), "{name} needs a description");
    }

    // A recipe is only worth its place if it renders with the caller's
    // arguments in it, so check the text an agent would actually receive.
    // GetPromptRequestParams is #[non_exhaustive], so it is built from
    // the default rather than named field by field.
    let mut params = rmcp::model::GetPromptRequestParams::default();
    params.name = "run_and_wait".into();
    params.arguments = json!({"pane": "%7", "command": "cargo test"})
        .as_object()
        .cloned();
    let rendered = wire
        .client
        .get_prompt(params)
        .await
        .expect("the recipe renders");
    let text: String = rendered
        .messages
        .iter()
        .filter_map(|message| message.content.as_text().map(|text| text.text.clone()))
        .collect();

    assert!(text.contains("%7"), "the pane reaches the text: {text}");
    assert!(text.contains("cargo test"), "so does the command: {text}");
    // The three things this recipe exists to say.
    assert!(text.contains("run_command"), "it names the right tool");
    assert!(
        text.contains("deadline") && text.contains("no_shell"),
        "and the two outcomes that are not failures: {text}",
    );

    // GetPromptRequestParams is #[non_exhaustive], so it is built from
    // the default rather than named field by field.
    let mut params = rmcp::model::GetPromptRequestParams::default();
    params.name = "interrupt_gracefully".into();
    params.arguments = json!({"pane": "%2"}).as_object().cloned();
    let rendered = wire
        .client
        .get_prompt(params)
        .await
        .expect("the recipe renders");
    let text: String = rendered
        .messages
        .iter()
        .filter_map(|message| message.content.as_text().map(|text| text.text.clone()))
        .collect();
    assert!(
        text.contains("C-c") && text.contains("keys"),
        "the interrupt recipe must say to send the key, not type it: {text}",
    );

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_tier_withholds_the_tools_above_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");

    // Withheld, not merely refused: an agent cannot choose what it cannot
    // see, and a refusal it never provokes is better than one it has to
    // learn from.
    for (tier, expected_kills) in [
        (Safety::ReadOnly, 0),
        (Safety::Mutating, 0),
        (Safety::Destructive, 4),
    ] {
        let wire = Wire::connect(
            TmuxTools::builder(guard.server().clone())
                .safety(tier)
                .build(),
        )
        .await;
        let listed = wire
            .client
            .list_all_tools()
            .await
            .expect("tools are listed");
        let names: Vec<&str> = listed.iter().map(|tool| tool.name.as_ref()).collect();

        let kills = names
            .iter()
            .filter(|name| name.starts_with("kill_"))
            .count();
        assert_eq!(kills, expected_kills, "{tier:?} offered {names:?}");

        // Reading is always offered; it is the floor every tier shares.
        assert!(
            names.contains(&"list_panes"),
            "{tier:?} withheld list_panes"
        );

        let writes = names.contains(&"send_keys");
        assert_eq!(
            writes,
            tier != Safety::ReadOnly,
            "{tier:?} should {} send_keys",
            if tier == Safety::ReadOnly {
                "withhold"
            } else {
                "offer"
            },
        );

        // A withheld tool is gone, not hidden-but-callable.
        if tier != Safety::Destructive {
            assert!(
                wire.call("kill_server", json!({})).await.is_err(),
                "{tier:?} advertised no kill_server but still answered one",
            );
        }

        wire.shutdown().await;
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_default_tier_withholds_destruction() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    // What an operator gets without saying anything. This server can end
    // every session on a machine, and the caller guard only protects the
    // pane the agent talks through.
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::default())
            .build(),
    )
    .await;

    let listed = wire
        .client
        .list_all_tools()
        .await
        .expect("tools are listed");
    let names: Vec<&str> = listed.iter().map(|tool| tool.name.as_ref()).collect();

    assert!(
        !names.iter().any(|name| name.starts_with("kill_")),
        "the default tier should withhold destruction: {names:?}",
    );
    assert!(names.contains(&"run_command"), "but still allow work done");

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_discovery_anchors_ask_to_stay_loaded() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    let listed = wire
        .client
        .list_all_tools()
        .await
        .expect("tools are listed");

    let marked: Vec<&str> = listed
        .iter()
        .filter(|tool| {
            tool.meta
                .as_ref()
                .and_then(|meta| meta.0.get("anthropic/alwaysLoad"))
                == Some(&Value::Bool(true))
        })
        .map(|tool| tool.name.as_ref())
        .collect();

    // Three, and these three: enough that a bare "what is in my pane" finds
    // the server, few enough that the hint keeps its value. Marking more is a
    // decision to make deliberately, not by accident.
    assert_eq!(
        marked.len(),
        3,
        "expected exactly three discovery anchors, got {marked:?}",
    );
    for anchor in ["list_panes", "describe", "snapshot_pane"] {
        assert!(marked.contains(&anchor), "{anchor} is a discovery anchor");
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn every_tool_answers_with_a_typed_value() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    let listed = wire
        .client
        .list_all_tools()
        .await
        .expect("tools are listed");

    for tool in &listed {
        let name = tool.name.as_ref();
        // Without a schema an agent has to call the tool to learn what comes
        // back, which is the guessing this was meant to end.
        let schema = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("{name} does not say what it answers with"));
        let schema = serde_json::to_value(schema).expect("a schema serialises");
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{name} must answer with an object: the protocol says structured \
             content is one, so a bare array or string has nowhere to go",
        );
    }

    // And the value really arrives in the structured field, not only as text.
    wire.json("create_session", json!({"name": "typed"})).await;
    let answer = wire.json("list_panes", json!({})).await;
    assert!(
        answer["panes"].is_array(),
        "a listing arrives wrapped: {answer}",
    );

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_tool_that_does_not_exist_is_refused_over_the_wire() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    assert!(
        wire.call("no_such_tool", json!({})).await.is_err(),
        "an unknown tool is an error rather than a silent success",
    );
    assert!(
        wire.call("capture_pane", json!({})).await.is_err(),
        "a missing required argument is an error",
    );
    assert!(
        wire.call("capture_pane", json!({"pane": 42}))
            .await
            .is_err(),
        "an argument of the wrong type is an error",
    );

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_command_cannot_forge_the_result_of_its_own_run() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    wire.json("create_session", json!({"name": "forge"})).await;
    let panes = wire.json("list_panes", json!({})).await;
    let panes = panes["panes"].as_array().expect("a listing").clone();
    let pane = panes[0]["id"].as_str().expect("a pane id").to_owned();
    prompt_ready(guard.server(), &pane).await;

    // A run is bracketed by APC strings carrying a nonce. A command is free to
    // print APC of its own, including something shaped exactly like a closing
    // sentinel, and none of it may be read as this run's result.
    for (label, command, expected) in [
        (
            "foreign APC",
            r"printf 'before\033_not-ours\033\\after\n'; exit 4",
            4,
        ),
        (
            "sentinel-shaped APC with a foreign nonce",
            r"printf 'x\033_deadbeefe;99\033\\y\n'; exit 5",
            5,
        ),
        (
            "the sentinel's source text",
            r"printf '%s\n' '\033_fake\033\\'; exit 6",
            6,
        ),
    ] {
        let answer = wire
            .json(
                "run_command",
                json!({"pane": pane, "command": command, "seconds": 30}),
            )
            .await;
        assert_eq!(answer["outcome"], "completed", "{label}: {answer}");
        assert_eq!(
            answer["exit_status"], expected,
            "{label} must not be read as this run's status: {answer}",
        );
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn tmux_metacharacters_survive_the_round_trip() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    // A semicolon ends a command in tmux's own parser, and a space separates
    // arguments. Anything reaching tmux has to survive both.
    //
    // `$` is deliberately absent: tmux 3.2a and 3.4 escape it into the name
    // they actually store, so `dol$lar` becomes `dol\$lar` there and killing
    // it by the original name fails. That is tmux renaming the session, not
    // this crate mangling it, and it is not ours to paper over.
    for name in [
        "semi;colon",
        "with space",
        "quo'te",
        "dou\"ble",
        "bra{ce}",
        "ha#sh",
    ] {
        wire.call("create_session", json!({"name": name}))
            .await
            .unwrap_or_else(|error| panic!("creating {name:?}: {error}"));

        let listed = wire.json("list_sessions", json!({})).await;
        let found = listed["sessions"]
            .as_array()
            .expect("a listing")
            .iter()
            .any(|session| session["name"] == name);
        assert!(found, "{name:?} is listed as it was given: {listed}");

        wire.call("kill_session", json!({"session": name}))
            .await
            .unwrap_or_else(|error| panic!("killing {name:?}: {error}"));
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

// JSON Schema says an unknown `format` is an annotation to ignore, so
// nothing breaks -- it is the client's log that suffers. One real client
// printed a line per occurrence on every listing.
const KNOWN: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "uri-template",
    "json-pointer",
    "relative-json-pointer",
    "regex",
    "int32",
    "int64",
    "float",
    "double",
];

/// Collect every `format` under a schema, with the path that reached it.
fn formats(node: &Value, path: &str, found: &mut Vec<(String, String)>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(format)) = map.get("format") {
                found.push((path.to_owned(), format.clone()));
            }
            for (key, value) in map {
                formats(value, &format!("{path}/{key}"), found);
            }
        }
        Value::Array(items) => {
            for (index, value) in items.iter().enumerate() {
                formats(value, &format!("{path}/{index}"), found);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn every_advertised_schema_uses_formats_json_schema_defines() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;

    let listed = wire
        .client
        .list_all_tools()
        .await
        .expect("tools are listed");
    let mut found = Vec::new();
    for tool in &listed {
        let input = serde_json::to_value(&tool.input_schema).expect("a schema serialises");
        formats(&input, &format!("{}:input", tool.name), &mut found);
        if let Some(output) = tool.output_schema.as_ref() {
            let output = serde_json::to_value(output).expect("a schema serialises");
            formats(&output, &format!("{}:output", tool.name), &mut found);
        }
    }

    let unknown: Vec<_> = found
        .iter()
        .filter(|(_, format)| !KNOWN.contains(&format.as_str()))
        .collect();
    assert!(
        unknown.is_empty(),
        "these advertise a format JSON Schema does not define: {unknown:?}",
    );

    // An unsigned field still says it cannot go negative, which is the part
    // the removed format was standing in for.
    let snapshot = listed
        .iter()
        .find(|tool| tool.name == "snapshot_pane")
        .expect("snapshot_pane is offered");
    let schema = serde_json::to_value(
        snapshot
            .output_schema
            .as_ref()
            .expect("snapshot_pane answers with a typed value"),
    )
    .expect("a schema serialises");
    assert_eq!(schema["properties"]["width"]["minimum"], 0, "{schema}");

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_failure_says_whether_retrying_could_help() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    // Destructive, so the kill tools are offered: a tool the tier withheld
    // would fail as an unknown tool and never reach the classification.
    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .safety(Safety::Destructive)
            .build(),
    )
    .await;
    wire.json("create_session", json!({"name": "kinds"})).await;

    // A pane that is not there is the caller's problem, and a listing taken
    // now would say something different -- so it is worth taking again.
    let gone = detail(&wire, "capture_pane", json!({"pane": "%9999"})).await;
    assert_eq!(gone["kind"], "object_gone", "{gone}");
    assert_eq!(gone["stale"], true, "a vanished target is worth re-listing");
    assert_eq!(
        gone["retryable"], false,
        "the same call would fail the same way; the listing has to change first",
    );

    // An argument this server will not pass on is also the caller's problem,
    // but nothing has gone stale: re-listing would not help.
    let rejected = detail(
        &wire,
        "select_pane",
        json!({"pane": "%0", "direction": "sideways"}),
    )
    .await;
    assert_eq!(rejected["kind"], "invalid_input", "{rejected}");
    assert_eq!(
        rejected["stale"], false,
        "a bad argument does not mean the listing is out of date",
    );

    // The vocabulary is only useful if it is total: an agent that reads these
    // fields must not have to also handle their absence.
    let failures = [
        ("capture_pane", json!({"pane": "nonsense"})),
        ("list_window_panes", json!({"window": "@9999"})),
        ("list_session_windows", json!({"session": "no-such"})),
        ("rename", json!({"target": "@9999", "name": "x"})),
        ("kill_pane", json!({"pane": "%9999"})),
        (
            "resize_pane",
            json!({"pane": "%0", "direction": "inward", "cells": 1}),
        ),
        ("search_panes", json!({"pattern": "(", "regex": true})),
        ("show_option", json!({"scope": "session", "name": "status"})),
        (
            "wait_for_text",
            json!({"pane": "%9999", "patterns": ["x"], "seconds": 1}),
        ),
        ("run_command", json!({"pane": "%9999", "command": "true"})),
        ("capture_since", json!({"pane": "%9999"})),
    ];
    for (tool, arguments) in failures {
        let classified = detail(&wire, tool, arguments).await;
        assert!(
            classified["kind"].is_string(),
            "{tool} answered without a kind: {classified}",
        );
        assert!(
            classified["retryable"].is_boolean(),
            "{tool} did not say whether retrying helps: {classified}",
        );
        assert!(
            classified["stale"].is_boolean(),
            "{tool} did not say whether its listing is out of date: {classified}",
        );
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Read one URI and hand back its single text body.
async fn body(wire: &Wire, uri: String) -> String {
    let read = wire
        .client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(uri.clone()))
        .await
        .unwrap_or_else(|error| panic!("{uri} is readable: {error}"));
    match read.contents.into_iter().next() {
        Some(rmcp::model::ResourceContents::TextResourceContents { text, .. }) => text,
        other => panic!("{uri} answered with {other:?}"),
    }
}

#[tokio::test]
async fn the_hierarchy_is_reachable_as_resources() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(TmuxTools::new(guard.server().clone())).await;

    // A name with a space, because a client fills a template by percent
    // encoding and the server has to undo exactly that.
    wire.json("create_session", json!({"name": "res demo"}))
        .await;
    let panes = wire.json("list_panes", json!({})).await;
    let pane = panes["panes"][0]["id"]
        .as_str()
        .expect("a pane id is a string")
        .to_owned();
    let windows = wire.json("list_windows", json!({})).await;
    let index = windows["windows"][0]["index"].clone();

    // Everything advertised is readable. A picker that lists a URI the server
    // then refuses is worse than not listing it.
    let listed = wire
        .client
        .list_all_resources()
        .await
        .expect("resources are listed");
    assert!(!listed.is_empty(), "the server advertises resources");
    for resource in &listed {
        let read = wire
            .client
            .read_resource(rmcp::model::ReadResourceRequestParams::new(
                resource.uri.clone(),
            ))
            .await
            .unwrap_or_else(|error| {
                panic!("{} is advertised but unreadable: {error}", resource.uri)
            });
        assert!(
            !read.contents.is_empty(),
            "{} answered with nothing",
            resource.uri,
        );
    }

    // The templated forms, filled the way a client fills them. A URI naming
    // one thing answers with that thing: wrapping it in the tools' list
    // object would make every reader index into a one-element array to reach
    // what the URI already named.
    let session: Value =
        serde_json::from_str(&body(&wire, "tmux://sessions/res%20demo".to_owned()).await)
            .expect("a session resource is JSON");
    assert_eq!(session["name"], "res demo", "{session}");
    assert!(
        session.get("sessions").is_none(),
        "a single session is not wrapped in a listing: {session}",
    );

    let window: Value = serde_json::from_str(
        &body(&wire, format!("tmux://sessions/res%20demo/windows/{index}")).await,
    )
    .expect("a window resource is JSON");
    assert_eq!(window["index"], index, "{window}");
    assert!(window.get("windows").is_none(), "{window}");

    let one: Value = serde_json::from_str(&body(&wire, format!("tmux://panes/{pane}")).await)
        .expect("a pane resource is JSON");
    assert_eq!(one["id"], pane.as_str(), "{one}");
    assert!(one.get("panes").is_none(), "{one}");

    // The plural forms keep the wrapper, and their elements are the same
    // shape the singular forms answer with.
    let all: Value = serde_json::from_str(&body(&wire, "tmux://panes".to_owned()).await)
        .expect("the pane listing is JSON");
    assert!(all["panes"].is_array(), "{all}");
    assert_eq!(all["panes"][0]["id"], one["id"], "same shape either way");

    // Pane content is text rather than JSON, because it is what a terminal
    // drew rather than a value with fields.
    let content = wire
        .client
        .read_resource(rmcp::model::ReadResourceRequestParams::new(format!(
            "tmux://panes/{pane}/content"
        )))
        .await
        .expect("pane content is readable");
    match content.contents.first() {
        Some(rmcp::model::ResourceContents::TextResourceContents { mime_type, .. }) => {
            assert_eq!(mime_type.as_deref(), Some("text/plain"));
        }
        other => panic!("pane content came back as {other:?}"),
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_resource_that_names_nothing_is_refused_in_the_same_vocabulary() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(TmuxTools::new(guard.server().clone())).await;

    for (uri, kind) in [
        ("tmux://nope", "invalid_input"),
        ("tmux://panes/%9999", "object_gone"),
        ("tmux://sessions/no-such-session", "object_gone"),
    ] {
        let error = wire
            .client
            .read_resource(rmcp::model::ReadResourceRequestParams::new(uri.to_owned()))
            .await
            .err()
            .unwrap_or_else(|| panic!("{uri} should have failed"));
        let rmcp::service::ServiceError::McpError(data) = error else {
            panic!("{uri} failed in the wrong way");
        };
        // The same three fields the tools carry, so a client that reads them
        // does not need a second vocabulary for resources.
        let detail = data
            .data
            .unwrap_or_else(|| panic!("{uri} carried no classification"));
        assert_eq!(detail["kind"], kind, "{uri}: {detail}");
        assert!(detail["retryable"].is_boolean(), "{uri}: {detail}");
        assert!(detail["stale"].is_boolean(), "{uri}: {detail}");
    }

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn an_option_value_may_be_written_as_a_number_or_a_flag() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let wire = Wire::connect(TmuxTools::new(guard.server().clone())).await;
    wire.json("create_session", json!({"name": "opts"})).await;

    // tmux stores every option as text, so a number here is a spelling rather
    // than a type. An agent setting a limit writes 5000, and refusing that is
    // a deserialization error with no tmux in it -- the least useful failure
    // to hand back.
    wire.json(
        "set_option",
        json!({"name": "history-limit", "value": 5000, "scope": "global-session"}),
    )
    .await;
    let read = wire
        .json(
            "show_option",
            json!({"name": "history-limit", "scope": "global-session"}),
        )
        .await;
    assert_eq!(read["value"], "5000", "{read}");

    // A boolean becomes the on/off tmux itself uses, not "true".
    wire.json(
        "set_option",
        json!({"name": "status", "value": false, "scope": "global-session"}),
    )
    .await;
    let read = wire
        .json(
            "show_option",
            json!({"name": "status", "scope": "global-session"}),
        )
        .await;
    assert_eq!(read["value"], "off", "{read}");

    // And a string still means itself.
    wire.json(
        "set_option",
        json!({"name": "status-left", "value": "hello", "scope": "global-session"}),
    )
    .await;
    let read = wire
        .json(
            "show_option",
            json!({"name": "status-left", "scope": "global-session"}),
        )
        .await;
    assert_eq!(read["value"], "hello", "{read}");

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}
