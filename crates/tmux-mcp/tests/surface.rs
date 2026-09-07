//! Every advertised tool is called against a real tmux and answers the schema
//! it publishes.
//!
//! The manifest tests next door check the DECLARED surface: names, toolsets,
//! annotations, and that each schema is valid and closed. None of them invoke
//! anything. A tool can therefore be advertised, documented, schema-checked and
//! completely non-functional with every other gate green -- which is how
//! libtmux-go shipped `set_mouse_enabled` writing tmux's `mouse` option at
//! server scope, where it is a session option, so every call failed.
//!
//! Arguments are synthesised from each tool's own input schema rather than kept
//! in a hand-written table, so a new required field cannot silently go
//! unexercised. Only identifiers that must name live tmux objects are supplied
//! from the fixture.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;

use libtmux::test::TestServer;
use rmcp::model::{CallToolRequestParams, Tool};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _, serve_server};
use serde_json::{Map, Value, json};
use tmux_mcp::{Selection, TmuxTools};

/// Tools deliberately not driven here, each with the reason it cannot be.
///
/// Keep this list empty of convenience: every name here is a hole in the gate.
const NOT_DRIVEN: &[(&str, &str)] = &[(
    "call_read_tools_batch",
    "drives other tools by name, so it is covered by whichever they are",
)];

struct Wire {
    client: RunningService<RoleClient, ()>,
    _server: tokio::task::JoinHandle<()>,
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
        Self {
            client,
            _server: server,
        }
    }
}

/// A live session, window and pane for one tool to aim at and, if it is
/// destructive, to destroy without taking another tool's target with it.
struct Target {
    session: String,
    window: String,
    pane: String,
}

/// Produce a value for one required property from its schema, preferring a live
/// identifier whenever the property names a tmux object.
fn value_for(property: &str, schema: &Value, target: &Target) -> Value {
    // Match on the suffix so a second target (`swap_pane`'s `with_pane`) is
    // still recognised as needing a live identifier.
    if property.ends_with("pane") {
        return json!(target.pane);
    }
    if property.ends_with("window") {
        return json!(target.window);
    }
    if property.ends_with("session") {
        return json!(target.session);
    }
    if property == "channel" {
        return json!(format!(
            "libtmux-surface-{}",
            target.pane.trim_start_matches('%')
        ));
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return first.clone();
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("integer" | "number") => json!(1),
        Some("boolean") => json!(false),
        Some("array") => {
            // An empty array trips every minItems bound, so carry one element
            // built from the same rules.
            let items = schema.get("items").cloned().unwrap_or_else(|| json!({}));
            json!([value_for("item", &items, target)])
        }
        Some("object") => json!({}),
        _ => json!("x"),
    }
}

/// Values a schema cannot supply: tmux vocabulary, and the one-of-several
/// optional arguments some tools require in combination.
fn semantic_overrides(tool: &str, target: &Target) -> Map<String, Value> {
    let mut map = Map::new();
    let mut set = |key: &str, value: Value| {
        map.insert(key.to_owned(), value);
    };
    match tool {
        "find_pane_by_position" => set("corner", json!("top-left")),
        "resize_pane" | "select_pane" => set("direction", json!("up")),
        "select_window" => set("direction", json!("next")),
        "select_layout" => set("layout", json!("even-horizontal")),
        "respawn_pane" => set("kill_first", json!(true)),
        "send_keys" => set("text", json!("true")),
        "get_tmux_variables" => set("names", json!(["pane_id"])),
        "send_keys_batch" => set(
            "operations",
            json!([{"pane": target.pane, "text": "true", "enter": false}]),
        ),
        // The fixture holds indexes 0 and 1; move somewhere free.
        "move_window" => set("destination_index", json!(7)),
        "rename_session" => set("name", json!(format!("{}-renamed", target.session))),
        "rename_window" => set("name", json!(format!("{}-window", target.session))),
        _ => {}
    }
    map
}

fn arguments_for(tool: &Tool, target: &Target) -> Map<String, Value> {
    let schema = Value::Object((*tool.input_schema).clone());
    let required: Vec<String> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let properties = schema
        .get("properties")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut arguments = Map::new();
    for name in required {
        let property = properties.get(&name).cloned().unwrap_or_else(|| json!({}));
        arguments.insert(name.clone(), value_for(&name, &property, target));
    }
    for (key, value) in semantic_overrides(tool.name.as_ref(), target) {
        arguments.insert(key, value);
    }
    // Anything that waits must not wait long: the sweep drives 45 tools.
    for bound in ["seconds", "timeout"] {
        if arguments.contains_key(bound) {
            arguments.insert(bound.to_owned(), json!(1));
        }
    }
    arguments
}

#[tokio::test(flavor = "multi_thread")]
async fn every_advertised_tool_answers_the_schema_it_publishes() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let selection =
        Selection::parse(Some("inspect,manage,execute,teardown"), None, None).expect("selection");
    let offered = TmuxTools::builder(guard.server().clone())
        .selection(selection.clone())
        .build()
        .offered();
    assert!(!offered.is_empty(), "the manifest advertises nothing");

    let skipped: BTreeSet<&str> = NOT_DRIVEN.iter().map(|(name, _)| *name).collect();
    for (name, _) in NOT_DRIVEN {
        assert!(
            offered.iter().any(|tool| tool.name == *name),
            "{name} is exempted but no longer advertised; drop it from NOT_DRIVEN",
        );
    }

    let wire = Wire::connect(
        TmuxTools::builder(guard.server().clone())
            .selection(selection)
            .build(),
    )
    .await;

    let mut called = BTreeSet::new();
    let mut failures = Vec::new();
    for tool in &offered {
        let name = tool.name.to_string();
        if skipped.contains(name.as_str()) {
            continue;
        }

        // Its own session, so a destructive tool destroys only its own target.
        let session_name = format!("surface-{name}");
        let session = guard
            .server()
            .new_session(session_name.as_str())
            .await
            .expect("fixture session");
        // Two windows: the tools that move or select by direction have nowhere
        // to go on a session with one.
        session
            .new_window("second")
            .await
            .expect("fixture second window");
        let panes = session.panes().await.expect("fixture panes");
        let pane = panes.first().expect("a new session has a pane");
        let target = Target {
            session: session_name.clone(),
            window: pane.window_id().to_string(),
            pane: pane.id().to_string(),
        };

        let arguments = arguments_for(tool, &target);
        let request = CallToolRequestParams::new(name.clone()).with_arguments(arguments.clone());
        match wire.client.call_tool(request).await {
            Err(error) => failures.push(format!("{name}: transport error: {error}")),
            Ok(result) => {
                called.insert(name.clone());
                if result.is_error.unwrap_or(false) {
                    failures.push(format!(
                        "{name}: refused its own schema's arguments {}: {:?}",
                        Value::Object(arguments),
                        result.content,
                    ));
                    continue;
                }
                let Some(output_schema) = tool.output_schema.as_ref() else {
                    continue;
                };
                let Some(structured) = result.structured_content.as_ref() else {
                    failures.push(format!(
                        "{name}: publishes an output schema but answered none"
                    ));
                    continue;
                };
                let schema = Value::Object((**output_schema).clone());
                let validator = jsonschema::draft202012::new(&schema)
                    .expect("a published output schema compiles");
                if let Err(error) = validator.validate(structured) {
                    failures.push(format!(
                        "{name}: answer does not match its output schema: {error}"
                    ));
                }
            }
        }
    }

    let advertised: BTreeSet<String> = offered.iter().map(|tool| tool.name.to_string()).collect();
    let expected: BTreeSet<String> = advertised
        .iter()
        .filter(|name| !skipped.contains(name.as_str()))
        .cloned()
        .collect();
    let never_called: Vec<&String> = expected.difference(&called).collect();
    assert!(
        failures.is_empty() && never_called.is_empty(),
        "advertised but never called: {never_called:?}\n\n{}",
        failures.join("\n"),
    );
}
