//! Reply ownership across command expansion and independent tmux work.

#![cfg(all(feature = "control-mode", feature = "test-support"))]

macro_rules! parallel {
    ($left:expr, $right:expr $(,)?) => {
        std::thread::scope(|scope| {
            let left = scope.spawn(|| $left);
            let right = scope.spawn(|| $right);
            (
                left.join().expect("left command joins"),
                right.join().expect("right command joins"),
            )
        })
    };
    ($left:expr, $middle:expr, $right:expr $(,)?) => {
        std::thread::scope(|scope| {
            let left = scope.spawn(|| $left);
            let middle = scope.spawn(|| $middle);
            let right = scope.spawn(|| $right);
            (
                left.join().expect("left command joins"),
                middle.join().expect("middle command joins"),
                right.join().expect("right command joins"),
            )
        })
    };
}

use libtmux::control::ControlMode;
use libtmux::test::TestServer;
use libtmux::{Command, CommandChain, TmuxText};

#[test]
fn expanded_commands_keep_their_output_and_the_next_callers_reply() {
    let fixture = TestServer::new().expect("private server starts");
    let server = fixture.server();
    let session = server.new_session("correlation").expect("session");
    let empty_alias_supported = server
        .capabilities()
        .expect("tmux release is known")
        .tmux_version()
        .release()
        != Some(&libtmux::TmuxVersion::MIN_SUPPORTED);
    let setup = server
        .cmd(
            Command::new("set-option")
                .arg("-s")
                .arg("command-alias[990]")
                .arg("correlation-alias=display-message -p alias-first ; display-message -p alias-second"),
        )

        .expect("alias is configured");
    assert!(setup.success());
    assert!(
        server
            .cmd(
                Command::new("set-option")
                    .arg("-s")
                    .arg("command-alias[989]")
                    .arg("correlation-empty=")
            )
            .expect("empty alias is configured")
            .success()
    );
    let control = ControlMode::attach(server, session.id()).expect("control attaches");
    let (commands, events) = control.split();
    let mut cases = vec![
        (
            Command::new("if-shell").arg("-F").arg("1").arg(
                "display-message -p nested-first ; if-shell -F 1 'display-message -p nested-second'",
            ),
            vec![TmuxText::from("nested-first"), TmuxText::from("nested-second")],
        ),
        (
            Command::new("correlation-alias"),
            vec![TmuxText::from("alias-first"), TmuxText::from("alias-second")],
        ),
    ];
    // tmux 3.2a crashes the server while parsing an empty alias.
    if empty_alias_supported {
        cases.push((Command::new("correlation-empty"), Vec::new()));
    }
    let mut results = Vec::new();
    for (command, expected) in cases {
        let (expanded, following) = parallel!(
            commands.send(command),
            commands.send(Command::new("display-message").arg("-p").arg("following")),
        );
        results.push((expanded, following, expected));
    }
    events.shutdown().expect("control shuts down");
    fixture.shutdown().expect("fixture shuts down");
    for (expanded, following, expected) in results {
        let expanded = expanded.expect("expanded command answered");
        assert!(expanded.succeeded());
        assert_eq!(expanded.output(), expected);
        assert_eq!(
            following.expect("following command answered").output(),
            [TmuxText::from("following")],
        );
    }
}

#[test]
fn a_nested_error_survives_later_success_and_does_not_consume_a_reply() {
    let fixture = TestServer::new().expect("private server starts");
    let server = fixture.server();
    let session = server.new_session("nested-error").expect("session");
    let control = ControlMode::attach(server, session.id()).expect("control attaches");
    let (commands, events) = control.split();
    let routed = server
        .over_control_mode(&commands)
        .expect("control reaches this server");
    let chain = CommandChain::new(
        Command::new("if-shell")
            .arg("-F")
            .arg("1")
            .arg("list-panes -t %4294967294"),
    )
    .then(
        Command::new("display-message")
            .arg("-p")
            .arg("later-success"),
    );
    let (expanded, following, parse_error) = parallel!(
        routed.chain(chain),
        commands.send(Command::new("display-message").arg("-p").arg("following")),
        commands.send(Command::new("not-a-configured-user-alias")),
    );
    let recovery = commands.send(Command::new("display-message").arg("-p").arg("recovery"));
    events.shutdown().expect("control shuts down");
    fixture.shutdown().expect("fixture shuts down");
    let expanded = expanded.expect("expanded chain answered");
    assert!(!expanded.success(), "a later success cannot erase refusal");
    assert_eq!(expanded.stdout(), b"later-success\n");
    assert!(String::from_utf8_lossy(expanded.stderr()).contains("can't find pane"));
    assert_eq!(
        following.expect("following command answered").output(),
        [TmuxText::from("following")],
    );
    let parse_error = parse_error.expect("parse error is a reply");
    assert!(!parse_error.succeeded());
    assert_eq!(
        parse_error.output(),
        [TmuxText::from(
            "parse error: unknown command: not-a-configured-user-alias",
        )],
    );
    assert_eq!(
        recovery.expect("recovery command answered").output(),
        [TmuxText::from("recovery")],
    );
}

#[test]
fn reply_boundaries_ignore_display_aliases_and_do_not_run_error_hooks() {
    let fixture = TestServer::new().expect("private server starts");
    let server = fixture.server();
    let session = server.new_session("fence-aliases").expect("session");
    for (index, alias) in [
        (990, "correlation-print=display-message -p"),
        (991, "display-message=display-message -p overridden-long"),
        (992, "display=display-message -p overridden-short"),
    ] {
        let configured = server
            .cmd(
                Command::new("set-option")
                    .arg("-s")
                    .arg(format!("command-alias[{index}]"))
                    .arg(alias),
            )
            .expect("alias is configured");
        assert!(configured.success());
    }
    let error_hooks = server
        .capabilities()
        .expect("tmux release is known")
        .tmux_version()
        .meets(&libtmux::since::COMMAND_ERROR_HOOK);
    if error_hooks {
        assert!(
            server
                .cmd(
                    Command::new("set-hook")
                        .arg("-g")
                        .arg("command-error")
                        .arg("set-option -g @unexpected-error-hook yes"),
                )
                .expect("error hook is configured")
                .success(),
        );
    }
    let control = ControlMode::attach(server, session.id()).expect("control attaches");
    let result = control.send(Command::new("correlation-print").arg("literal-marker"));
    let hook = server.cmd(
        Command::new("show-options")
            .arg("-gvq")
            .arg("@unexpected-error-hook"),
    );
    control.shutdown().expect("control shuts down");
    fixture.shutdown().expect("fixture shuts down");
    let result = result.expect("aliased command answers");
    assert!(result.succeeded());
    assert_eq!(result.output(), [TmuxText::from("literal-marker")]);
    assert_eq!(hook.expect("hook state is read").stdout(), b"");
}

#[test]
fn hook_and_background_blocks_are_independent_of_queued_replies() {
    let fixture = TestServer::new().expect("private server starts");
    let server = fixture.server();
    let session = server.new_session("independent-blocks").expect("session");
    assert!(
        server
            .cmd(
                Command::new("set-hook")
                    .arg("-g")
                    .arg("after-new-window")
                    .arg("display-message -p hook-output")
            )
            .expect("hook is configured")
            .success()
    );
    let control = ControlMode::attach(server, session.id()).expect("control attaches");
    let (commands, events) = control.split();
    let (created, following) = parallel!(
        commands.send(
            Command::new("new-window")
                .arg("-d")
                .arg("-P")
                .arg("-F")
                .arg("created")
        ),
        commands.send(Command::new("display-message").arg("-p").arg("following")),
    );
    let background = commands.send(
        Command::new("if-shell")
            .arg("-b")
            .arg("true")
            .arg("display-message -p background-output ; wait-for -S background-finished"),
    );
    let finished = server
        .wait_for_channel("background-finished", std::time::Duration::from_secs(5))
        .expect("background branch finishes");
    let recovery = commands.send(Command::new("display-message").arg("-p").arg("recovery"));
    events.shutdown().expect("control shuts down");
    fixture.shutdown().expect("fixture shuts down");
    assert_eq!(
        created.expect("window command answered").output(),
        [TmuxText::from("created")]
    );
    assert_eq!(
        following.expect("following command answered").output(),
        [TmuxText::from("following")]
    );
    assert_eq!(finished, libtmux::ChannelWait::Signalled);
    assert!(
        background
            .expect("background job launched")
            .output()
            .is_empty()
    );
    assert_eq!(
        recovery.expect("recovery command answered").output(),
        [TmuxText::from("recovery")]
    );
}

#[test]
fn expanded_reply_bytes_share_one_budget() {
    let fixture = TestServer::new().expect("private server starts");
    let server = fixture.server();
    let session = server.new_session("reply-limit").expect("session");
    let control = ControlMode::attach_with_limits(
        server,
        session.id(),
        libtmux::ControlLimits::default().max_block_bytes(4),
    )
    .expect("empty attach reply fits");
    let (commands, events) = control.split();
    let result = commands.send(
        Command::new("if-shell")
            .arg("-F")
            .arg("1")
            .arg("display-message -p ab ; display-message -p cd"),
    );
    let shutdown = events.shutdown();
    fixture.shutdown().expect("fixture shuts down");
    assert!(matches!(
        result,
        Err(libtmux::Error::ControlModeFrameTooLarge {
            frame: "reply",
            limit: 4,
            ..
        })
    ));
    assert!(matches!(
        shutdown,
        Err(libtmux::Error::ControlModeFrameTooLarge {
            frame: "reply",
            limit: 4,
            ..
        })
    ));
}
