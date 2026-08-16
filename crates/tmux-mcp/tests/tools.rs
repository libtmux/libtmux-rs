//! The MCP tool surface, exercised against real tmux.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::test::TestServer;
use rmcp::ServiceExt as _;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolRequestParams;
use rmcp::serve_server;
use serde_json::Value;
use tmux_mcp::TmuxTools;

/// The rows of a listing answer.
///
/// A listing is wrapped in a named object -- `{"panes": [...]}` -- because the
/// protocol says structured content is an object. The single array inside is
/// what these tests are after.
/// Render a tool's typed answer the way a client receives it.
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

/// What a `capture_pane` answer is showing.
fn text<T: serde::Serialize>(answer: rmcp::handler::server::wrapper::Json<T>) -> String {
    json(answer)["text"]
        .as_str()
        .expect("a capture carries text")
        .to_owned()
}

fn rows<T: serde::Serialize>(answer: rmcp::handler::server::wrapper::Json<T>) -> Vec<Value> {
    let value = serde_json::to_value(answer.0).expect("a listing serialises");
    match value {
        Value::Array(rows) => rows,
        Value::Object(fields) => fields
            .values()
            .find_map(|field| field.as_array().cloned())
            .expect("a listing wraps exactly one array"),
        other => panic!("not a listing: {other}"),
    }
}

#[tokio::test]
async fn listing_tools_report_the_live_hierarchy() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());

    assert!(rows(tools.list_sessions().await.expect("sessions")).is_empty());

    tools
        .create_session(Parameters(
            serde_json::from_value(serde_json::json!({
                "name": "work"
            }))
            .expect("arguments deserialize"),
        ))
        .await
        .expect("session is created");

    let sessions = rows(tools.list_sessions().await.expect("sessions"));
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["name"], "work");
    assert_eq!(sessions[0]["windows"], 1);
    assert_eq!(sessions[0]["attached"], false);
    assert!(
        sessions[0]["id"]
            .as_str()
            .expect("an id is a string")
            .starts_with('$'),
    );

    let windows = rows(tools.list_windows().await.expect("windows"));
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0]["session_id"], sessions[0]["id"]);
    assert_eq!(windows[0]["linked"], false);

    let panes = rows(tools.list_panes().await.expect("panes"));
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0]["window_id"], windows[0]["id"]);
    assert_eq!(panes[0]["active"], true);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn an_unknown_target_is_invalid_input_rather_than_an_internal_failure() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());

    let error = tools
        .capture_pane(Parameters(
            serde_json::from_value(serde_json::json!({"pane": "%404"}))
                .expect("arguments deserialize"),
        ))
        .await
        .map(|_| ())
        .expect_err("an unknown pane is refused");
    assert!(
        error.message.contains("%404"),
        "the caller learns which target was unknown: {}",
        error.message,
    );

    let error = tools
        .kill_session(Parameters(
            serde_json::from_value(serde_json::json!({"session": "absent"}))
                .expect("arguments deserialize"),
        ))
        .await
        .map(|_| ())
        .expect_err("an unknown session is refused");
    assert!(error.message.contains("absent"));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn mutating_tools_change_what_the_listing_tools_report() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());

    tools
        .create_session(Parameters(
            serde_json::from_value(serde_json::json!({"name": "driven"}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("session is created");

    let pane = rows(tools.list_panes().await.expect("panes"))[0]["id"]
        .as_str()
        .expect("an id is a string")
        .to_owned();

    let created = tools
        .split_pane(Parameters(
            serde_json::from_value(serde_json::json!({"pane": pane}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("split succeeds");
    let created = id(created);
    assert!(created.starts_with('%'));
    assert_eq!(rows(tools.list_panes().await.expect("panes")).len(), 2);

    tools
        .kill_session(Parameters(
            serde_json::from_value(serde_json::json!({"session": "driven"}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("kill succeeds");
    assert!(rows(tools.list_sessions().await.expect("sessions")).is_empty());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_server_advertises_its_tools_over_the_protocol() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    // The full surface, named explicitly: the default tier withholds the
    // tools that destroy work, and this test is about what is advertised.
    let tools = TmuxTools::builder(guard.server().clone())
        .safety(tmux_mcp::Safety::Destructive)
        .build();

    // Drive the real protocol over an in-memory duplex rather than trusting
    // the handler methods alone: the tool schemas are what an agent sees.
    let (client_transport, server_transport) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        let service = serve_server(tools, server_transport)
            .await
            .expect("server starts");
        service.waiting().await
    });

    let client = ().serve(client_transport).await.expect("client connects");
    let listed = client.list_all_tools().await.expect("tools are listed");
    let names: Vec<_> = listed.iter().map(|tool| tool.name.as_ref()).collect();

    for expected in [
        "list_sessions",
        "list_windows",
        "list_panes",
        "capture_pane",
        "create_session",
        "kill_session",
        "split_pane",
        "send_keys",
        "find_panes",
        "list_session_windows",
        "list_window_panes",
        "new_window",
        "kill_window",
        "kill_pane",
        "rename",
        "describe",
        "watch_pane",
        "resize_pane",
        "find_sessions",
    ] {
        assert!(names.contains(&expected), "{expected} is advertised");
    }

    // CallToolRequestParams is #[non_exhaustive], so it is built from the
    // default rather than named field by field.
    let mut call = CallToolRequestParams::default();
    call.name = "create_session".into();
    call.arguments = serde_json::json!({"name": "over-the-wire"})
        .as_object()
        .cloned();
    let created = client
        .call_tool(call)
        .await
        .expect("the tool call succeeds");
    assert_eq!(created.is_error, Some(false));

    let mut call = CallToolRequestParams::default();
    call.name = "list_sessions".into();
    let listed = client
        .call_tool(call)
        .await
        .expect("the tool call succeeds");
    let payload = format!("{listed:?}");
    assert!(
        payload.contains("over-the-wire"),
        "the session created over the protocol is visible over it too",
    );

    client.cancel().await.expect("client shuts down");
    let _ = server.await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_pane_is_split_where_the_caller_says_and_from_the_pane_it_names() {
    use tmux_mcp::{ResizePaneArgs, SplitPaneArgs};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("placed").await.expect("session");
    let tools = TmuxTools::new(server.clone());

    let first = session
        .panes()
        .await
        .expect("panes")
        .remove(0)
        .id()
        .to_string();

    let second = id(tools
        .split_pane(Parameters(SplitPaneArgs {
            pane: first.clone(),
            direction: Some("right".into()),
            percent: Some(50),
            command: Some("sleep 300".into()),
        }))
        .await
        .expect("the split succeeds"));

    // Splitting the second pane must divide that pane, not whichever one tmux
    // considers active. Asking for a third of it is how that shows.
    let third = id(tools
        .split_pane(Parameters(SplitPaneArgs {
            pane: second.clone(),
            direction: Some("below".into()),
            percent: Some(50),
            command: Some("sleep 300".into()),
        }))
        .await
        .expect("the split succeeds"));

    let panes = session.panes().await.expect("panes");
    assert_eq!(panes.len(), 3);

    let find = |id: &str| {
        panes
            .iter()
            .find(|pane| pane.id().to_string() == id)
            .expect("the pane exists")
            .clone()
    };
    let (second_pane, third_pane) = (find(&second), find(&third));
    assert_eq!(
        second_pane.width(),
        third_pane.width(),
        "the third pane came out of the second, so they share its width",
    );
    assert!(
        find(&first).width() != second_pane.width() || second_pane.height() < find(&first).height(),
        "the first pane was not the one divided the second time",
    );

    // A direction that is not one of the four is the caller's mistake, and is
    // reported as such rather than silently defaulting.
    assert!(
        tools
            .split_pane(Parameters(SplitPaneArgs {
                pane: first.clone(),
                direction: Some("sideways".into()),
                percent: None,
                command: None,
            }))
            .await
            .is_err(),
    );

    let sized = tools
        .resize_pane(Parameters(ResizePaneArgs {
            pane: third.clone(),
            direction: "up".into(),
            cells: 2,
        }))
        .await
        .expect("the resize succeeds");
    let sized = json(sized);
    assert_eq!(sized["pane"], third.as_str());
    assert!(
        sized["width"].as_u64().is_some_and(|width| width > 0)
            && sized["height"].as_u64().is_some_and(|height| height > 0),
        "the new size is reported: {sized}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn reading_a_pane_can_reach_past_the_visible_screen() {
    use tmux_mcp::CapturePaneArgs;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("scrolled").await.expect("session");
    let tools = TmuxTools::new(server.clone());

    let pane = session.panes().await.expect("panes").remove(0);
    let id = pane.id().to_string();

    // More lines than the screen holds, so the early ones are only reachable
    // through the history.
    pane.send_keys("for i in $(seq 1 200); do echo line-$i; done")
        .await
        .expect("keys are sent");
    pane.send_key_names(["Enter"]).await.expect("Enter is sent");

    let read = |history: bool| {
        let tools = tools.clone();
        let pane = id.clone();
        async move {
            tools
                .capture_pane(Parameters(CapturePaneArgs {
                    last_command: false,
                    pane,
                    history,
                    start: None,
                    end: None,
                }))
                .await
                .map(text)
                .expect("the capture succeeds")
        }
    };

    libtmux::test::retry_until(std::time::Duration::from_secs(30), || async {
        read(false).await.contains("line-200")
    })
    .await
    .expect("the command finishes");

    let visible = read(false).await;
    let everything = read(true).await;

    assert!(
        !visible.contains("line-1\n"),
        "the first line has scrolled off the visible screen",
    );
    assert!(
        everything.contains("line-1\n"),
        "and is still in the history",
    );
    assert!(everything.len() > visible.len());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn watching_a_pane_reports_what_capture_would_miss() {
    use tmux_mcp::{SendKeysArgs, WatchPaneArgs};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("watched").await.expect("session");
    let tools = TmuxTools::new(server.clone());

    let pane = session
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane")
        .id()
        .to_string();

    // Watch and type at the same time: watching blocks for its whole window,
    // so an agent driving both has to overlap them, and the tool has to keep
    // reporting while something else is writing.
    let watching = tokio::spawn({
        let tools = tools.clone();
        let pane = pane.clone();
        async move {
            tools
                .watch_pane(Parameters(WatchPaneArgs {
                    pane,
                    seconds: 5,
                    max_bytes: None,
                }))
                .await
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    tools
        .send_keys(Parameters(SendKeysArgs {
            pane: pane.clone(),
            // The escape is written by the command rather than left to the
            // shell's prompt: whether a prompt emits colour or redraws is a
            // property of whichever shell the fixture runs, and the assertion
            // below is about this tool not stripping escapes, not about that.
            text: Some(r"printf 'from-the-pane\033[31m'".to_owned()),
            keys: None,
            enter: true,
        }))
        .await
        .expect("keys are sent");

    let watched = watching
        .await
        .expect("the watch task finishes")
        .expect("watched");
    let view = json(watched);

    assert_eq!(view["pane"], pane);
    assert!(
        view["output"]
            .as_str()
            .expect("output is text")
            .contains("from-the-pane"),
        "the pane's own output is reported: {view}",
    );
    assert!(view["bytes"].as_u64().expect("a byte count") > 0);

    // This tool is the raw stream, which is what makes it different from
    // wait_for_text: that one strips escape sequences before matching, and
    // this one must not, or a caller watching for terminal control -- a
    // progress bar redrawing, an alternate-screen switch -- would be handed
    // output with the very bytes it was watching for removed.
    let output = view["output"].as_str().expect("output is text");
    assert!(
        output.contains('\u{1b}'),
        "watch_pane reports bytes as written, escapes included: {output:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_portable_filter_expression_selects_panes_over_the_protocol() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());

    tools
        .create_session(Parameters(
            serde_json::from_value(serde_json::json!({"name": "filtered"}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("session is created");

    let pane = rows(tools.list_panes().await.expect("panes"))[0]["id"]
        .as_str()
        .expect("an id is a string")
        .to_owned();
    tools
        .split_pane(Parameters(
            serde_json::from_value(serde_json::json!({"pane": pane}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("split succeeds");

    // The same envelope the TypeScript port speaks.
    let matched = tools
        .find_panes(Parameters(
            serde_json::from_value(serde_json::json!({
                "filter": {
                    "version": 1,
                    "target": "pane",
                    "expr": {"field": "pane_active", "op": "eq", "value": true},
                }
            }))
            .expect("arguments deserialize"),
        ))
        .await
        .expect("the filter is accepted");
    let matched = rows(matched);
    assert_eq!(matched.len(), 1, "exactly one pane is active");
    assert_eq!(matched[0]["active"], true);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane's fields are its own, so "which session is it in" is not something
/// an expression can ask. Narrowing is how that question gets asked, and the
/// two compose.
#[tokio::test]
async fn a_session_is_found_by_what_it_contains() {
    use tmux_mcp::TreeFilterArgs;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let tools = TmuxTools::new(server.clone());

    server
        .new_session(libtmux::NewSessionOptions::new("has-editor").window_name("editor"))
        .await
        .expect("session");
    server
        .new_session(libtmux::NewSessionOptions::new("has-shell").window_name("shell"))
        .await
        .expect("session");

    let find = |expression: Value| {
        let tools = tools.clone();
        async move {
            tools
                .find_sessions(Parameters(TreeFilterArgs { filter: expression }))
                .await
        }
    };

    // The question find_panes cannot ask: which sessions hold a window named
    // this. A pane expression names a pane's own fields only.
    let matched = rows(
        find(serde_json::json!({
            "version": 1,
            "target": "session_tree",
            "expr": {
                "op": "relation",
                "quantifier": "any",
                "field": "windows",
                "expr": {"op": "eq", "field": "window_name", "value": "editor"},
            },
        }))
        .await
        .expect("the filter runs"),
    );
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0]["name"], "has-editor");

    // A session's own fields sit alongside the relation, not behind it.
    let combined = rows(
        find(serde_json::json!({
            "version": 1,
            "target": "session_tree",
            "expr": {"op": "and", "args": [
                {"op": "starts_with", "field": "session_name", "value": "has-"},
                {"op": "relation", "quantifier": "none", "field": "windows",
                 "expr": {"op": "eq", "field": "window_name", "value": "editor"}},
            ]},
        }))
        .await
        .expect("the filter runs"),
    );
    assert_eq!(combined.len(), 1);
    assert_eq!(combined[0]["name"], "has-shell");

    // An expression naming a field the related type does not have is the
    // caller's mistake, reported rather than matching nothing.
    assert!(
        find(serde_json::json!({
            "version": 1,
            "target": "session_tree",
            "expr": {
                "op": "relation",
                "quantifier": "any",
                "field": "windows",
                "expr": {"op": "eq", "field": "pane_index", "value": 0},
            },
        }))
        .await
        .is_err(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_filter_is_narrowed_by_scope_rather_than_by_naming_a_parent() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());

    for name in ["filtered", "elsewhere"] {
        tools
            .create_session(Parameters(
                serde_json::from_value(serde_json::json!({"name": name})).expect("arguments parse"),
            ))
            .await
            .expect("the session is created");
    }

    let active = |session: Option<&str>| {
        let tools = tools.clone();
        let mut arguments = serde_json::json!({
            "filter": {"version": 1, "target": "pane", "expr": {
                "field": "pane_active", "op": "eq", "value": true}},
        });
        if let Some(session) = session {
            arguments["session"] = session.into();
        }
        async move {
            tools
                .find_panes(Parameters(
                    serde_json::from_value(arguments).expect("arguments parse"),
                ))
                .await
        }
    };

    let everywhere = rows(active(None).await.expect("the filter runs"));
    let scoped = rows(active(Some("elsewhere")).await.expect("the filter runs"));

    assert!(
        everywhere.len() > scoped.len(),
        "the same expression covers both sessions unscoped: {everywhere:?}",
    );
    assert_eq!(scoped.len(), 1, "and one session when narrowed to it");

    // A scope tmux does not have is the caller's mistake, not an empty result.
    assert!(active(Some("no-such-session")).await.is_err());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn scoped_tools_narrow_to_one_parent() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());

    for name in ["one", "two"] {
        tools
            .create_session(Parameters(
                serde_json::from_value(serde_json::json!({"name": name}))
                    .expect("arguments deserialize"),
            ))
            .await
            .expect("session is created");
    }

    let created = tools
        .new_window(Parameters(
            serde_json::from_value(serde_json::json!({"session": "one", "name": "extra"}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("window is created");
    let created = id(created);
    assert!(created.starts_with('@'));

    // The server sees three windows; each session sees only its own.
    assert_eq!(rows(tools.list_windows().await.expect("windows")).len(), 3);
    let scoped = rows(
        tools
            .list_session_windows(Parameters(
                serde_json::from_value(serde_json::json!({"session": "one"}))
                    .expect("arguments deserialize"),
            ))
            .await
            .expect("windows"),
    );
    assert_eq!(scoped.len(), 2);

    let panes = rows(
        tools
            .list_window_panes(Parameters(
                serde_json::from_value(serde_json::json!({"window": created}))
                    .expect("arguments deserialize"),
            ))
            .await
            .expect("panes"),
    );
    assert_eq!(panes.len(), 1);

    // Renaming dispatches on the id's sigil.
    let session_id = rows(tools.list_sessions().await.expect("sessions"))[0]["id"]
        .as_str()
        .expect("an id is a string")
        .to_owned();
    tools
        .rename(Parameters(
            serde_json::from_value(serde_json::json!({"target": session_id, "name": "renamed"}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("the session is renamed");
    tools
        .rename(Parameters(
            serde_json::from_value(serde_json::json!({"target": created, "name": "renamed"}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("the window is renamed");

    tools
        .kill_window(Parameters(
            serde_json::from_value(serde_json::json!({"window": created}))
                .expect("arguments deserialize"),
        ))
        .await
        .expect("the window is killed");
    assert_eq!(rows(tools.list_windows().await.expect("windows")).len(), 2);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A plan an agent could have written by hand, as JSON.
fn plan_json(session: &str) -> Value {
    let mut plan = libtmux::plan::Plan::new();
    let created = plan.add(libtmux::plan::NewSession::new(session));
    let window = plan.add(libtmux::plan::NewWindow::new(created).name("built").focus());
    plan.add(
        libtmux::plan::SendKeys::new(window.pane())
            .text("# from a plan")
            .enter(),
    );
    serde_json::to_value(&plan).expect("a plan serialises")
}

#[tokio::test]
async fn a_plan_runs_as_one_call_instead_of_one_call_per_step() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::new(guard.server().clone());

    let answer = tools
        .run_plan(Parameters(
            serde_json::from_value(serde_json::json!({
                "plan": plan_json("planned"),
                "grouping": "marked"
            }))
            .expect("arguments deserialize"),
        ))
        .await
        .expect("the plan runs");
    let view = json(answer);

    assert_eq!(view["complete"], true, "{view}");
    assert_eq!(
        view["outcomes"]
            .as_array()
            .expect("outcomes is an array")
            .len(),
        3,
    );
    // Three operations, but fewer tmux invocations: that is what the grouping
    // is for, and it is reported rather than left to be guessed.
    let dispatches = view["dispatches"].as_u64().expect("a dispatch count");
    assert!(dispatches < 3, "the plan folded: {dispatches}");

    let sessions = guard
        .server()
        .sessions()
        .await
        .expect("sessions list")
        .len();
    assert_eq!(sessions, 1, "the plan built exactly what it described");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_plan_is_refused_per_operation_when_the_tier_does_not_offer_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::builder(guard.server().clone())
        .safety(tmux_mcp::Safety::Mutating)
        .build();

    // A tool annotation describes the tool. A plan is a bag of operations, so
    // the destructive one inside it has to be caught on its own.
    let mut plan = libtmux::plan::Plan::new();
    plan.add(libtmux::plan::NewSession::new("gated"));
    plan.add(libtmux::plan::KillWindow::new(
        "@1".parse::<libtmux::WindowId>().expect("a window id"),
    ));

    let error = tools
        .run_plan(Parameters(
            serde_json::from_value(serde_json::json!({
                "plan": serde_json::to_value(&plan).expect("serialises")
            }))
            .expect("arguments deserialize"),
        ))
        .await
        .map(|_| ())
        .expect_err("a destructive step is refused at the mutating tier");

    let message = error.message.to_string();
    assert!(
        message.contains("step 1"),
        "the refusal names it: {message}"
    );
    assert!(message.contains("kill-window"), "{message}");

    // Refused before anything ran, so the session the plan would have made
    // does not exist.
    assert!(
        guard
            .server()
            .sessions()
            .await
            .expect("sessions list")
            .is_empty(),
        "a refused plan changes nothing",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
