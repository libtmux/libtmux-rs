//! Saved placements refuse withdrawal, reindexing and daemon replacement.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux_tokio::test::TestServer;
use libtmux_tokio::{Session, Window};
use serde_json::{Value, json};
use tmux_mcp::TmuxTools;

mod support;

use support::{bare_tools, call_tool, listed_window_link};

struct Fixture {
    guard: TestServer,
    tools: TmuxTools,
    source: Session,
    target: Session,
    window: Window,
}

impl Fixture {
    async fn new() -> Self {
        let guard = TestServer::new().await.expect("private daemon");
        let source = guard.session("source").await.expect("source session");
        let target = guard.session("target").await.expect("target session");
        let window = source.new_window("shared").await.expect("shared window");
        window.link_to(&target, Some(7)).await.expect("second link");
        let tools = bare_tools(guard.server());
        Self {
            guard,
            tools,
            source,
            target,
            window,
        }
    }

    async fn linked(&self) -> Window {
        self.target
            .window_at(7)
            .await
            .expect("target lookup")
            .expect("linked window")
    }

    async fn state(&self) -> Vec<(String, i32, String, bool)> {
        let rows = self
            .tools
            .list_windows()
            .await
            .expect("listed windows")
            .0
            .windows;
        let mut state: Vec<_> = rows
            .into_iter()
            .map(|row| (row.session_id, row.index, row.id, row.active))
            .collect();
        state.sort();
        state
    }
}

fn failure(result: rmcp::model::CallToolResult, kind: &str) {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let wire = serde_json::to_value(result).expect("tool error serializes");
    let body: Value =
        serde_json::from_str(wire["content"][0]["text"].as_str().expect("error text"))
            .expect("error JSON");
    assert_eq!(body["data"]["kind"], kind, "{body}");
    assert_eq!(body["data"]["retryable"], false, "{body}");
    assert_eq!(body["data"]["stale"], true, "{body}");
}

async fn refuse_every_placement_operation(fixture: &Fixture, link: &Value, kind: &str) {
    let before = fixture.state().await;
    for direction in [None, Some("next"), Some("previous"), Some("last")] {
        let mut arguments = json!({"link": link});
        if let Some(direction) = direction {
            arguments["direction"] = direction.into();
        }
        failure(
            call_tool(fixture.tools.clone(), "select_window", arguments).await,
            kind,
        );
        assert_eq!(fixture.state().await, before);
    }
    failure(call_tool(fixture.tools.clone(), "move_window", json!({
        "link": link, "destination_session": fixture.source.id().as_ref(), "destination_index": 9,
    })).await, kind);
    assert_eq!(fixture.state().await, before);
}

#[tokio::test]
async fn withdrawn_link_never_selects_or_moves_the_surviving_link() {
    let fixture = Fixture::new().await;
    let linked = fixture.linked().await;
    let saved = listed_window_link(&fixture.tools, &linked).await;
    assert_eq!(
        saved["generation"]["pid"],
        fixture.window.key().generation().pid()
    );
    linked
        .link()
        .unlink()
        .await
        .expect("withdraw intended link");
    refuse_every_placement_operation(&fixture, &saved, "link_gone").await;
    assert!(
        fixture
            .guard
            .server()
            .window_by_id(fixture.window.id())
            .await
            .unwrap()
            .is_some()
    );
    fixture
        .guard
        .shutdown()
        .await
        .expect("private daemon cleanup");
}

#[tokio::test]
async fn reindexed_window_and_reused_slot_do_not_retarget_saved_link() {
    let fixture = Fixture::new().await;
    let mut placement = fixture.linked().await.link();
    let saved = listed_window_link(&fixture.tools, placement.window()).await;
    placement
        .move_to(&fixture.target, 8)
        .await
        .expect("reindex source");
    let replacement = fixture
        .source
        .new_window("replacement")
        .await
        .expect("replacement window");
    replacement
        .link_to(&fixture.target, Some(7))
        .await
        .expect("reuse source index");
    refuse_every_placement_operation(&fixture, &saved, "link_gone").await;
    fixture
        .guard
        .shutdown()
        .await
        .expect("private daemon cleanup");
}

#[tokio::test]
async fn destroyed_window_is_distinct_from_a_withdrawn_link() {
    let fixture = Fixture::new().await;
    let saved = listed_window_link(&fixture.tools, &fixture.linked().await).await;
    fixture
        .window
        .clone()
        .kill()
        .await
        .expect("kill intrinsic window");
    refuse_every_placement_operation(&fixture, &saved, "object_gone").await;
    fixture
        .guard
        .shutdown()
        .await
        .expect("private daemon cleanup");
}

#[tokio::test]
async fn a_reference_from_another_daemon_refuses_reused_ids() {
    let original = Fixture::new().await;
    let saved = listed_window_link(&original.tools, &original.linked().await).await;
    let replacement = Fixture::new().await;
    assert_eq!(original.window.id(), replacement.window.id());
    refuse_every_placement_operation(&replacement, &saved, "server_replaced").await;
    original.guard.shutdown().await.expect("original cleanup");
    replacement
        .guard
        .shutdown()
        .await
        .expect("replacement cleanup");
}

#[tokio::test]
async fn live_link_selects_its_own_session_and_move_returns_fresh_placement() {
    let fixture = Fixture::new().await;
    let saved = listed_window_link(&fixture.tools, &fixture.linked().await).await;
    let source_before = fixture
        .source
        .active_window()
        .await
        .unwrap()
        .unwrap()
        .id()
        .clone();
    for direction in [None, Some("next"), Some("previous"), Some("last")] {
        let mut arguments = json!({"link": saved});
        if let Some(direction) = direction {
            arguments["direction"] = direction.into();
        }
        let result = call_tool(fixture.tools.clone(), "select_window", arguments).await;
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert_eq!(
            result.structured_content.unwrap()["windows"][0]["session_id"],
            fixture.target.id().as_ref()
        );
        assert_eq!(
            fixture.source.active_window().await.unwrap().unwrap().id(),
            &source_before
        );
    }
    let moved = call_tool(fixture.tools.clone(), "move_window", json!({
        "link": saved, "destination_session": fixture.target.id().as_ref(), "destination_index": 9,
    })).await;
    assert_ne!(moved.is_error, Some(true), "{moved:?}");
    let fresh = moved.structured_content.unwrap()["link"].clone();
    assert_eq!(fresh["session_id"], fixture.target.id().as_ref());
    assert_eq!(fresh["index"], 9);
    assert_eq!(fresh["window_id"], fixture.window.id().as_ref());
    refuse_every_placement_operation(&fixture, &saved, "link_gone").await;
    fixture
        .guard
        .shutdown()
        .await
        .expect("private daemon cleanup");
}
