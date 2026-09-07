//! `paste_text` with `enter` runs the line, including in a shell that brackets
//! pastes.
//!
//! Bracketed paste exists so a terminal INSERTS what arrives rather than acting
//! on it, so a trailing newline delivered through `paste-buffer -p` is typed and
//! the command never runs -- while the tool reports success. libtmux-go shipped
//! exactly that.
//!
//! The default test shell is `/bin/sh`, which ignores the markers, so the whole
//! defect is invisible against it: the same paste that is inert in bash runs
//! fine there. This case pins the pane to bash for that reason.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use libtmux::test::TestServer;
use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _, serve_server};
use serde_json::json;
use tmux_mcp::{Selection, TmuxTools};

#[tokio::test(flavor = "multi_thread")]
async fn paste_text_with_enter_runs_the_line_in_a_bracketing_shell() {
    let Ok(bash) = which_bash() else {
        eprintln!("skipping: no bash to bracket a paste with");
        return;
    };

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("paste-enter").command(bash.as_str()))
        .await
        .expect("bash session");
    let panes = session.panes().await.expect("panes");
    let pane = panes.first().expect("a new session has a pane");

    // Without this the case is worthless: /bin/sh ignores bracketed-paste
    // markers, so the same defect that is inert under bash runs fine there and
    // the assertion below passes either way. CI's default shell is sh.
    let running = pane
        .current_command()
        .and_then(|command| command.as_str().ok().map(str::to_owned))
        .unwrap_or_default();
    assert!(
        running.contains("bash"),
        "this case needs a shell that brackets pastes; the pane runs {running:?}",
    );

    // Settle before pasting: a pane still drawing its prompt puts the prompt on
    // the same line as what follows, which reads like a missing line rather
    // than a late one.
    pane.wait_for_quiet(Duration::from_millis(300), Duration::from_secs(5))
        .await
        .expect("bash settles");

    let tools = TmuxTools::builder(guard.server().clone())
        .selection(Selection::parse(Some("execute"), None, None).expect("selection"))
        .build();
    let (client_transport, server_transport) = tokio::io::duplex(1 << 20);
    let server = tokio::spawn(async move {
        let service = serve_server(tools, server_transport)
            .await
            .expect("server starts");
        let _ = service.waiting().await;
    });
    let client: RunningService<RoleClient, ()> =
        ().serve(client_transport).await.expect("client connects");

    let request = CallToolRequestParams::new("paste_text").with_arguments(
        json!({"pane": pane.id().to_string(), "text": "printf 'RAN%s\\n' OK", "enter": true})
            .as_object()
            .cloned()
            .expect("object arguments"),
    );
    let result = client.call_tool(request).await.expect("paste_text answers");
    assert!(
        !result.is_error.unwrap_or(false),
        "paste_text refused: {result:?}"
    );

    // The tool reporting success is exactly what the bracketed-paste defect
    // does, so the assertion has to be the line's own output.
    let ran = libtmux::test::retry_until(Duration::from_secs(5), async || {
        pane.capture().await.is_ok_and(|screen| {
            screen
                .iter()
                .any(|line| line.as_str().is_ok_and(|text| text.contains("RANOK")))
        })
    })
    .await;
    let screen = pane.capture().await.unwrap_or_default();
    assert!(
        ran.is_ok(),
        "paste_text reported success but the line never ran; pane held:\n{}",
        screen
            .iter()
            .filter_map(|line| line.as_str().ok())
            .collect::<Vec<_>>()
            .join("\n"),
    );

    server.abort();
}

fn which_bash() -> Result<String, ()> {
    for candidate in ["/bin/bash", "/usr/bin/bash"] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_owned());
        }
    }
    Err(())
}
