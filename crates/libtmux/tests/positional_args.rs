//! Caller text remains positional when it begins with a dash.

#![cfg(feature = "test-support")]

use libtmux::test::TestServer;
use libtmux::{
    Command, ErrorKind, NewSessionOptions, NewWindowOptions, SplitDirection, SplitOptions,
};

#[tokio::test]
async fn real_tmux_compat_dash_shell_commands_are_not_flags() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    guard.session("shell").await.expect("a session exists");

    let foreground = server.run_shell("-b").await;
    let background = server.spawn_shell("-z").await;
    let error = foreground.expect_err("the shell refuses -b instead of tmux accepting a flag");
    assert!(matches!(
        error.kind(),
        ErrorKind::Refused | ErrorKind::UnsupportedVersion
    ));
    background.expect("tmux accepts -z as shell text without parsing it as a flag");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_dash_formats_and_messages_are_text() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let session = guard.session("formats").await.expect("session");
    let window = session.windows().await.expect("windows").remove(0);
    let pane = window.panes().await.expect("panes").remove(0);

    let formats = [
        server.format(Some(&pane), "-a").await,
        session.format("-a").await,
        window.format("-a").await,
        pane.format("-a").await,
    ];
    let messages = [
        session.display("-dnot-a-number").await,
        window.display("-dnot-a-number").await,
        pane.display("-dnot-a-number").await,
    ];
    assert!(
        formats
            .iter()
            .all(|result| result.as_ref().is_ok_and(|text| text.as_bytes() == b"-a")),
        "a flag-shaped format must not list tmux variables",
    );
    assert!(
        messages.iter().all(Result::is_ok),
        "a flag-shaped message must not set a delay: {messages:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_dash_keys_do_not_change_binding_flags() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    guard.session("keys").await.expect("session");
    // tmux 3.7 through 3.7c omit a table containing only one binding.
    for key in ["X", "Y"] {
        server
            .bind_key("dash-table", key, "display-message retained")
            .await
            .expect("a binding exists");
    }

    let bind = server
        .bind_key("dash-table", "-a", "display-message unwanted")
        .await;
    let unbind = server.unbind_key("dash-table", "-a").await;
    for result in [bind, unbind] {
        let error = result.expect_err("-a is an invalid key, not a binding flag");
        assert!(
            error.to_string().contains("unknown key: -a"),
            "tmux must receive the literal key: {error}",
        );
    }
    assert_eq!(
        server
            .key_bindings(Some("dash-table"))
            .await
            .expect("the original binding remains")
            .len(),
        2,
        "-a must not remove every binding",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_dash_access_users_are_not_flags() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    guard.session("access").await.expect("session");

    for result in [
        server
            .grant_access("-w", libtmux::AccessMode::ReadOnly)
            .await,
        server.revoke_access("-w").await,
    ] {
        let error = result.expect_err("-w is not a user");
        if error.kind() != ErrorKind::UnsupportedVersion {
            assert!(
                error.to_string().contains("unknown user: -w"),
                "tmux must receive the literal username: {error}",
            );
        }
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn real_tmux_compat_dash_creation_commands_are_not_flags() {
    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    guard.session("anchor").await.expect("session");
    let retained = server
        .cmd(
            Command::new("set-window-option")
                .arg("-g")
                .arg("remain-on-exit")
                .arg("on"),
        )
        .await
        .expect("tmux answers");
    assert!(retained.success());

    // The shell refuses -d; retaining exited panes makes its exact command
    // observable without racing the process exit against the creation reply.
    let session = server
        .new_session(NewSessionOptions::new("created").command("-d"))
        .await
        .expect("the session command is positional");
    let first = session.panes().await.expect("panes").remove(0);
    let window = session
        .new_window(NewWindowOptions::new("created").command("-d"))
        .await
        .expect("the window command is positional");
    let second = window.panes().await.expect("panes").remove(0);
    let third = window
        .split(SplitOptions::new(SplitDirection::Below).command("-d"))
        .await
        .expect("the split command is positional");
    for pane in [first, second, third] {
        assert_eq!(
            pane.format("#{pane_start_command}")
                .await
                .expect("tmux reports the command")
                .as_bytes(),
            b"-d",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "plan")]
#[tokio::test]
async fn real_tmux_compat_dash_plan_commands_are_not_flags() {
    use libtmux::plan::{NewWindow, Plan, Planner, SplitWindow};

    let guard = TestServer::new().await.expect("tmux starts");
    let server = guard.server();
    let session = guard.session("plans").await.expect("session");
    let retained = server
        .cmd(
            Command::new("set-window-option")
                .arg("-g")
                .arg("remain-on-exit")
                .arg("on"),
        )
        .await
        .expect("tmux answers");
    assert!(retained.success());

    for planner in [Planner::Sequential, Planner::Folding, Planner::Marked] {
        let mut plan = Plan::new();
        let window = plan.add(NewWindow::new(session.id().clone()).command("-d").focus());
        plan.add(SplitWindow::new(window).command("-d").focus());
        let result = plan.run(server, planner).await.expect("the plan runs");
        assert!(result.is_complete(), "{planner:?}: {result:?}");
        let id = result.created(0).expect("the window has an id");
        let window = session
            .windows()
            .await
            .expect("windows")
            .into_iter()
            .find(|window| window.id().as_ref() == id)
            .expect("the created window is listed");
        let panes = window.panes().await.expect("panes");
        assert_eq!(panes.len(), 2);
        for pane in panes {
            assert_eq!(
                pane.format("#{pane_start_command}")
                    .await
                    .expect("tmux reports the command")
                    .as_bytes(),
                b"-d",
                "{planner:?}",
            );
        }
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}
