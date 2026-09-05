//! The frozen tool surface as an MCP client reaches it.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;

use libtmux::test::TestServer;
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams, ResourceContents};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _, serve_server};
use serde_json::{Value, json};
use tmux_mcp::{Selection, SocketProvenance, TmuxTools};

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

    async fn call(&self, name: &'static str, arguments: Value) -> rmcp::model::CallToolResult {
        let request = CallToolRequestParams::new(name)
            .with_arguments(arguments.as_object().cloned().expect("object arguments"));
        self.client
            .call_tool(request)
            .await
            .unwrap_or_else(|error| panic!("{name} failed: {error}"))
    }

    async fn shutdown(self) {
        self.client.cancel().await.expect("client shuts down");
        let _ = self.server.await;
    }
}

fn selection(toolsets: &str) -> Selection {
    Selection::parse(Some(toolsets), None, None).expect("valid selection")
}

#[tokio::test]
async fn client_sees_the_exact_cross_port_inventory() {
    let server = libtmux::Server::builder()
        .socket_name("libtmux-mcp")
        .build()
        .expect("server config");
    let tools = TmuxTools::builder(server)
        .selection(selection("inspect,manage,execute,teardown"))
        .build();
    let wire = Wire::connect(tools).await;
    let actual: BTreeSet<_> = wire
        .client
        .list_all_tools()
        .await
        .expect("tools list")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    let expected: BTreeSet<_> = [
        "list_sessions",
        "list_windows",
        "list_panes",
        "get_server_info",
        "get_session_info",
        "get_window_info",
        "get_pane_info",
        "capture_pane",
        "capture_since",
        "snapshot_pane",
        "search_panes",
        "find_pane_by_position",
        "wait_for_text",
        "get_tmux_variables",
        "show_option",
        "show_environment",
        "show_hooks",
        "call_read_tools_batch",
        "rename_session",
        "rename_window",
        "select_window",
        "select_pane",
        "select_layout",
        "resize_window",
        "resize_pane",
        "move_window",
        "swap_pane",
        "set_pane_title",
        "wait_for_channel",
        "signal_channel",
        "set_mouse_enabled",
        "set_history_limit",
        "create_session",
        "create_window",
        "split_window",
        "respawn_pane",
        "run_shell_command",
        "send_keys",
        "send_keys_batch",
        "paste_text",
        "set_synchronize_panes",
        "clear_pane_scrollback",
        "kill_pane",
        "kill_window",
        "kill_session",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();

    assert_eq!(actual, expected);
    wire.shutdown().await;
}

#[tokio::test]
async fn descriptions_annotations_and_manifest_metadata_survive_the_wire() {
    let tools = TmuxTools::builder(libtmux::Server::new().expect("server config"))
        .selection(selection("inspect,manage,execute,teardown"))
        .build();
    let wire = Wire::connect(tools).await;
    let listed = wire.client.list_all_tools().await.expect("tools list");

    for tool in listed {
        let description = tool.description.expect("controlled description");
        assert!(
            description.starts_with("Inspect tmux metadata;")
                || description.starts_with("Read pane output;")
                || description.starts_with("Read the tmux environment;")
                || description.starts_with("Read configured tmux commands;")
                || description.starts_with("Change tmux state;")
                || description.starts_with("Start a pane's configured process;")
                || description.starts_with("Send input to a pane's program;")
                || description.starts_with("Run a shell command in a pane")
                || description.starts_with("Delete tmux state;"),
            "{}: {description}",
            tool.name,
        );
        let annotations = tool.annotations.expect("whole-call annotations");
        assert!(annotations.read_only_hint.is_some(), "{}", tool.name);
        assert!(annotations.destructive_hint.is_some(), "{}", tool.name);
        assert!(annotations.idempotent_hint.is_some(), "{}", tool.name);
        assert!(annotations.open_world_hint.is_some(), "{}", tool.name);
        assert!(
            tool.meta
                .as_ref()
                .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
                .is_some(),
            "{}",
            tool.name,
        );
    }
    wire.shutdown().await;
}

#[tokio::test]
async fn read_batch_rejects_more_than_sixteen_operations() {
    let tools = TmuxTools::builder(libtmux::Server::new().expect("server config"))
        .selection(selection("inspect"))
        .build();
    let wire = Wire::connect(tools).await;
    let operations: Vec<_> = (0..17)
        .map(|_| json!({"tool": "list_sessions", "arguments": {}}))
        .collect();

    let arguments = json!({"operations": operations, "on_error": "stop"});
    let request = CallToolRequestParams::new("call_read_tools_batch")
        .with_arguments(arguments.as_object().cloned().expect("object arguments"));
    let error = wire
        .client
        .call_tool(request)
        .await
        .expect_err("seventeen operations exceed the batch limit");

    assert!(error.to_string().contains("1 through 16"), "{error}");
    wire.shutdown().await;
}

#[tokio::test]
async fn read_batch_preserves_nested_protocol_errors() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::builder(guard.server().clone())
        .selection(selection("inspect"))
        .build();
    let wire = Wire::connect(tools).await;

    let response = wire
        .call(
            "call_read_tools_batch",
            json!({
                "operations": [{
                    "tool": "get_pane_info",
                    "arguments": {"pane": "%999999"}
                }],
                "on_error": "stop"
            }),
        )
        .await;
    let structured = response
        .structured_content
        .expect("batch has structured content");
    let error = &structured["results"][0]["error"];

    assert_eq!(error["code"], -32602, "{error}");
    assert_eq!(error["message"], "no pane %999999", "{error}");
    assert_eq!(error["data"]["kind"], "object_gone", "{error}");
    assert_eq!(error["data"]["retryable"], false, "{error}");
    assert_eq!(error["data"]["stale"], true, "{error}");

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn withheld_tools_are_neither_listed_nor_callable() {
    let tools = TmuxTools::builder(libtmux::Server::new().expect("server config"))
        .selection(selection("inspect"))
        .build();
    let wire = Wire::connect(tools).await;
    let names: BTreeSet<_> = wire
        .client
        .list_all_tools()
        .await
        .expect("tools list")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();

    assert_eq!(names.len(), 18);
    assert!(!names.contains("kill_session"));
    wire.client
        .call_tool(
            CallToolRequestParams::new("kill_session")
                .with_arguments(json!({"session": "$1"}).as_object().cloned().unwrap()),
        )
        .await
        .expect_err("withheld route");
    wire.shutdown().await;
}

#[tokio::test]
async fn capabilities_resource_reports_the_effective_surface() {
    let tools = TmuxTools::builder(libtmux::Server::new().expect("server config"))
        .selection(
            Selection::parse(Some("inspect"), None, Some("capture_pane")).expect("selection"),
        )
        .build();
    let wire = Wire::connect(tools).await;
    let resource = wire
        .client
        .read_resource(ReadResourceRequestParams::new("tmux://capabilities"))
        .await
        .expect("capabilities resource");
    let ResourceContents::TextResourceContents { text, .. } = &resource.contents[0] else {
        panic!("capabilities must be text")
    };
    let report: Value = serde_json::from_str(text).expect("JSON report");
    let listed = wire.client.list_all_tools().await.expect("tools list");
    let names: BTreeSet<_> = report["tools"]
        .as_array()
        .expect("tool rows")
        .iter()
        .map(|row| row["name"].as_str().expect("tool name"))
        .collect();
    let nested: BTreeSet<_> = report["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "call_read_tools_batch")
        .expect("batch row")["nestedAuthority"]
        .as_array()
        .expect("nested authority")
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();

    assert!(!names.contains("capture_pane"));
    assert!(!nested.contains("capture_pane"));
    for tool in &listed {
        let row = report["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == tool.name.as_ref())
            .unwrap_or_else(|| panic!("{} report row", tool.name));
        let metadata = tool
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("com.git-pull.libtmux-mcp/capability"))
            .unwrap_or_else(|| panic!("{} capability metadata", tool.name));
        assert_eq!(metadata, row, "{} metadata/report", tool.name);
        assert_eq!(row["description"].as_str(), tool.description.as_deref());
        assert_eq!(
            row["inputSchema"],
            Value::Object((*tool.input_schema).clone()),
            "{} input schema",
            tool.name,
        );
        assert_eq!(
            row["outputSchema"],
            Value::Object((**tool.output_schema.as_ref().expect("typed output schema")).clone()),
            "{} output schema",
            tool.name,
        );
    }
    wire.shutdown().await;
}

#[tokio::test]
async fn aggregate_only_selection_dispatches_hidden_native_routes() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    guard
        .server()
        .new_session("nested-only")
        .await
        .expect("session");
    let selected = Selection::parse(Some(""), Some("call_read_tools_batch"), None)
        .expect("aggregate-only selection");
    let tools = TmuxTools::builder(guard.server().clone())
        .selection(selected)
        .build();
    let wire = Wire::connect(tools).await;
    let names: Vec<_> = wire
        .client
        .list_all_tools()
        .await
        .expect("tools list")
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    assert_eq!(names, ["call_read_tools_batch"]);

    let response = wire
        .call(
            "call_read_tools_batch",
            json!({
                "operations": [{"tool": "list_sessions", "arguments": {}}],
                "on_error": "stop"
            }),
        )
        .await;
    let nested = &response
        .structured_content
        .expect("batch structured result")["results"][0];
    assert_eq!(nested["success"], true, "{nested}");
    assert_eq!(
        nested["result"]["structuredContent"]["sessions"][0]["name"], "nested-only",
        "{nested}",
    );
    wire.client
        .call_tool(CallToolRequestParams::new("list_sessions"))
        .await
        .expect_err("hidden child is not directly callable");

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn capability_report_uses_the_common_socket_and_boundary_shape() {
    let server = libtmux::Server::builder()
        .socket_name("libtmux-mcp")
        .build()
        .expect("server config");
    let tools = TmuxTools::builder(server)
        .selection(selection("inspect"))
        .socket_provenance(SocketProvenance::DedicatedMinimal)
        .build();
    let wire = Wire::connect(tools).await;
    let resource = wire
        .client
        .read_resource(ReadResourceRequestParams::new("tmux://capabilities"))
        .await
        .expect("capabilities resource");
    let ResourceContents::TextResourceContents { text, .. } = &resource.contents[0] else {
        panic!("capabilities must be text")
    };
    let report: Value = serde_json::from_str(text).expect("JSON report");

    assert_eq!(report["hostCommandTools"], 0);
    assert_eq!(
        report["toolFilteringBoundary"],
        "interface-shaping-not-authorization"
    );
    assert_eq!(report["executionAuthority"], "tmux-user");
    assert_eq!(report["operatingSystemBoundary"], "none");
    assert_eq!(report["boundary"]["oneSocketPerProcess"], true);
    assert_eq!(report["boundary"]["perCallSocketSelection"], false);
    assert_eq!(report["boundary"]["hostCommandExecution"], false);
    assert_eq!(report["boundary"]["dynamicResources"], false);
    assert_eq!(report["toolCount"], 18);
    assert_eq!(report["toolsets"], json!(["inspect"]));
    assert_eq!(report["socket"]["selector"], "name:libtmux-mcp");
    assert_eq!(report["socket"]["selectionProvenance"], "default-dedicated");
    assert_eq!(report["socket"]["serverState"], "created");
    assert_eq!(report["socket"]["configurationProvenance"], "minimal");
    assert_eq!(report["socket"]["namespaceBoundary"], "tmux-objects-only");
    assert_eq!(report["connection"]["socketSelector"], "name:libtmux-mcp");
    assert_eq!(
        report["connection"]["socketProvenance"],
        "default-dedicated"
    );
    assert_eq!(report["connection"]["serverState"], "created");
    assert_eq!(report["connection"]["configurationProvenance"], "minimal");
    assert!(report["connection"]["resolvedSocketPath"].is_string());
    assert!(
        report["connection"]["attachCommand"]
            .as_str()
            .is_some_and(|command| command.contains(" -N -S ") && command.ends_with(" attach"))
    );
    wire.shutdown().await;
}

#[tokio::test]
async fn commandless_creation_runs_the_configured_process() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let tools = TmuxTools::builder(guard.server().clone())
        .selection(selection("inspect,execute,teardown"))
        .caller(None)
        .build();
    let wire = Wire::connect(tools).await;

    let created = wire.call("create_session", json!({"name": "wire"})).await;
    assert_ne!(created.is_error, Some(true));
    let listed = wire.call("list_sessions", json!({})).await;
    assert_ne!(listed.is_error, Some(true));
    let rendered = serde_json::to_string(&listed.structured_content).expect("JSON answer");
    assert!(rendered.contains("wire"), "{rendered}");

    wire.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}
