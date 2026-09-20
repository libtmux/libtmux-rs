//! Integration tests for hierarchy mutations against real tmux.

#![cfg(feature = "test-support")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use libtmux::test::{TestServer, retry_until, scaled};
use libtmux::{ErrorKind, Layout, NewSessionOptions, NewWindowOptions};
use libtmux::{SplitDirection, SplitOptions, TmuxText};

#[tokio::test]
async fn layout_preflight_rejects_unsafe_saved_input_before_dispatch() {
    let guard = TestServer::new().await.unwrap();
    let session = guard.session("layout-keeper").await.unwrap();
    let mut window = session.active_window().await.unwrap().unwrap();
    let before = window.layout().to_owned();
    let error = window.select_layout("not-a-layout").await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    window.refresh().await.unwrap();
    assert_eq!(window.layout(), &before);
    assert_eq!(guard.server().sessions().await.unwrap().len(), 1);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn layout_preflight_static_inputs_need_no_executable() {
    use std::ffi::OsStr;
    let server = libtmux::Server::builder()
        .tmux_executable("/tmp/libtmux-rs-test/absent-layout-executable")
        .build()
        .unwrap();
    server
        .validate_layouts([(OsStr::new("t"), 1)])
        .await
        .unwrap();
    let error = server
        .validate_layouts([
            (OsStr::new("main-horizontal-mirrored"), 1),
            (OsStr::new("not-a-layout"), 1),
        ])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_tmux_compat_layout_preflight_uses_daemon_version() {
    use std::ffi::OsStr;
    let guard = TestServer::new().await.unwrap();
    let keeper = guard.session("layout-version-keeper").await.unwrap();
    let version = guard.server().format(None, "#{version}").await.unwrap();
    let version = libtmux::TmuxVersion::parse_output(
        format!("tmux {}\n", version.to_string_lossy()).as_bytes(),
    )
    .unwrap();
    let executable = std::env::var_os("LIBTMUX_LAYOUT_CLIENT")
        .unwrap_or_else(|| guard.server().tmux_executable().to_owned());
    let selected = libtmux::Server::builder()
        .socket_path(guard.socket_path())
        .tmux_executable(executable)
        .build()
        .unwrap();
    let client = selected.capabilities().await.unwrap().tmux_version();
    eprintln!(
        "layout version check: daemon={}, client={}",
        version.raw(),
        client.raw()
    );
    let abbreviated = selected.validate_layouts([(OsStr::new("main-h"), 1)]).await;
    let mirrored = selected
        .validate_layouts([(OsStr::new("main-horizontal-mirrored"), 1)])
        .await;
    if version.meets(&libtmux::since::MIRRORED_LAYOUTS) {
        assert_eq!(abbreviated.unwrap_err().kind(), ErrorKind::InvalidInput);
        mirrored.unwrap();
    } else {
        abbreviated.unwrap();
        assert_eq!(mirrored.unwrap_err().kind(), ErrorKind::UnsupportedVersion);
    }
    assert_eq!(selected.sessions().await.unwrap()[0].id(), keeper.id());
    selected.shutdown().await.unwrap();
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_tmux_compat_layout_preflight_cold_and_empty_daemons() {
    use std::ffi::OsStr;
    let guard = TestServer::new().await.unwrap();
    let session = guard.session("layout-empty").await.unwrap();
    let observed = guard.server().format(None, "#{version}").await.unwrap();
    let version = libtmux::TmuxVersion::parse_output(
        format!("tmux {}\n", observed.to_string_lossy()).as_bytes(),
    )
    .unwrap();
    let layout = if version.meets(&libtmux::since::MIRRORED_LAYOUTS) {
        "main-horizontal-m"
    } else {
        "main-h"
    };
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let cold = libtmux::Server::builder()
        .socket_path(directory.path().join("s"))
        .tmux_executable(guard.server().tmux_executable())
        .build()
        .unwrap();
    cold.validate_layouts([(OsStr::new(layout), 1)])
        .await
        .unwrap();
    assert!(!directory.path().join("s").exists());
    cold.shutdown().await.unwrap();
    guard
        .server()
        .cmd(
            libtmux::Command::new("set-option")
                .arg("-g")
                .arg("exit-empty")
                .arg("off"),
        )
        .await
        .unwrap();
    session.kill().await.unwrap();
    guard
        .server()
        .validate_layouts([(OsStr::new(layout), 1)])
        .await
        .unwrap();
    assert!(guard.server().sessions().await.unwrap().is_empty());
    assert_eq!(
        guard
            .server()
            .format(None, "#{pid}")
            .await
            .unwrap()
            .to_string_lossy(),
        guard.daemon_pid().to_string()
    );
    guard.shutdown().await.unwrap();
}

fn text(value: Option<&TmuxText>) -> Vec<u8> {
    value.expect("tmux reports the value").as_bytes().to_vec()
}

async fn wait_for_prompt(pane: &libtmux::Pane) {
    for _ in 0..600 {
        let lines = pane.capture().await.expect("pane captures");
        if lines
            .iter()
            .any(|line| matches!(line.as_bytes().last(), Some(b'$' | b'#')))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the pane never drew a prompt");
}

#[tokio::test]
async fn creating_an_object_returns_it_hydrated_in_one_command() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // A bare name is enough for the common case.
    let session = server
        .new_session("work")
        .await
        .expect("session is created");
    assert_eq!(session.name().as_bytes().to_vec(), b"work".to_vec());
    assert_eq!(session.window_count(), 1);
    // The handle came back populated, so no follow-up listing was needed.
    assert!(session.created() > std::time::UNIX_EPOCH);

    let window = session
        .new_window(NewWindowOptions::new("editor").command("sleep 300"))
        .await
        .expect("window is created");
    assert_eq!(window.name().as_bytes().to_vec(), b"editor".to_vec());
    assert_eq!(window.session_id(), session.id());
    assert_eq!(window.pane_count(), 1);

    let pane = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");
    assert_eq!(pane.window_id(), window.id());
    assert!(pane.pid().expect("a running pane reports a pid") > 0);

    // The server agrees with what the creating commands reported.
    assert_eq!(server.sessions().await.expect("sessions").len(), 1);
    assert_eq!(server.windows().await.expect("windows").len(), 2);
    assert_eq!(server.panes().await.expect("panes").len(), 3);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn creation_options_reach_tmux() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let directory = tempfile::tempdir().expect("temporary directory");

    let session = server
        .new_session(
            NewSessionOptions::new("configured")
                .window_name("first")
                .start_directory(directory.path())
                .size(120, 40)
                .command("sleep 300"),
        )
        .await
        .expect("session is created");

    let windows = session.windows().await.expect("windows list");
    assert_eq!(windows[0].name().as_bytes().to_vec(), b"first".to_vec());

    // tmux records the requested size as the session's `default-size`, which
    // is what this option controls and is the same on every supported
    // release. Whether a window with no client attached is then drawn at that
    // size is tmux's own business and is not: 3.2a leaves it at 80x23 where
    // 3.6 uses the default. Asserting the rendered size would be asserting
    // tmux's behaviour rather than the crate's.
    assert_eq!(
        session.typed_option("default-size").await.expect("read"),
        Some(libtmux::OptionValue::from("120x40")),
        "new-session -x -y sets it",
    );

    let panes = session.panes().await.expect("panes list");
    assert_eq!(
        text(panes[0].current_path()),
        directory
            .path()
            .canonicalize()
            .expect("canonical path")
            .as_os_str()
            .as_encoded_bytes(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_new_window_does_not_steal_selection_unless_asked() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server
        .new_session("work")
        .await
        .expect("session is created");
    let first = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");

    session
        .new_window(NewWindowOptions::new("background").command("sleep 300"))
        .await
        .expect("window is created");

    let still_first = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    assert_eq!(still_first.id(), first.id(), "creation does not select");

    let selected = session
        .new_window(
            NewWindowOptions::new("foreground")
                .command("sleep 300")
                .select(),
        )
        .await
        .expect("window is created");
    let active = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    assert_eq!(active.id(), selected.id(), "select() makes it active");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn renaming_updates_the_handle_that_performed_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut session = server
        .new_session("before")
        .await
        .expect("session is created");
    session.rename("after").await.expect("rename succeeds");
    assert_eq!(session.name().as_bytes().to_vec(), b"after".to_vec());

    let mut window = session
        .new_window(NewWindowOptions::new("old").command("sleep 300"))
        .await
        .expect("window is created");
    window.rename("new").await.expect("rename succeeds");
    assert_eq!(window.name().as_bytes().to_vec(), b"new".to_vec());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn flag_shaped_names_layouts_and_keys_stay_literal() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let mut session = server
        .new_session(NewSessionOptions::new("ordinary").command("cat"))
        .await
        .expect("session is created");
    let mut window = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    let pane = window
        .active_pane()
        .await
        .expect("active pane resolves")
        .expect("a window always has an active pane");

    session
        .rename("-literal-session")
        .await
        .expect("a flag-shaped session name stays literal");
    window
        .rename("-literal-window")
        .await
        .expect("a flag-shaped window name stays literal");
    assert_eq!(session.name().as_bytes(), b"-literal-session");
    assert_eq!(window.name().as_bytes(), b"-literal-window");

    window
        .select_layout(Layout::Tiled)
        .await
        .expect("an ordinary layout still applies");
    let error = window
        .select_layout("-literal-layout")
        .await
        .expect_err("a flag-shaped invalid layout is refused as a layout");
    assert!(
        !error.to_string().contains("unknown flag"),
        "the layout reached tmux as an operand: {error}"
    );

    pane.send_keys("-literal-text")
        .await
        .expect("flag-shaped text stays literal");
    pane.send_key_names(["Enter"])
        .await
        .expect("Enter follows literal text");
    pane.send_key_names(["-literal-key", "Enter"])
        .await
        .expect("flag-shaped key names stay operands");
    pane.send_line("-literal-line")
        .await
        .expect("a flag-shaped line stays literal");

    retry_until(Duration::from_secs(5), async || {
        pane.capture().await.is_ok_and(|lines| {
            let screen = lines
                .iter()
                .flat_map(|line| line.as_bytes().iter().copied())
                .collect::<Vec<_>>();
            [
                &b"-literal-text"[..],
                &b"-literal-key"[..],
                &b"-literal-line"[..],
            ]
            .iter()
            .all(|needle| screen.windows(needle.len()).any(|part| part == *needle))
        })
    })
    .await
    .expect("all literal input reaches the pane");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn flag_shaped_commands_and_paths_stay_literal() {
    // Without a terminator tmux reads each operand below as a flag and fails
    // with "unknown flag -z" before it ever looks at the value. Respawning a
    // bogus command fails either way, so the flag text is what is asserted,
    // not the success of the call.
    fn stayed_literal(label: &str, error: Option<libtmux::Error>) {
        let Some(error) = error else {
            return;
        };
        let message = error.to_string();
        assert!(
            !message.contains("unknown flag"),
            "{label} parsed its operand as a flag: {message}"
        );
    }

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server
        .new_session(NewSessionOptions::new("respawn").command("cat"))
        .await
        .expect("session is created");
    let mut window = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    let mut pane = window
        .active_pane()
        .await
        .expect("active pane resolves")
        .expect("a window always has an active pane");

    // pipe-pane withholds its stderr because the command is sensitive input,
    // so success is the only signal available here.
    pane.pipe(Some("-zzz-pipe"))
        .await
        .expect("a flag-shaped pipe command stays an operand");
    pane.pipe(None::<&str>)
        .await
        .expect("stopping a pipe still needs no operand");

    stayed_literal(
        "source-file",
        server.source_file("-zzz-missing").await.err(),
    );
    stayed_literal(
        "respawn-pane",
        pane.respawn(Some("-zzz-respawn"), libtmux::Respawn::Replacing)
            .await
            .err(),
    );
    stayed_literal(
        "respawn-window",
        window
            .respawn(Some("-zzz-respawn"), libtmux::Respawn::Replacing)
            .await
            .err(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A raw `-t name` target prefix-matches: tmux answers `kill-session -t
/// doom` for a session actually named `doomsday` by killing it, silently,
/// exit zero, no warning. `Session::kill` never sends a name -- it targets
/// `self.id()`, which a name never resolves to by accident -- and the
/// lookup that hands out a `Session` in the first place ([`Server::session`])
/// lists sessions and compares names as whole bytes rather than asking tmux
/// to resolve a target, so neither step can find `doomsday` while looking
/// for `doomed`.
#[tokio::test]
async fn killing_a_name_that_is_a_prefix_of_another_kills_neither() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .new_session("doomed")
        .await
        .expect("session is created");
    server
        .new_session("doomsday")
        .await
        .expect("session is created");

    assert!(
        server
            .session("doom")
            .await
            .expect("lookup succeeds")
            .is_none(),
        "a prefix of two real names must not resolve to either",
    );

    let doomed = server
        .session("doomed")
        .await
        .expect("lookup succeeds")
        .expect("the exact name is found");
    doomed.kill().await.expect("kill succeeds");

    assert!(!server.has_session("doomed").await.expect("lookup succeeds"));
    assert!(
        server
            .has_session("doomsday")
            .await
            .expect("lookup succeeds"),
        "killing doomed must not have reached doomsday",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_removes_the_object_and_strands_other_handles() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server
        .new_session("doomed")
        .await
        .expect("session is created");
    server
        .new_session("survivor")
        .await
        .expect("session is created");
    let mut stale = session.clone();

    session.kill().await.expect("kill succeeds");

    assert_eq!(server.sessions().await.expect("sessions").len(), 1);
    let error = stale.refresh().await.expect_err("a killed session is gone");
    assert!(matches!(
        error,
        libtmux::Error::ObjectGone {
            kind: libtmux::ObjectKind::Session,
            ..
        },
    ));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn unlink_removes_one_link_while_kill_removes_the_window() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let origin = server
        .new_session("origin")
        .await
        .expect("session is created");
    server
        .new_session("borrower")
        .await
        .expect("session is created");
    let shared = origin
        .new_window(NewWindowOptions::new("shared").command("sleep 300"))
        .await
        .expect("window is created");

    server
        .cmd(
            libtmux::Command::new("link-window")
                .arg("-s")
                .arg(shared.id().to_string())
                .arg("-t")
                .arg("borrower:9"),
        )
        .await
        .expect("link succeeds");

    let links: Vec<_> = server
        .windows()
        .await
        .expect("windows list")
        .into_iter()
        .filter(|window| window.id() == shared.id())
        .collect();
    assert_eq!(links.len(), 2);

    // Unlinking removes one edge; the window survives in the other session.
    links[0].clone().unlink().await.expect("unlink succeeds");
    let remaining: Vec<_> = server
        .windows()
        .await
        .expect("windows list")
        .into_iter()
        .filter(|window| window.id() == shared.id())
        .collect();
    assert_eq!(
        remaining.len(),
        1,
        "the window survives in its other session"
    );

    // Killing removes the window itself.
    remaining[0].clone().kill().await.expect("kill succeeds");
    assert!(
        server
            .windows()
            .await
            .expect("windows list")
            .iter()
            .all(|window| window.id() != shared.id()),
        "the window is gone everywhere",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_input_and_capture_round_trip_through_a_shell() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server
        .new_session(NewSessionOptions::new("io").command("sh"))
        .await
        .expect("session is created");
    let mut pane = session
        .panes()
        .await
        .expect("panes list")
        .into_iter()
        .next()
        .expect("one pane");

    pane.send_keys("printf marker-8fa1\n")
        .await
        .expect("keys are sent");

    // Wait for the shell to produce the output rather than sleeping.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let captured = loop {
        let lines = pane.capture().await.expect("capture succeeds");
        if lines.iter().any(|line| {
            line.as_bytes()
                .windows(11)
                .any(|window| window == b"marker-8fa1")
        }) {
            break lines;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the shell did not echo the marker before the deadline",
        );
        tokio::task::yield_now().await;
    };
    assert!(!captured.is_empty());

    // A sole pane fills its window, so there is nothing for a resize to take
    // space from. Split first, then narrow the original.
    let window = pane
        .window()
        .await
        .expect("parent resolves")
        .expect("the pane's window exists");
    window
        .split(SplitOptions::new(SplitDirection::Right).command("sleep 300"))
        .await
        .expect("pane is created");

    pane.refresh().await.expect("the pane still exists");
    let full_width = pane.width();
    pane.resize(20, 8).await.expect("resize succeeds");

    // The handle reports what tmux did, not what was requested.
    assert_eq!(pane.width(), 20);
    assert!(full_width > 20, "the split left room to shrink into");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn cancelling_a_line_send_cannot_leave_enter_undispatched() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server
        .new_session("line-cancellation")
        .await
        .expect("session is created");
    let pane = session.panes().await.expect("panes list").remove(0);
    wait_for_prompt(&pane).await;

    let accepted = "send-line-accepted";
    let release = "send-line-release";
    let ran = "send-line-ran";
    session
        .set_hook(
            "after-send-keys",
            format!(
                "if-shell -F '#{{==:#{{hook_flag_l}},1}}' \
                 'wait-for -S {accepted}; wait-for {release}'"
            ),
        )
        .await
        .expect("the dispatch gate is installed");

    let sending = tokio::spawn({
        let pane = pane.clone();
        async move { pane.send_line(format!("tmux wait-for -S {ran}")).await }
    });
    assert_eq!(
        server
            .wait_for_channel(accepted, scaled(Duration::from_secs(5)))
            .await
            .expect("the send can signal"),
        libtmux::ChannelWait::Signalled,
        "the line did not reach tmux",
    );

    sending.abort();
    assert!(
        sending
            .await
            .expect_err("the line send is cancelled")
            .is_cancelled(),
    );
    server
        .signal_channel(release)
        .await
        .expect("the send is released");

    assert_eq!(
        server
            .wait_for_channel(ran, scaled(Duration::from_secs(1)))
            .await
            .expect("the command signal can be read"),
        libtmux::ChannelWait::Signalled,
        "cancellation stranded the literal text without Enter",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_line_send_preserves_adversarial_literal_text() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server
        .new_session("literal-line")
        .await
        .expect("session is created");
    let pane = session.panes().await.expect("panes list").remove(0);
    wait_for_prompt(&pane).await;

    pane.send_keys(
        r#"stty -echo; printf 'reader-ready\n'; while IFS= read -r line; do printf 'got:<%s>\n' "$line"; done"#,
    )
    .await
    .expect("the reader is typed");
    pane.send_key_names(["Enter"])
        .await
        .expect("the reader is started");
    assert_eq!(
        pane.wait_for_text("reader-ready", Duration::from_secs(5))
            .await
            .expect("the reader can be watched"),
        libtmux::PaneWait::Arrived,
    );

    for payload in [
        "C-c Enter -l",
        r#"; \; '#{pane_id}' "$()" `x`;"#,
        "unicode-🦀-空",
    ] {
        pane.send_line(payload).await.expect("the line is sent");
        let expected = format!("got:<{payload}>");
        assert_eq!(
            pane.wait_for_text(&expected, Duration::from_secs(5))
                .await
                .expect("the reader can be watched"),
            libtmux::PaneWait::Arrived,
        );
        assert!(
            pane.capture()
                .await
                .expect("the pane captures")
                .iter()
                .any(|line| line.as_bytes() == expected.as_bytes()),
            "the captured line differs from the payload: {payload:?}",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_line_send_redacts_input_and_classifies_a_gone_pane() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("line-error")
        .await
        .expect("session is created");
    let window = session
        .active_window()
        .await
        .expect("the window resolves")
        .expect("a session has a window");
    let pane = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("a second pane is created");
    let stale = pane.clone();
    pane.kill().await.expect("the pane is killed");

    let secret = "sentinel-line-secret";
    let error = stale.send_line(secret).await.expect_err("the pane is gone");

    assert_eq!(error.kind(), ErrorKind::ObjectGone);
    let diagnostic = format!("{error:?} {error}");
    assert!(!diagnostic.contains(secret), "{diagnostic}");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn killing_the_server_leaves_nothing_running() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server().clone();

    server
        .new_session("work")
        .await
        .expect("session is created");
    assert!(server.is_alive().await);

    server.kill().await.expect("kill-server succeeds");
    assert!(!server.is_alive().await);
    // Killing an already-dead server is the state the caller asked for.
    server.kill().await.expect("kill-server is idempotent");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_operations_reshape_the_hierarchy() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server.new_session("shaping").await.expect("session");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");
    let second = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");
    let mut first = window
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    // A title belongs to the pane, and refreshing shows tmux's own view.
    first.set_title("named").await.expect("title is set");
    assert_eq!(first.title().as_bytes(), b"named",);

    first.clear_history().await.expect("history is cleared");

    // Swapping reports positions from tmux, not from the request.
    let before = first.index();
    first.swap_with(&second).await.expect("panes are swapped");
    assert_ne!(first.index(), before, "the pane moved");

    // Breaking a pane out gives it a window of its own.
    let windows_before = session.windows().await.expect("windows").len();
    second.break_out().await.expect("pane is broken out");
    assert_eq!(
        session.windows().await.expect("windows").len(),
        windows_before + 1,
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn window_operations_move_and_resize() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("moving").await.expect("session");
    let destination = server
        .new_session("moved-into")
        .await
        .expect("destination session");

    let mut window = session
        .new_window(NewWindowOptions::new("mover").command("sleep 300"))
        .await
        .expect("window is created");

    window
        .move_to(&destination, 20)
        .await
        .expect("window is moved");
    assert_eq!(window.index(), 20);
    assert_eq!(window.session_id(), destination.id());
    assert!(
        !session
            .windows()
            .await
            .expect("source windows")
            .iter()
            .any(|candidate| candidate.id() == window.id()),
    );
    assert!(
        destination
            .windows()
            .await
            .expect("destination windows")
            .iter()
            .any(|candidate| candidate.id() == window.id()),
    );

    window.resize(100, 30).await.expect("window is resized");
    assert_eq!(window.width(), 100);
    assert_eq!(window.height(), 30);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn handle_mutations_reject_another_server() {
    use libtmux::{Error, ErrorKind, JoinOptions};

    macro_rules! error_kind {
        ($future:expr) => {
            match $future.await {
                Err(error @ Error::ServerMismatch { .. }) => error.kind(),
                Err(error) => panic!("foreign handle returned {error:?}"),
                Ok(_) => panic!("foreign handle was accepted"),
            }
        };
    }

    let left_guard = TestServer::builder().start().await.expect("tmux starts");
    let right_guard = TestServer::builder().start().await.expect("tmux starts");
    let left = left_guard.server();
    let right = right_guard.server();
    let left_session = left.new_session("left").await.expect("left session");
    let right_session = right.new_session("right").await.expect("right session");
    let mut left_window = left_session
        .active_window()
        .await
        .expect("left window lookup")
        .expect("left window");
    let right_window = right_session
        .active_window()
        .await
        .expect("right window lookup")
        .expect("right window");
    let mut left_pane = left_window.panes().await.expect("left panes").remove(0);
    let right_pane = right_window.panes().await.expect("right panes").remove(0);

    assert_eq!(left_session.id(), right_session.id());
    assert_eq!(left_window.id(), right_window.id());
    assert_eq!(left_pane.id(), right_pane.id());
    let original_index = left_window.index();

    let kinds = [
        error_kind!(left_window.swap_with(&right_window)),
        error_kind!(left_window.link_to(&right_session, Some(29))),
        error_kind!(left_window.move_to(&right_session, 30)),
        error_kind!(left_pane.swap_with(&right_pane)),
        error_kind!(
            left_pane
                .clone()
                .join_into(&right_pane, JoinOptions::new(SplitDirection::Below))
        ),
    ];

    assert_eq!(kinds, [ErrorKind::InvalidInput; 5]);
    assert_eq!(
        left_session
            .windows()
            .await
            .expect("left windows")
            .first()
            .expect("left window remains")
            .index(),
        original_index,
    );

    right_guard
        .shutdown()
        .await
        .expect("right tmux fixture shuts down");
    left_guard
        .shutdown()
        .await
        .expect("left tmux fixture shuts down");
}

#[tokio::test]
async fn pane_window_follows_an_external_move() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("pane-moved").await.expect("session");
    let source = session
        .active_window()
        .await
        .expect("source lookup")
        .expect("source window");
    let moving = source
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("moving pane");
    let destination_session = server
        .new_session(NewSessionOptions::new("pane-destination").command("sleep 300"))
        .await
        .expect("destination session");
    let destination = destination_session
        .active_window()
        .await
        .expect("destination lookup")
        .expect("destination window");
    let beside = destination
        .panes()
        .await
        .expect("destination panes")
        .remove(0);

    let moved = server
        .cmd(
            libtmux::Command::new("join-pane")
                .arg("-d")
                .arg("-s")
                .arg(moving.id().to_string())
                .arg("-t")
                .arg(beside.id().to_string()),
        )
        .await
        .expect("join-pane runs");
    assert!(moved.success(), "join-pane succeeds: {moved:?}");

    let current = moving
        .window()
        .await
        .expect("window lookup")
        .expect("the pane still has a window");
    assert_eq!(current.id(), destination.id());
    assert_eq!(current.session_id(), destination_session.id());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_session_environment_is_set_read_and_removed() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("environed").await.expect("session");

    assert!(
        session.environment("EDITOR").await.expect("read").is_none(),
        "a variable nobody set is absent rather than empty",
    );

    session
        .set_environment("EDITOR", "hx")
        .await
        .expect("the variable is set");
    assert!(matches!(
        session.environment("EDITOR").await.expect("read"),
        Some(libtmux::EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
    ));

    // A session variable reaches processes the session starts from now on,
    // not the ones already running.
    let window = session
        .new_window(NewWindowOptions::new("later").command("sleep 300"))
        .await
        .expect("window");
    let pane = window.panes().await.expect("panes").remove(0);
    let seen = server
        .format(Some(&pane), "#{?#{==:#{EDITOR},},unset,set}")
        .await
        .expect("a format expands");
    assert_eq!(seen.as_bytes(), b"set");

    session
        .unset_environment("EDITOR")
        .await
        .expect("the variable is removed");
    assert!(session.environment("EDITOR").await.expect("read").is_none());

    // Hiding is not unsetting: tmux keeps an entry marked so a process
    // started here does not inherit the name, which is a third state that
    // reporting absence would hide.
    session
        .set_environment("PAGER", "less")
        .await
        .expect("the variable is set");
    session
        .hide_environment("PAGER")
        .await
        .expect("the variable is hidden");
    assert_eq!(
        session.environment("PAGER").await.expect("read"),
        Some(libtmux::EnvironmentEntry::Removed),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_window_linked_into_two_sessions_is_one_window() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let source = server.new_session("linking-from").await.expect("session");
    let target = server.new_session("linking-to").await.expect("session");

    let mut window = source
        .new_window(NewWindowOptions::new("shared").command("sleep 300"))
        .await
        .expect("window");

    window
        .link_to(&target, None)
        .await
        .expect("the window is linked");

    // One window, two winlinks: the id is the same on both sides, and the
    // link reports itself as linked rather than as a copy.
    let linked = target
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .find(|other| other.id() == window.id())
        .expect("the link exists in the target session");
    assert!(linked.is_linked());

    // Renaming through one link is visible through the other, which a copy
    // would not be.
    window
        .rename("renamed")
        .await
        .expect("the window is renamed");
    assert_eq!(
        linked
            .refreshed()
            .await
            .expect("the link still exists")
            .name()
            .as_bytes(),
        b"renamed",
    );

    let destination = server
        .new_session("linking-destination")
        .await
        .expect("destination session");
    let selected = server
        .cmd(
            libtmux::Command::new("select-window")
                .arg("-t")
                .arg(format!("{}:{}", target.id(), window.id())),
        )
        .await
        .expect("target link is selected");
    assert!(selected.success());

    window
        .move_to(&destination, 9)
        .await
        .expect("the source link moves");
    assert!(
        !source
            .windows()
            .await
            .expect("source windows")
            .iter()
            .any(|other| other.id() == window.id()),
    );
    assert!(
        target
            .windows()
            .await
            .expect("target windows")
            .iter()
            .any(|other| other.id() == window.id()),
        "the other link remains",
    );

    // Unlinking removes one winlink, and the window survives in the other.
    linked.unlink().await.expect("the link is removed");
    assert!(
        !target
            .windows()
            .await
            .expect("windows")
            .iter()
            .any(|other| other.id() == window.id()),
    );
    assert!(
        destination
            .windows()
            .await
            .expect("windows")
            .iter()
            .any(|other| other.id() == window.id()),
        "the window itself is still there, only the second link is gone",
    );

    // An index another window already holds is a refusal, not an overwrite.
    let occupied = target
        .windows()
        .await
        .expect("windows")
        .first()
        .expect("a window")
        .index();
    assert!(window.link_to(&target, Some(occupied)).await.is_err());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn objects_are_found_by_their_tmux_identity() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("identified").await.expect("session");
    let window = session
        .new_window(NewWindowOptions::new("found").command("sleep 300"))
        .await
        .expect("window");
    let pane = window.panes().await.expect("panes").remove(0);

    assert_eq!(
        server
            .session_by_id(session.id())
            .await
            .expect("the lookup runs")
            .expect("the session exists")
            .id(),
        session.id(),
    );
    assert_eq!(
        server
            .window_by_id(window.id())
            .await
            .expect("the lookup runs")
            .expect("the window exists")
            .id(),
        window.id(),
    );
    assert_eq!(
        server
            .pane_by_id(pane.id())
            .await
            .expect("the lookup runs")
            .expect("the pane exists")
            .id(),
        pane.id(),
    );

    // An id tmux does not have is absent rather than an error: it is an
    // ordinary answer to "is this still here".
    let gone = window.id().clone();
    window.kill().await.expect("the window is killed");
    assert!(
        server
            .window_by_id(&gone)
            .await
            .expect("the lookup runs")
            .is_none(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_failure_says_what_to_do_about_it() {
    use libtmux::{ErrorKind, Server};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("classified").await.expect("session");

    // An object that has gone is the branch a caller writes most: it means
    // look again or create, not that the request was wrong.
    let window = session
        .new_window(NewWindowOptions::new("doomed").command("sleep 300"))
        .await
        .expect("window");
    let mut stale = window.clone();
    window.kill().await.expect("the window is killed");

    let gone = stale
        .rename("gone")
        .await
        .expect_err("the window is killed");
    assert_eq!(gone.kind(), ErrorKind::ObjectGone);
    assert!(gone.is_object_gone());
    assert!(!gone.is_transient(), "looking again will not bring it back");

    // Every object kind reports the same way, through whichever command the
    // caller happened to run.
    let pane = session.panes().await.expect("panes").remove(0);
    let stale_pane = pane.clone();
    let gone_session = session.clone();
    session.kill().await.expect("the session is killed");

    for (kind, error) in [
        (
            ErrorKind::ObjectGone,
            stale_pane.capture().await.expect_err("the pane is gone"),
        ),
        (
            ErrorKind::ObjectGone,
            gone_session
                .windows()
                .await
                .expect_err("the session is gone"),
        ),
    ] {
        assert_eq!(error.kind(), kind, "{error}");
    }

    // Not everything reports a missing target. tmux expands a format against
    // one to nothing and exits zero, so this records what tmux does rather
    // than what would be tidier.
    assert!(
        server
            .format(Some(&stale_pane), "#{pane_id}")
            .await
            .expect("tmux does not refuse a format against a target that has gone")
            .as_bytes()
            .is_empty(),
    );

    // tmux answering "no" is a different thing: the arguments were wrong.
    let refused = server
        .delete_buffer("never-existed")
        .await
        .expect_err("tmux refuses to delete a buffer it does not have");
    assert_eq!(refused.kind(), ErrorKind::Refused);
    assert!(
        !refused.is_transient(),
        "tmux will answer the same way again"
    );

    // tmux not being runnable at all is neither: nothing about the request
    // will change it.
    let absent = Server::builder()
        .tmux_executable("tmux-that-is-not-installed")
        .socket_path("/tmp/libtmux-rs-unreachable.sock")
        .build()
        .expect("the configuration is valid")
        .sessions()
        .await
        .expect_err("there is no such executable");
    assert_eq!(absent.kind(), ErrorKind::Unreachable);
    assert!(!absent.is_transient());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_name_with_ambiguous_separators_is_rejected_before_creation() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // tmux splits a:b into session prefix a and window prefix b. An implicit
    // window named bash would match, so choose a nonmatching window name.
    server
        .cmd(
            libtmux::Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg("a:b")
                .arg("-n")
                .arg("other-window"),
        )
        .await
        .expect("tmux accepts the name");

    let found = server
        .cmd(libtmux::Command::new("has-session").arg("-t").arg("a:b"))
        .await
        .expect("the command runs");
    assert!(
        !found.success(),
        "the separator makes lookup depend on the window name"
    );
    assert!(
        found.stderr_lossy().contains("can't find window"),
        "it split the name at the colon: {:?}",
        found.stderr_lossy(),
    );

    assert_eq!(server.sessions().await.expect("sessions").len(), 1);

    assert!(libtmux::SessionName::new("a:b").is_err());
    assert!(libtmux::SessionName::new("c.d").is_err());
    assert!(libtmux::SessionName::new("").is_err());
    assert_eq!(
        libtmux::SessionName::new("has,comma")
            .expect("a comma is addressable")
            .as_str(),
        "has,comma",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_taken_session_name_is_classified_rather_than_a_bare_refusal() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server.new_session("taken").await.expect("first session");

    // "pick another name" is the one creation failure a caller routinely
    // handles, so it arrives as its own variant rather than as text to
    // match on. Checking with has-session first would race: another process
    // can take the name between the check and the create.
    let error = server
        .new_session("taken")
        .await
        .map(|_| ())
        .expect_err("tmux refuses a duplicate name");

    assert!(
        matches!(&error, libtmux::Error::SessionExists { name } if name == "taken"),
        "the refusal names what was taken: {error:?}",
    );
    assert_eq!(error.kind(), ErrorKind::Refused);

    // The first session is untouched by the refusal.
    assert_eq!(server.sessions().await.expect("sessions").len(), 1);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn directional_focus_follows_the_layout_and_wraps_at_the_edge() {
    use libtmux::{NewWindowOptions, PaneDirection, SplitDirection};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("directions")
        .await
        .expect("session");

    // Two panes, one above the other. Kept to two because `split` divides
    // whichever pane is active, and splitting is detached by default, so a
    // third pane's position depends on focus rather than on the call.
    let stacked = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");
    let mut top = stacked
        .active_pane()
        .await
        .expect("panes")
        .expect("a window has a pane");
    let bottom = stacked
        .split(SplitDirection::Below)
        .await
        .expect("split below");

    top.select().await.expect("focus the top pane");
    assert_eq!(
        stacked
            .focus_direction(PaneDirection::Below)
            .await
            .expect("focus moves")
            .id(),
        bottom.id(),
        "down from the top",
    );
    assert_eq!(
        stacked
            .focus_direction(PaneDirection::Above)
            .await
            .expect("focus moves")
            .id(),
        top.id(),
        "up from the bottom",
    );

    // tmux wraps rather than refusing, so this lands on the other pane
    // instead of reporting that there is nothing above.
    assert_eq!(
        stacked
            .focus_direction(PaneDirection::Above)
            .await
            .expect("the edge is not an error")
            .id(),
        bottom.id(),
        "up from the top wraps to the bottom",
    );

    // Side by side, in a window of its own for the same reason.
    let side = session
        .new_window(NewWindowOptions::new("side").command("sleep 300"))
        .await
        .expect("window created");
    let mut left = side
        .active_pane()
        .await
        .expect("panes")
        .expect("a window has a pane");
    let right = side
        .split(SplitDirection::Right)
        .await
        .expect("split right");

    left.select().await.expect("focus the left pane");
    assert_eq!(
        side.focus_direction(PaneDirection::Right)
            .await
            .expect("focus moves")
            .id(),
        right.id(),
        "right from the left",
    );
    assert_eq!(
        side.focus_direction(PaneDirection::Left)
            .await
            .expect("focus moves")
            .id(),
        left.id(),
        "left from the right",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A layout is named, saved, or stepped through, and each is a different thing.
///
/// tmux takes a name it computes an arrangement from, a string it produced
/// earlier that restores pane sizes exactly, or a step through its own list.
/// The step is a flag rather than a name, so `select-layout next` is a refusal
/// and nothing that takes a layout name could ever express it.
#[tokio::test]
async fn a_layout_is_named_saved_or_stepped_through() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("layouts").await.expect("session");
    let mut window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");

    // Two panes, so the arrangements differ from each other.
    window
        .split(SplitOptions::new(SplitDirection::Below))
        .await
        .expect("a second pane");

    window
        .select_layout(Layout::EvenHorizontal)
        .await
        .expect("tmux arranges the panes");
    let horizontal = window.layout().to_owned();

    window
        .select_layout(Layout::EvenVertical)
        .await
        .expect("tmux arranges the panes");
    let vertical = window.layout().to_owned();
    assert_ne!(
        horizontal.as_bytes(),
        vertical.as_bytes(),
        "the two named layouts arrange two panes differently",
    );

    // A saved layout restores the exact arrangement, which is what a name
    // cannot do: it carries the pane sizes rather than a rule for them.
    window
        .select_layout(&horizontal)
        .await
        .expect("tmux restores the saved layout");
    assert_eq!(window.layout().as_bytes(), horizontal.as_bytes());

    // Stepping is a flag on the same command, so it is reachable only as its
    // own method. Passing "next" as a name is what tmux refuses.
    assert!(
        window.select_layout("next").await.is_err(),
        "`next` is a flag rather than a layout name",
    );
    // Where a step lands is tmux's own list order, which this does not assert:
    // that each step moves is the contract, and a saved layout is not
    // necessarily a position in that list to come back to.
    window.next_layout().await.expect("tmux steps forward");
    let stepped = window.layout().to_owned();
    assert_ne!(stepped.as_bytes(), horizontal.as_bytes());

    window.previous_layout().await.expect("tmux steps back");
    assert_ne!(window.layout().as_bytes(), stepped.as_bytes());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A saved layout with more than two panes restores each pane's own position
/// only where tmux reports the layout as JSON.
///
/// `LayoutSpec::Saved`'s doc says a saved string restores "the exact
/// arrangement, including which process ended up where" only from
/// `since::JSON_LAYOUTS` (3.8) onward; below it, the classic checksum-prefixed
/// string reconstructs the shape but can hand two same-sized cells to each
/// other's panes. This is tmux's own limitation, matching what the Python
/// and Go ports found for a saved layout's pane identity, pinned here
/// per-version rather than asserted as always true or always false.
#[tokio::test]
async fn real_tmux_compat_saved_layout_pane_identity_needs_json() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server
        .new_session("layout-identity")
        .await
        .expect("session");
    let mut window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");

    for _ in 0..3 {
        window
            .split(SplitOptions::new(SplitDirection::Below))
            .await
            .expect("a pane is added");
    }
    // Mirrored presets arrived in 3.5; the unmirrored one is asymmetric too.
    let asymmetric = if server
        .capabilities()
        .await
        .expect("capabilities read")
        .tmux_version()
        .has_behavior(&libtmux::since::MIRRORED_LAYOUTS)
    {
        Layout::MainVerticalMirrored
    } else {
        Layout::MainVertical
    };
    window
        .select_layout(asymmetric)
        .await
        .expect("tmux arranges four panes asymmetrically");

    let saved = window.layout().to_owned();
    let before: Vec<(i32, i32, String)> = window
        .panes()
        .await
        .expect("panes list")
        .iter()
        .map(|pane| (pane.left(), pane.top(), pane.id().to_string()))
        .collect();

    window
        .select_layout(Layout::Tiled)
        .await
        .expect("tmux rearranges the panes");
    window
        .select_layout(&saved)
        .await
        .expect("tmux restores the saved layout");

    let after: Vec<(i32, i32, String)> = window
        .panes()
        .await
        .expect("panes list")
        .iter()
        .map(|pane| (pane.left(), pane.top(), pane.id().to_string()))
        .collect();

    let json_capable = server
        .capabilities()
        .await
        .expect("capabilities read")
        .tmux_version()
        .has_behavior(&libtmux::since::JSON_LAYOUTS);

    if json_capable {
        assert_eq!(
            window.layout().as_bytes(),
            saved.as_bytes(),
            "a JSON layout restores byte-exact, pane ids included",
        );
        assert_eq!(
            after, before,
            "each pane returns to the exact cell it saved",
        );
    } else {
        // Below 3.8 the shape always comes back; whether each pane's own id
        // returns to its own cell depends on whether the live pane-list order
        // happens to match the saved cell order, which this does not assert
        // either way -- only that the crate does not overclaim it for this
        // arrangement, which mirrored layouts are chosen to stress. Compared
        // as a multiset, since which pane reports which coordinate first is
        // exactly what is not being asserted here.
        let mut before_shape: Vec<(i32, i32)> =
            before.iter().map(|(left, top, _)| (*left, *top)).collect();
        let mut after_shape: Vec<(i32, i32)> =
            after.iter().map(|(left, top, _)| (*left, *top)).collect();
        before_shape.sort_unstable();
        after_shape.sort_unstable();
        assert_eq!(
            after_shape, before_shape,
            "the geometry is restored either way"
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A value that is not a layout is refused before it reaches tmux.
///
/// tmux 3.3 and 3.3a exit on a layout `select-layout` cannot parse, taking
/// every session on the socket with them, and `--` alone turns `-o` from the
/// undo flag into exactly such a value. The session surviving is what fails
/// there without the refusal.
#[tokio::test]
async fn a_value_that_is_not_a_layout_is_refused_before_dispatch() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("not-a-layout").await.expect("session");
    let mut window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");

    for value in ["-o", "garbage", "", "next", "0000", "zzzz,80x24,0,0,0"] {
        let error = window
            .select_layout(value)
            .await
            .expect_err("a value that is not a layout is refused");
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{value:?}: {error}");
    }
    assert!(
        server
            .has_session("not-a-layout")
            .await
            .expect("tmux still answers"),
        "the session survives every refused value",
    );
    window
        .select_layout("tiled")
        .await
        .expect("a preset name passed as text still applies");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A unique preset prefix applies; a prefix naming more than one preset is
/// refused, naming the candidates.
///
/// tmux's own `layout_set_lookup` is a prefix match, so `tile` and `even-h`
/// apply on every release and never reach the 3.3a crash path -- an
/// exact-match-only guard refuses a spelling tmux itself accepts.
#[tokio::test]
async fn a_unique_layout_prefix_applies_an_ambiguous_one_names_its_candidates() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("layout-prefix").await.expect("session");
    let mut window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");
    window
        .split(SplitOptions::new(SplitDirection::Right))
        .await
        .expect("a second pane to lay out");

    window
        .select_layout("tile")
        .await
        .expect("a unique prefix of `tiled` applies");
    window
        .select_layout("even-h")
        .await
        .expect("a unique prefix of `even-horizontal` applies");

    let error = window
        .select_layout("even-")
        .await
        .expect_err("a prefix naming two presets is ambiguous");
    assert_eq!(error.kind(), ErrorKind::InvalidInput, "{error}");
    let message = error.to_string();
    assert!(message.contains("even-horizontal"), "{message}");
    assert!(message.contains("even-vertical"), "{message}");

    // Empty is not a prefix of "every preset": it stays the same refusal as
    // every other unrecognized value, not "ambiguous among all seven".
    let empty_error = window
        .select_layout("")
        .await
        .expect_err("empty is refused, not ambiguous");
    assert!(
        !empty_error.to_string().contains("more than one preset"),
        "{empty_error}",
    );

    // `main-h` is unique among the presets tmux 3.2a knows (five) and
    // ambiguous once the mirrored pair exists (3.5+) -- the candidate set is
    // the running release's, not every preset this crate can name.
    let mirrored_capable = server
        .capabilities()
        .await
        .expect("capabilities read")
        .tmux_version()
        .has_behavior(&libtmux::since::MIRRORED_LAYOUTS);
    let main_h = window.select_layout("main-h").await;
    if mirrored_capable {
        let error = main_h.expect_err("main-h is ambiguous once the mirrored pair exists");
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{error}");
    } else {
        main_h.expect("main-h is unique without the mirrored pair");
    }

    assert!(
        server
            .has_session("layout-prefix")
            .await
            .expect("tmux still answers"),
        "the session survives every prefix, unique or ambiguous",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A JSON-looking saved layout on an old tmux is refused with a version
/// hint, not just tmux's generic "invalid layout".
#[tokio::test]
async fn a_json_layout_on_an_old_tmux_names_the_version_it_needs() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("json-layout").await.expect("session");
    let mut window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");

    let json_capable = server
        .capabilities()
        .await
        .expect("capabilities read")
        .tmux_version()
        .has_behavior(&libtmux::since::JSON_LAYOUTS);

    let error = window
        .select_layout(r#"{"V":2,"L":[]}"#)
        .await
        .expect_err("a bare JSON skeleton is not a real layout either way");
    let message = error.to_string();
    if json_capable {
        // This tmux understands the form; whatever refuses it is tmux's own
        // validation of the content, not a version gate.
        assert!(
            !message.contains("needs tmux"),
            "a JSON-capable tmux is not refused for its version: {message}",
        );
    } else {
        assert!(
            message.contains("needs tmux") && message.contains("3.8"),
            "an old tmux names the release a JSON layout needs: {message}",
        );
    }

    // A classic-looking garbage string never mentions a version: it is
    // refused for its content on every release, the same way named layouts
    // and other malformed strings already are.
    let classic_error = window
        .select_layout("not-a-real-layout")
        .await
        .expect_err("garbage is refused");
    assert!(
        !classic_error.to_string().contains("needs tmux"),
        "a non-JSON refusal never claims a version floor: {classic_error}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane broken out into its own window can be put back.
///
/// `break_out` moves a pane away and `join_into` moves one back, so between
/// them a pane goes anywhere. Both consume the handle, because the pane's
/// window changes and a snapshot of where it used to be is wrong; the pane
/// itself keeps its id and whatever is running in it.
#[tokio::test]
async fn a_pane_broken_out_can_be_joined_back() {
    use libtmux::{JoinOptions, SplitDirection};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("moving").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");

    // Two panes, because tmux has nothing to break a lone pane out of.
    let leaving = window
        .split(SplitOptions::new(SplitDirection::Below))
        .await
        .expect("a second pane");
    let staying = window.panes().await.expect("panes").remove(0);
    let travelled = leaving.id().clone();

    leaving
        .break_out()
        .await
        .expect("the pane leaves its window");
    assert_eq!(
        window.panes().await.expect("panes").len(),
        1,
        "the window it left keeps the other pane",
    );

    // The pane still exists, in a window of its own, under the same id.
    let stranded = server
        .pane_by_id(&travelled)
        .await
        .expect("lookup")
        .expect("the pane outlived its old window");
    assert_ne!(stranded.window_id(), window.id());

    let returned = stranded
        .join_into(&staying, JoinOptions::new(SplitDirection::Below))
        .await
        .expect("the pane comes back");

    assert_eq!(returned.id(), &travelled, "the same pane, not a new one");
    assert_eq!(returned.window_id(), window.id());
    assert_eq!(window.panes().await.expect("panes").len(), 2);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// The forms tmux has at every level, not only the one that was reachable.
///
/// tmux respawns a window as well as a pane, and locks a client and a whole
/// server as well as a session. Only the narrower of each pair existed, so a
/// caller wanting the other had to build the command by hand.
#[tokio::test]
async fn respawning_and_locking_reach_every_level_tmux_offers() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("levels").await.expect("session");
    let mut window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");

    // A second pane, so respawning the window can be told from respawning one
    // pane: the window form replaces every pane with the one it runs.
    window
        .split(SplitOptions::new(SplitDirection::Below))
        .await
        .expect("a second pane");
    assert_eq!(window.panes().await.expect("panes").len(), 2);

    window
        .respawn(Some("sh"), libtmux::Respawn::Replacing)
        .await
        .expect("the window restarts");
    assert_eq!(
        window.panes().await.expect("panes").len(),
        1,
        "respawning a window leaves the one pane its command runs in",
    );

    // Locking with nobody attached locks nobody, and tmux accepts that at
    // every level rather than reporting it as a failure.
    server.lock_all().await.expect("the server locks");
    session.lock().await.expect("the session locks");
    for client in server.clients().await.unwrap_or_default() {
        client.lock().await.expect("the client locks");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane `remain-on-exit` keeps after its process ends still decodes.
///
/// tmux 3.8 reports `#{pane_pid}` as an empty string once a pane's process
/// is gone; every release before it keeps reporting that process's own pid
/// rather than clearing the field. Before this fix, an empty value there was
/// `RequiredFieldEmpty`, which failed the whole listing outright rather than
/// reporting an absent pid -- reproduced against the real daemon rather than
/// a synthetic fixture, because this is the same shape `tmux-mcp`'s dead-pane
/// tests hit: `respawn(Some("exit 0"), true)` leaves a pane whose listing
/// used to fail this way. Which of the two shapes the running tmux actually
/// reports is asserted rather than assumed, so this passes identically on
/// every release from 3.2a through the `next-3.9` tmux-matrix probe.
#[tokio::test]
async fn a_dead_panes_pid_is_absent_rather_than_a_decode_failure() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("dead-pid").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a window");
    let mut pane = window.active_pane().await.expect("panes").expect("a pane");

    let running_pid = pane.pid().expect("a running pane reports a pid");
    assert!(running_pid > 0);

    pane.set_option("remain-on-exit", "on")
        .await
        .expect("the dead pane is kept rather than closed");
    pane.respawn(Some("exit 0"), libtmux::Respawn::Replacing)
        .await
        .expect("the command runs and exits");

    retry_until(Duration::from_secs(5), async || {
        pane.refreshed()
            .await
            .is_ok_and(|refreshed| refreshed.is_dead())
    })
    .await
    .expect("the pane becomes dead");
    // The listing this refresh runs is exactly where `RequiredFieldEmpty`
    // used to surface: reaching the assertions below at all is most of what
    // this test proves.
    pane.refresh().await.expect("the pane refreshes once dead");

    assert!(pane.is_dead(), "the pane is kept, not closed");
    // Every release through 3.7c retains the exited process's own pid, so
    // this arm runs there. tmux 3.8 and later clears it instead, and the
    // `None` that leaves unchecked -- reaching it at all, rather than the
    // `refresh` above returning `Err(RequiredFieldEmpty)`, is the fix.
    if let Some(pid) = pane.pid() {
        assert!(pid > 0, "a reported pid is never zero");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Renumber a session, leaving every index after the hole naming a different
/// window than the handles cached.
async fn renumber_after_dropping(
    server: &libtmux::Server,
    session: &libtmux::Session,
    windows: &[libtmux::Window],
    drop: usize,
) {
    windows[drop]
        .clone()
        .kill()
        .await
        .expect("the window is killed");
    server
        .cmd(
            libtmux::Command::new("move-window")
                .arg("-r")
                .arg("-t")
                .arg(session.id().to_string()),
        )
        .await
        .expect("the session is renumbered");
}

/// The window sitting at an index, if anything is.
fn place_of(windows: &[libtmux::Window], index: i32) -> Option<&libtmux::Window> {
    windows.iter().find(|window| window.index() == index)
}

fn place(windows: &[libtmux::Window], id: &str) -> i32 {
    windows
        .iter()
        .find(|window| window.id().to_string() == id)
        .map(libtmux::Window::index)
        .expect("the window is listed")
}

/// Swapping targets an identity, so a renumber cannot redirect it.
///
/// `swap-window -t` took `session:index` from a cached handle. Once anything
/// renumbered the session that index named a different window, or none, so the
/// swap moved the wrong pair or failed reporting `ObjectGone` with an index
/// where a window id belongs. `is_object_gone` is the predicate a caller
/// consults before dropping a handle, and it answered `true` for a window that
/// was alive and listed.
#[tokio::test]
async fn swapping_a_window_after_a_renumber_reaches_the_same_window() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("home").await.expect("session");
    for name in ["second", "third", "fourth"] {
        session
            .new_window(NewWindowOptions::new(name))
            .await
            .expect("window");
    }

    let windows = session.windows().await.expect("windows");
    assert_eq!(windows.len(), 4, "four windows to renumber");
    let mut first = windows[0].clone();
    let last = windows[3].clone();
    let (first_id, last_id) = (first.id().to_string(), last.id().to_string());

    renumber_after_dropping(server, &session, &windows, 1).await;

    let before = session.windows().await.expect("windows");
    let (was_first, was_last) = (place(&before, &first_id), place(&before, &last_id));
    assert_ne!(
        was_last,
        last.index(),
        "the renumber moved the window out from under its cached index",
    );

    // Neither handle has been refreshed: this is what a caller holds.
    first
        .swap_with(&last)
        .await
        .expect("the windows are swapped");

    let after = session.windows().await.expect("windows");
    assert_eq!(
        (place(&after, &first_id), place(&after, &last_id)),
        (was_last, was_first),
        "the two windows exchanged places",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn swapping_windows_across_sessions_refreshes_the_destination_link() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let left = server.new_session("left").await.expect("left session");
    let right = server.new_session("right").await.expect("right session");
    let mut left_window = left
        .active_window()
        .await
        .expect("left window lookup")
        .expect("left window");
    let right_window = right
        .active_window()
        .await
        .expect("right window lookup")
        .expect("right window");
    let left_id = left_window.id().clone();
    let right_id = right_window.id().clone();
    let destination_index = right_window.index();

    left_window
        .swap_with(&right_window)
        .await
        .expect("the windows are swapped");

    assert_eq!(left_window.id(), &left_id);
    assert_eq!(left_window.session_id(), right.id());
    assert_eq!(left_window.index(), destination_index);
    assert_eq!(
        left.windows().await.expect("left windows")[0].id(),
        &right_id,
    );
    assert_eq!(
        right.windows().await.expect("right windows")[0].id(),
        &left_id,
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A rendered window target is offered for pasting, so it must not go stale.
///
/// `Display` rendered `session:index`. An index is a place within a session,
/// and the window sitting at one moves whenever anything renumbers, so the
/// rendered target reached a different window -- silently -- or nothing at
/// all. Display is also what lands in logs and in interpolated error text, so
/// the stale value travels well away from the handle that produced it.
#[tokio::test]
async fn a_rendered_window_target_survives_a_renumber() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("home").await.expect("session");
    for name in ["second", "third", "fourth"] {
        session
            .new_window(NewWindowOptions::new(name))
            .await
            .expect("window");
    }

    // The subject is a window in the middle, and the one dropped is ahead of
    // it, so after the renumber the index it cached is still OCCUPIED -- by a
    // different live window. A subject at the end would leave its old index
    // vacant, where a stale target fails loudly; this is the quiet case, and
    // the one worth holding a test.
    let windows = session.windows().await.expect("windows");
    let subject = windows[1].clone();
    let rendered = subject.to_string();
    let identity = subject.id().to_string();

    renumber_after_dropping(server, &session, &windows, 0).await;

    // The handle has not been refreshed: this is the string a caller holds.
    assert_eq!(rendered, subject.to_string(), "the rendering is unchanged");

    let stale = rendered
        .rsplit(':')
        .next()
        .and_then(|half| half.parse::<i32>().ok());
    if let Some(index) = stale {
        assert!(
            place_of(&session.windows().await.expect("windows"), index).is_some(),
            "the index {index} this rendering cached is occupied by another window",
        );
    }

    // Select away from the window under test before asking, so that reaching
    // it proves the target resolved rather than that it was already current.
    //
    // `display-message` is not the oracle here, though it is the obvious one.
    // Its `-t` is declared `CMD_FIND_PANE, CMD_FIND_CANFAIL`, so a target it
    // cannot resolve expands against the client's current pane and exits zero:
    // it answers the same for `home:@99` and for a correct target. Asking it
    // whether a rendering resolves would pass whenever the window under test
    // is also the current one, which a fixture that just built it guarantees.
    // `select-window` has no such permission and refuses.
    server
        .cmd(
            libtmux::Command::new("select-window")
                .arg("-t")
                .arg(windows[3].id().to_string()),
        )
        .await
        .expect("the command runs");

    let selected = server
        .cmd(
            libtmux::Command::new("select-window")
                .arg("-t")
                .arg(rendered.clone()),
        )
        .await
        .expect("the command runs");
    assert!(
        selected.success(),
        "the rendered target still resolves: {rendered} said {:?}",
        selected.stderr_lossy(),
    );

    // Read the current window with no target of its own, so this reading
    // cannot fall back the way the one above would have.
    let current = server
        .cmd(
            libtmux::Command::new("display-message")
                .arg("-p")
                .arg("#{window_id}"),
        )
        .await
        .expect("the command runs");
    assert_eq!(
        current.stdout_lossy().trim(),
        identity,
        "{rendered} reaches the window it was taken from",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_new_session_carries_environment_to_its_first_process() {
    let guard = TestServer::builder().start().await.expect("tmux starts");

    let session = guard
        .server()
        .new_session(NewSessionOptions::new("env").environment("LIBTMUX_RS", "set-here"))
        .await
        .expect("the session is created");
    let pane = session.panes().await.expect("panes list").remove(0);

    wait_for_prompt(&pane).await;
    pane.send_line("printf 'seen=%s\\n' \"$LIBTMUX_RS\"")
        .await
        .expect("the pane accepts the line");

    assert_eq!(
        pane.wait_for_text("seen=set-here", Duration::from_secs(10))
            .await
            .expect("the pane is readable"),
        libtmux::PaneWait::Arrived,
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_pane_reaches_the_session_it_was_found_through() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("reach")
        .await
        .expect("the session is created");
    let pane = session.panes().await.expect("panes list").remove(0);

    let reached = pane
        .session()
        .await
        .expect("the session listing is readable")
        .expect("the session still exists");

    assert_eq!(reached.id(), session.id());
    assert_eq!(text(Some(reached.name())), b"reach".to_vec());

    guard.shutdown().await.expect("tmux fixture shuts down");
}
