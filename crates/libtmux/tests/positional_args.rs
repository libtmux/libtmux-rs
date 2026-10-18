//! Caller text remains positional when it begins with a dash.

#![cfg(feature = "test-support")]

use libtmux::test::TestServer;
use libtmux::{
    Command, ErrorKind, NewSessionOptions, NewWindowOptions, SplitDirection, SplitOptions,
};

#[test]
fn real_tmux_compat_dash_shell_commands_are_not_flags() {
    let guard = TestServer::new().expect("tmux starts");
    let server = guard.server();
    guard.session("shell").expect("a session exists");

    let foreground = server.run_shell("-b");
    let background = server.spawn_shell("-z");
    let error = foreground.expect_err("the shell refuses -b instead of tmux accepting a flag");
    assert!(matches!(
        error.kind(),
        ErrorKind::Refused | ErrorKind::UnsupportedVersion
    ));
    background.expect("tmux accepts -z as shell text without parsing it as a flag");

    guard.shutdown().expect("tmux fixture shuts down");
}

#[test]
fn real_tmux_compat_dash_formats_and_messages_are_text() {
    let guard = TestServer::new().expect("tmux starts");
    let server = guard.server();
    let session = guard.session("formats").expect("session");
    let window = session.windows().expect("windows").remove(0);
    let pane = window.panes().expect("panes").remove(0);

    let formats = [
        server.format(Some(&pane), "-a"),
        session.format("-a"),
        window.format("-a"),
        pane.format("-a"),
    ];
    let messages = [
        session.display("-dnot-a-number"),
        window.display("-dnot-a-number"),
        pane.display("-dnot-a-number"),
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

    guard.shutdown().expect("tmux fixture shuts down");
}

#[test]
fn real_tmux_compat_dash_keys_do_not_change_binding_flags() {
    let guard = TestServer::new().expect("tmux starts");
    let server = guard.server();
    guard.session("keys").expect("session");
    // tmux 3.7 through 3.7c omit a table containing only one binding.
    for key in ["X", "Y"] {
        server
            .bind_key("dash-table", key, "display-message retained")
            .expect("a binding exists");
    }

    let bind = server.bind_key("dash-table", "-a", "display-message unwanted");
    let unbind = server.unbind_key("dash-table", "-a");
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
            .expect("the original binding remains")
            .len(),
        2,
        "-a must not remove every binding",
    );

    guard.shutdown().expect("tmux fixture shuts down");
}

#[test]
fn real_tmux_compat_dash_access_users_are_not_flags() {
    let guard = TestServer::new().expect("tmux starts");
    let server = guard.server();
    guard.session("access").expect("session");

    for result in [
        server.grant_access("-w", libtmux::AccessMode::ReadOnly),
        server.revoke_access("-w"),
    ] {
        let error = result.expect_err("-w is not a user");
        if error.kind() != ErrorKind::UnsupportedVersion {
            assert!(
                error.to_string().contains("unknown user: -w"),
                "tmux must receive the literal username: {error}",
            );
        }
    }

    guard.shutdown().expect("tmux fixture shuts down");
}

#[test]
fn real_tmux_compat_dash_creation_commands_are_not_flags() {
    let guard = TestServer::new().expect("tmux starts");
    let server = guard.server();
    guard.session("anchor").expect("session");
    let retained = server
        .cmd(
            Command::new("set-window-option")
                .arg("-g")
                .arg("remain-on-exit")
                .arg("on"),
        )
        .expect("tmux answers");
    assert!(retained.success());

    // The shell refuses -d; retaining exited panes makes its exact command
    // observable without racing the process exit against the creation reply.
    let session = server
        .new_session(NewSessionOptions::new("created").command("-d"))
        .expect("the session command is positional");
    let first = session.panes().expect("panes").remove(0);
    let window = session
        .new_window(NewWindowOptions::new("created").command("-d"))
        .expect("the window command is positional");
    let second = window.panes().expect("panes").remove(0);
    let third = window
        .split(SplitOptions::new(SplitDirection::Below).command("-d"))
        .expect("the split command is positional");
    for pane in [first, second, third] {
        assert_eq!(
            pane.format("#{pane_start_command}")
                .expect("tmux reports the command")
                .as_bytes(),
            b"-d",
        );
    }

    guard.shutdown().expect("tmux fixture shuts down");
}

#[cfg(feature = "plan")]
#[test]
fn real_tmux_compat_dash_plan_commands_are_not_flags() {
    use libtmux::plan::{NewWindow, Plan, Planner, SplitWindow};

    let guard = TestServer::new().expect("tmux starts");
    let server = guard.server();
    let session = guard.session("plans").expect("session");
    let retained = server
        .cmd(
            Command::new("set-window-option")
                .arg("-g")
                .arg("remain-on-exit")
                .arg("on"),
        )
        .expect("tmux answers");
    assert!(retained.success());

    for planner in [Planner::Sequential, Planner::Folding, Planner::Marked] {
        let mut plan = Plan::new();
        let window = plan.add(NewWindow::new(session.id().clone()).command("-d").focus());
        plan.add(SplitWindow::new(window).command("-d").focus());
        let result = plan.run(server, planner).expect("the plan runs");
        assert!(result.is_complete(), "{planner:?}: {result:?}");
        let id = result.created(0).expect("the window has an id");
        let window = session
            .windows()
            .expect("windows")
            .into_iter()
            .find(|window| window.id().as_ref() == id)
            .expect("the created window is listed");
        let panes = window.panes().expect("panes");
        assert_eq!(panes.len(), 2);
        for pane in panes {
            assert_eq!(
                pane.format("#{pane_start_command}")
                    .expect("tmux reports the command")
                    .as_bytes(),
                b"-d",
                "{planner:?}",
            );
        }
    }

    guard.shutdown().expect("tmux fixture shuts down");
}
