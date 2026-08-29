//! Shared adapters for direct tool integration tests.

#![allow(dead_code)]

use std::time::Duration;

use libtmux::{Command, Server};
use rmcp::handler::server::wrapper::{Json, Parameters};
use serde_json::Value;
use tmux_mcp::{Safety, TmuxTools};

/// Build tool arguments from JSON, as the protocol delivers them.
pub(crate) fn args<T: serde::de::DeserializeOwned>(value: Value) -> Parameters<T> {
    Parameters(serde_json::from_value(value).expect("arguments deserialize"))
}

/// Render a typed answer as the structured content a client receives.
pub(crate) fn json<T: serde::Serialize>(answer: Json<T>) -> Value {
    serde_json::to_value(answer.0).expect("a tool answer serialises")
}

/// Read the id from an answer that made or destroyed one object.
pub(crate) fn id<T: serde::Serialize>(answer: Json<T>) -> String {
    json(answer)["id"]
        .as_str()
        .expect("the answer carries an id")
        .to_owned()
}

/// Build tools without inheriting the developer's tmux identity or policy.
pub(crate) fn bare_tools(server: &Server) -> TmuxTools {
    TmuxTools::builder(server.clone())
        .caller(None)
        .safety(Safety::default())
        .confirm(false)
        .build()
}

/// Wait until a pane's shell can receive input.
pub(crate) async fn prompt_ready(server: &Server, pane: &str) {
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
