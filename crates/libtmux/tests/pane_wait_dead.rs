//! A pane or server that ends mid-wait is the same answer, `PaneWait::Dead`.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used)]

use std::time::Duration;

use libtmux::test::TestServer;
use libtmux::{Command, NewSessionOptions, PaneWait};

async fn waiting_pane(guard: &TestServer) -> libtmux::Pane {
    let server = guard.server();
    server
        .new_session(NewSessionOptions::new("keeper").command("sleep 600"))
        .await
        .expect("keeper session");
    let session = server
        .new_session(NewSessionOptions::new("watched").command("sleep 600"))
        .await
        .expect("session");
    session.panes().await.expect("panes").remove(0)
}

#[tokio::test]
async fn a_pane_killed_mid_wait_is_dead() {
    let guard = TestServer::new().await.expect("tmux starts");
    let pane = waiting_pane(&guard).await;
    let id = pane.id().to_string();
    let server = guard.server();
    let (seen, ()) = tokio::join!(
        async {
            pane.wait_for_text("never-printed", Duration::from_secs(20))
                .await
        },
        async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            server
                .cmd(Command::new("kill-pane").arg("-t").arg(id.clone()))
                .await
                .expect("pane is killed");
        }
    );
    assert_eq!(seen.expect("a killed pane is an answer"), PaneWait::Dead);
    guard.shutdown().await.expect("fixture closes");
}

#[tokio::test]
async fn a_server_killed_mid_wait_is_dead() {
    let guard = TestServer::new().await.expect("tmux starts");
    let pane = waiting_pane(&guard).await;
    let server = guard.server();
    let (seen, ()) = tokio::join!(
        async {
            pane.wait_for_text("never-printed", Duration::from_secs(20))
                .await
        },
        async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = server.cmd(Command::new("kill-server")).await;
        }
    );
    assert_eq!(seen.expect("a killed server is an answer"), PaneWait::Dead);
    let _ = guard.shutdown().await;
}
