//! Window placements refuse reindexing and daemon replacement before mutation.
#![cfg(feature = "test-support")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux_tokio::test::TestServer;
use libtmux_tokio::{Command, Error, NewWindowOptions, Window, WindowLink};

async fn checked(guard: &TestServer, command: Command) {
    assert!(guard.server().cmd(command).await.unwrap().success());
}

async fn active(session: &libtmux_tokio::Session) -> Window {
    session.active_window().await.unwrap().unwrap()
}

async fn at(session: &libtmux_tokio::Session, index: i32) -> WindowLink {
    session
        .windows()
        .await
        .unwrap()
        .into_iter()
        .find(|window| window.index() == index)
        .unwrap()
        .link()
}

#[tokio::test]
async fn reindexed_source_with_reused_slot_refuses_select_move_and_swap() {
    let guard = TestServer::new().await.unwrap();
    let session = guard.session("link-source").await.unwrap();
    let destination = guard.session("link-destination").await.unwrap();
    let source = session
        .new_window(NewWindowOptions::new("source").index(4))
        .await
        .unwrap();
    let mut stale = source.link();
    checked(
        &guard,
        Command::new("move-window")
            .arg("-s")
            .arg(format!("{}:4", session.id()))
            .arg("-t")
            .arg(format!("{}:9", session.id())),
    )
    .await;
    let intruder = session
        .new_window(NewWindowOptions::new("intruder").index(4))
        .await
        .unwrap();
    let mut current = intruder.link();
    current.select().await.unwrap();
    assert!(matches!(
        stale.select().await.unwrap_err(),
        Error::LinkGone { .. }
    ));
    assert!(matches!(
        stale.move_to(&destination, 7).await.unwrap_err(),
        Error::LinkGone { .. }
    ));
    assert!(matches!(
        stale.swap_with(&current).await.unwrap_err(),
        Error::LinkGone { .. }
    ));
    assert_eq!(active(&session).await.id(), intruder.id());
    assert_eq!(at(&session, 4).await.window().id(), intruder.id());
    assert_eq!(at(&session, 9).await.window().id(), source.id());
    let mut fresh = at(&session, 9).await;
    fresh.select().await.unwrap();
    fresh.move_to(&destination, 7).await.unwrap();
    assert_eq!(fresh.snapshot().session(), destination.id());
    assert_eq!(fresh.snapshot().index(), 7);
    assert_eq!(fresh.window().id(), source.id());
    assert_eq!(fresh.window().session_id(), destination.id());
    assert_eq!(fresh.window().index(), 7);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_reindexed_swap_target_does_not_exchange_a_reused_slot() {
    let guard = TestServer::new().await.unwrap();
    let session = guard.session("swap-target").await.unwrap();
    let source = session
        .new_window(NewWindowOptions::new("source").index(4))
        .await
        .unwrap();
    let target = session
        .new_window(NewWindowOptions::new("target").index(7))
        .await
        .unwrap();
    let mut source_link = source.link();
    let stale_target = target.link();
    checked(
        &guard,
        Command::new("move-window")
            .arg("-s")
            .arg(format!("{}:7", session.id()))
            .arg("-t")
            .arg(format!("{}:9", session.id())),
    )
    .await;
    let intruder = session
        .new_window(NewWindowOptions::new("intruder").index(7))
        .await
        .unwrap();
    assert!(matches!(
        source_link.swap_with(&stale_target).await.unwrap_err(),
        Error::LinkGone { .. }
    ));
    assert_eq!(at(&session, 4).await.window().id(), source.id());
    assert_eq!(at(&session, 7).await.window().id(), intruder.id());
    let fresh_target = at(&session, 9).await;
    source_link.swap_with(&fresh_target).await.unwrap();
    assert_eq!(source_link.snapshot().index(), 9);
    assert_eq!(source_link.window().index(), 9);
    assert_eq!(at(&session, 4).await.window().id(), target.id());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn two_links_to_one_window_in_one_session_keep_their_indexes() {
    let guard = TestServer::new().await.unwrap();
    let session = guard.session("repeated-link").await.unwrap();
    let original = active(&session).await;
    original.link_to(&session, Some(7)).await.unwrap();
    checked(
        &guard,
        Command::new("select-window").arg("-t").arg(format!(
            "{}:{}",
            session.id(),
            original.index()
        )),
    )
    .await;
    assert_eq!(active(&session).await.index(), original.index());
    let mut second = at(&session, 7).await;
    second.select().await.unwrap();
    assert_eq!(active(&session).await.index(), 7);
    second.unlink().await.unwrap();
    assert_eq!(active(&session).await.id(), original.id());
    assert_eq!(active(&session).await.index(), original.index());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn selecting_a_tied_window_changes_only_the_named_session() {
    let guard = TestServer::new().await.unwrap();
    let left = guard.session("tied-left").await.unwrap();
    let right = guard.session("tied-right").await.unwrap();
    let shared = left
        .new_window(NewWindowOptions::new("shared").index(4))
        .await
        .unwrap();
    shared.link_to(&right, Some(7)).await.unwrap();
    let mut left_base = at(&left, 0).await;
    let mut right_base = at(&right, 0).await;
    left_base.select().await.unwrap();
    right_base.select().await.unwrap();
    let mut right_shared = at(&right, 7).await;
    assert_eq!(right_shared.window(), &shared);
    right_shared.select().await.unwrap();
    assert_eq!(active(&right).await.id(), shared.id());
    assert_eq!(active(&left).await.id(), left_base.window().id());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn reused_ids_from_another_generation_never_become_link_operands() {
    let old = TestServer::new().await.unwrap();
    let replacement = TestServer::new().await.unwrap();
    let old_session = old.session("old").await.unwrap();
    let new_session = replacement.session("new").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let alias = directory.path().join("s");
    std::os::unix::fs::symlink(old.socket_path(), &alias).unwrap();
    let server = libtmux_tokio::Server::builder()
        .socket_path(&alias)
        .tmux_executable(old.server().tmux_executable())
        .build()
        .unwrap();
    let before = server
        .session_by_id(old_session.id())
        .await
        .unwrap()
        .unwrap();
    let mut stale = active(&before).await.link();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(replacement.socket_path(), &alias).unwrap();
    let after = server
        .session_by_id(new_session.id())
        .await
        .unwrap()
        .unwrap();
    let fresh = active(&after).await.link();
    assert_eq!(stale.window().id(), fresh.window().id());
    assert_eq!(stale.snapshot().session(), fresh.snapshot().session());
    assert_ne!(
        stale.snapshot().window().generation(),
        fresh.snapshot().window().generation()
    );
    assert!(matches!(
        stale.move_to(&after, 7).await.unwrap_err(),
        Error::ServerGenerationChanged { .. }
    ));
    assert!(matches!(
        stale.swap_with(&fresh).await.unwrap_err(),
        Error::ServerGenerationChanged { .. }
    ));
    let intruder = after
        .new_window(NewWindowOptions::new("must-stay-active").index(4))
        .await
        .unwrap();
    intruder.link().select().await.unwrap();
    let refused = stale.select().await.unwrap_err();
    assert!(
        !refused.is_object_gone(),
        "an ID reused by another daemon is not a vanished window"
    );
    assert_eq!(active(&after).await.id(), intruder.id());
    server.shutdown().await.unwrap();
    old.shutdown().await.unwrap();
    replacement.shutdown().await.unwrap();
}

#[tokio::test]
async fn intrinsic_link_and_kill_survive_loss_of_the_discovery_session() {
    let guard = TestServer::new().await.unwrap();
    let source = guard.session("intrinsic-source").await.unwrap();
    let holding = guard.session("intrinsic-holding").await.unwrap();
    let target = guard.session("intrinsic-target").await.unwrap();
    let window = active(&source).await;
    window.link_to(&holding, Some(7)).await.unwrap();
    source.kill().await.unwrap();
    window.link_to(&target, Some(9)).await.unwrap();
    assert_eq!(at(&target, 9).await.window().id(), window.id());
    let id = window.id().clone();
    window.kill().await.unwrap();
    assert!(
        holding
            .windows()
            .await
            .unwrap()
            .iter()
            .all(|other| other.id() != &id)
    );
    assert!(
        target
            .windows()
            .await
            .unwrap()
            .iter()
            .all(|other| other.id() != &id)
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_accepted_move_classification() {
    let guard = TestServer::new().await.unwrap();
    let source = guard.session("refresh-source").await.unwrap();
    let target = guard.session("refresh-target").await.unwrap();
    let limited = libtmux_tokio::Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(guard.server().tmux_executable())
        .output_limits(libtmux_tokio::OutputLimits::default().max_stdout_bytes(4096))
        .build()
        .unwrap();
    let id = active(&source).await.id().clone();
    let mut link = limited.window_by_id(&id).await.unwrap().unwrap().link();
    checked(
        &guard,
        Command::new("rename-window")
            .arg("-t")
            .arg(id.to_string())
            .arg("x".repeat(8192)),
    )
    .await;
    let error = link.move_to(&target, 7).await.unwrap_err();
    assert!(matches!(
        error,
        Error::AfterEffect {
            operation: "move-window",
            ..
        }
    ));
    assert_eq!(at(&target, 7).await.window().id(), &id);
    limited.shutdown().await.unwrap();
    guard.shutdown().await.unwrap();
}
