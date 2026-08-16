//! Buffers, keys, formats, and scoped operations against real tmux.

#![cfg(feature = "test-support")]

use libtmux::test::TestServer;
use libtmux::{Command, NewWindowOptions, SplitDirection, SplitOptions};

#[tokio::test]
async fn buffers_hold_exact_bytes_and_report_absence() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    assert!(server.buffer_names().await.expect("names").is_empty());
    assert!(
        server.buffer("absent").await.expect("read").is_none(),
        "an unknown buffer is absent rather than an error",
    );

    // A buffer holds bytes, including ones no string type would keep.
    server
        .set_buffer(Some("payload"), "line one\nline two\ttabbed")
        .await
        .expect("buffer is stored");

    assert_eq!(
        server.buffer("payload").await.expect("read"),
        Some(b"line one\nline two\ttabbed".to_vec()),
    );

    let names = server.buffer_names().await.expect("names");
    assert_eq!(names, ["payload"]);

    server
        .delete_buffer("payload")
        .await
        .expect("buffer is deleted");
    assert!(server.buffer("payload").await.expect("read").is_none());
    assert!(
        server.delete_buffer("payload").await.is_err(),
        "deleting a buffer that is gone is a failure, not a silent success",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn key_bindings_can_be_added_and_removed() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .bind_key("prefix", "Y", "display-message bound")
        .await
        .expect("key is bound");

    let listed = server
        .key_bindings(Some("prefix"))
        .await
        .expect("bindings are listed");
    assert!(
        listed
            .iter()
            .any(|line| line.contains(" Y ") && line.contains("display-message")),
        "the new binding appears in tmux's own listing",
    );

    server
        .unbind_key("prefix", "Y")
        .await
        .expect("key is unbound");
    let listed = server
        .key_bindings(Some("prefix"))
        .await
        .expect("bindings are listed");
    assert!(!listed.iter().any(|line| line.contains(" Y ")));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn formats_expand_against_a_target() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("formatted").await.expect("session");

    // A format is evaluated against a pane, and resolves the session and
    // window around it from there.
    let pane = session
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    // The trailing newline display-message adds is framing, not content.
    let name = server
        .format(Some(&pane), "#{session_name}")
        .await
        .expect("format expands");
    assert_eq!(name.as_bytes(), b"formatted");

    let combined = server
        .format(Some(&pane), "#{session_name}:#{session_windows}")
        .await
        .expect("format expands");
    assert_eq!(combined.as_bytes(), b"formatted:1");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn sourcing_a_file_applies_its_commands() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = directory.path().join("extra.conf");
    std::fs::write(&config, "set-option -s @sourced yes\n").expect("config is written");

    server.source_file(&config).await.expect("file is sourced");
    assert_eq!(
        server
            .get_option("@sourced")
            .await
            .expect("read")
            .expect("the sourced option is set")
            .as_bytes(),
        b"yes",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A caller's own error type, carrying `From<libtmux::Error>`.
///
/// That conversion is what lets a scope's setup and teardown failures join
/// the same channel as the operation's own.
#[derive(Debug, PartialEq)]
enum Failure {
    Deliberate,
    Tmux(String),
}

impl From<libtmux::Error> for Failure {
    fn from(error: libtmux::Error) -> Self {
        Self::Tmux(error.to_string())
    }
}

#[tokio::test]
async fn scoped_operations_clean_up_after_success_and_failure() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // Success: the session exists inside the scope and not after it.
    let seen = server
        .with_session("scoped", async |session| {
            Ok::<_, libtmux::Error>(session.id().to_string())
        })
        .await
        .expect("the scope runs and the operation succeeds");
    assert!(seen.starts_with('$'));
    assert!(server.sessions().await.expect("sessions").is_empty());

    // Failure: the operation's error comes back, and cleanup still ran.
    // The operation's error comes back directly: one `?`, not two.
    let outcome = server
        .with_session("failing", async |_session| {
            Err::<(), Failure>(Failure::Deliberate)
        })
        .await;
    assert_eq!(outcome, Err(Failure::Deliberate));
    assert!(
        server.sessions().await.expect("sessions").is_empty(),
        "cleanup runs even when the operation failed",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn scoped_windows_and_panes_nest() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("nested").await.expect("session");

    let before = session.windows().await.expect("windows").len();

    let pane_count = session
        .with_window(
            NewWindowOptions::new("temporary").command("sleep 300"),
            async |window| {
                window
                    .with_pane(
                        SplitOptions::new(SplitDirection::Below).command("sleep 300"),
                        async |_pane| {
                            Ok::<_, libtmux::Error>(window.panes().await.expect("panes").len())
                        },
                    )
                    .await
            },
        )
        .await
        .expect("both scopes run and the operation succeeds");

    assert_eq!(pane_count, 2, "the scoped pane existed inside its scope");
    assert_eq!(
        session.windows().await.expect("windows").len(),
        before,
        "the scoped window is gone afterwards",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn shell_commands_run_through_tmux_and_report_their_output() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // tmux 3.3 through 3.4 route run-shell output into a pane's copy-mode
    // buffer rather than to the client, and still exit zero. The crate refuses
    // there rather than reporting an empty listing, which would be a wrong
    // answer the caller could not tell from a command that printed nothing.
    let outcome = server.run_shell("printf 'first\\nsecond\\n'").await;
    let Ok(lines) = outcome else {
        let error = outcome.expect_err("refused");
        assert!(
            matches!(&error, libtmux::Error::CapabilityDefective { capability, .. }
                if *capability == "run-shell output"),
            "an affected release refuses with the reason, got {error:?}",
        );
        assert_eq!(error.kind(), libtmux::ErrorKind::UnsupportedVersion);
        guard.shutdown().await.expect("tmux fixture shuts down");
        return;
    };

    assert_eq!(
        lines
            .iter()
            .map(libtmux::TmuxText::as_bytes)
            .collect::<Vec<_>>(),
        [&b"first"[..], &b"second"[..]],
    );

    // A command producing nothing is an empty listing, not a failure.
    assert!(server.run_shell("true").await.expect("runs").is_empty());

    // Background acceptance says nothing about the command's own outcome.
    server
        .spawn_shell("false")
        .await
        .expect("tmux accepts a background command whatever it later does");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn wait_for_channels_lock_and_release() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // Locking an unheld channel takes it; unlocking releases it again.
    server
        .lock_channel("gate")
        .await
        .expect("channel is locked");
    server
        .unlock_channel("gate")
        .await
        .expect("channel is unlocked");

    // Signalling a channel nobody waits on is accepted and does nothing.
    server
        .signal_channel("gate")
        .await
        .expect("channel is signalled");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn client_operations_need_a_client_and_report_when_there_is_none() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server.new_session("headless").await.expect("session");

    // A test server has no terminal attached, so there are no clients and the
    // operations that need one fail loudly rather than pretending to work.
    assert!(server.clients().await.expect("clients list").is_empty());
    assert!(
        server.display_popup(None, "true").await.is_err(),
        "a popup needs a client with a terminal",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Exercise `Client` against the one kind of client a headless test can make.
///
/// Every other client needs a terminal. A control-mode attach does not, and
/// tmux counts it as a client like any other, so it is what makes the
/// positive case testable at all.
#[cfg(feature = "control-mode")]
#[tokio::test]
async fn a_client_reports_itself_while_it_is_attached() {
    use libtmux::control::ControlMode;
    use std::time::Duration;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let first = server.new_session("client-one").await.expect("session");
    let second = server.new_session("client-two").await.expect("session");

    let control = ControlMode::attach(server, first.id())
        .await
        .expect("control mode attaches");

    // tmux registers the client as part of attaching, but the listing is a
    // separate command, so wait for it rather than assuming the order.
    libtmux::test::retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| !clients.is_empty())
    })
    .await
    .expect("the attached client is listed");

    let mut client = server.clients().await.expect("clients").remove(0);
    assert!(client.is_control_mode(), "this client attached with -C");
    assert!(!client.name().as_bytes().is_empty());
    assert!(client.pid() > 0);

    // A client is attached to exactly one session, and switching moves it.
    client
        .switch_to(&second)
        .await
        .expect("the client switches session");
    libtmux::test::retry_until(Duration::from_secs(10), async || {
        second
            .refreshed()
            .await
            .is_ok_and(|session| session.is_attached())
    })
    .await
    .expect("the second session reports the client");

    client.refresh().await.expect("the client still exists");

    // Detaching consumes the handle, and the listing empties out.
    client.detach().await.expect("the client detaches");
    libtmux::test::retry_until(Duration::from_secs(10), async || {
        server
            .clients()
            .await
            .is_ok_and(|clients| clients.is_empty())
    })
    .await
    .expect("the detached client stops being listed");

    let _ = control.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_modes_enter_and_leave() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("modes").await.expect("session");
    let mut pane = session
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    assert!(!pane.is_in_mode(), "a fresh pane has no mode open");

    pane.copy_mode().await.expect("copy mode is entered");
    pane.refresh().await.expect("the pane still exists");
    assert!(pane.is_in_mode(), "copy mode is a pane mode");

    pane.exit_mode().await.expect("the mode is cancelled");
    pane.refresh().await.expect("the pane still exists");
    assert!(!pane.is_in_mode(), "cancelling leaves the mode");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn interactive_commands_need_a_client() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server.new_session("interactive").await.expect("session");

    // A headless server has no terminal, so commands that draw on one fail
    // loudly rather than reporting a success nobody could see.
    assert!(server.display_panes(None).await.is_err());
    assert!(
        server
            .display_menu(
                None,
                "menu",
                [("Item".into(), "i".into(), "kill-pane".into())]
            )
            .await
            .is_err(),
    );
    assert!(
        server
            .command_prompt(None, Some("question"), "kill-pane")
            .await
            .is_err(),
    );

    // The choosers are the exception: tmux accepts one with no client and
    // silently does nothing. This records what tmux does rather than what
    // symmetry would suggest.
    server
        .choose(libtmux::Chooser::Tree, None)
        .await
        .expect("tmux accepts a chooser with no client");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn retry_until_waits_for_tmux_rather_than_sleeping() {
    use libtmux::test::{retry_until, unique_name};
    use std::time::Duration;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // A unique name lets concurrent tests share one server without colliding.
    let name = unique_name("retry");
    let session = server.new_session(name.as_str()).await.expect("session");
    assert_ne!(unique_name("retry"), name);

    // tmux applies a split before the new pane's command has necessarily
    // execed, so wait for the state under test.
    session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window")
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");

    retry_until(Duration::from_secs(5), async || {
        session.panes().await.is_ok_and(|panes| panes.len() == 2)
    })
    .await
    .expect("the second pane appears");

    // A condition that never holds reports the deadline instead of hanging.
    let waited = retry_until(Duration::from_millis(50), async || false)
        .await
        .expect_err("a condition that never holds times out");
    assert_eq!(waited.waited(), Duration::from_millis(50));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_scoped_raw_command_targets_its_own_object() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let first = server.new_session("first").await.expect("first session");
    let second = server.new_session("second").await.expect("second session");

    // Each handle addresses itself, not whatever tmux considers current.
    for (session, expected) in [(&first, "first"), (&second, "second")] {
        let named = session
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("#{session_name}"),
            )
            .await
            .expect("the command runs");
        assert_eq!(named.stdout_lossy().trim(), expected);
    }

    // The placement is the point. tmux stops reading flags at the first
    // positional, so a target appended after `--` is taken as text: the
    // command succeeds and types it into whichever pane was current. Sending
    // through the handle puts `-t` where tmux still reads it.
    let window = second
        .active_window()
        .await
        .expect("active window")
        .expect("a session has a window");
    let pane = window
        .active_pane()
        .await
        .expect("active pane")
        .expect("a window has a pane");

    pane.cmd(
        Command::new("send-keys")
            .arg("--")
            .arg("echo scoped")
            .arg("Enter"),
    )
    .await
    .expect("keys are sent");

    let seen = libtmux::test::retry_until(std::time::Duration::from_secs(5), async || {
        pane.capture().await.is_ok_and(|lines| {
            lines
                .iter()
                .any(|line| String::from_utf8_lossy(line.as_bytes()).contains("scoped"))
        })
    })
    .await;
    assert!(seen.is_ok(), "the keys reached this pane, not another");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn formatting_returns_text_and_displaying_returns_nothing() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("split").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("active window")
        .expect("a session has a window");

    // The reading half expands in the object's own context, so each handle
    // answers about itself rather than about whatever tmux considers current.
    assert_eq!(
        session
            .format("#{session_name}")
            .await
            .expect("format")
            .to_string_lossy(),
        "split",
    );
    assert_eq!(
        window
            .format("#{window_id}")
            .await
            .expect("format")
            .to_string_lossy(),
        window.id().to_string(),
    );

    // A format that expands to nothing is empty text rather than an error:
    // tmux expanded it fine, the answer is just empty.
    assert!(
        session
            .format("#{?0,yes,}")
            .await
            .expect("format")
            .as_bytes()
            .is_empty(),
    );

    // The showing half returns nothing, and succeeds with nobody attached: a
    // headless server shows the message to no one, which is not a failure.
    let pane = window
        .active_pane()
        .await
        .expect("active pane")
        .expect("a window has a pane");
    assert_eq!(
        pane.format("#{pane_id}")
            .await
            .expect("format")
            .to_string_lossy(),
        pane.id().to_string(),
    );

    session.display("hello").await.expect("display");
    window.display("hello").await.expect("display");
    pane.display("hello").await.expect("display");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_capability_too_new_for_this_tmux_is_refused_rather_than_ignored() {
    // Follows the compat suite's convention: `LIBTMUX_TEST_TMUX` points this
    // at one release of the version matrix, so the same assertion runs across
    // every supported tmux rather than only whichever is on PATH.
    let mut builder = TestServer::builder();
    if let Some(executable) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        builder = builder.tmux_executable(executable);
    }
    let guard = builder.start().await.expect("tmux starts");
    let server = guard.server();

    let version = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .clone();
    let supported = version.release().is_none_or(|release| {
        *release >= libtmux::ReleaseVersion::new(3, 3, libtmux::ReleaseSuffix::FINAL)
    });

    let answer = server.prompt_history(libtmux::PromptKind::Command).await;
    if supported {
        // Nothing has answered a prompt on a fresh server, so the history is
        // empty rather than absent.
        assert!(answer.expect("prompt history reads").is_empty());
    } else {
        // tmux below 3.3 has no such command. Left to tmux this would surface
        // as an unknown-command failure or, for a flag, as silence: the
        // command succeeding having ignored what was asked for.
        let error = answer.expect_err("an older tmux refuses the capability");
        assert_eq!(error.kind(), libtmux::ErrorKind::UnsupportedVersion);
        assert!(
            error.to_string().contains("3.3"),
            "the refusal says what it needs: {error}",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_server_access_list_names_its_owner_and_refuses_to_unseat_them() {
    let mut builder = TestServer::builder();
    if let Some(executable) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        builder = builder.tmux_executable(executable);
    }
    let guard = builder.start().await.expect("tmux starts");
    let server = guard.server();

    let supported = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .release()
        .is_none_or(|release| {
            *release >= libtmux::ReleaseVersion::new(3, 3, libtmux::ReleaseSuffix::FINAL)
        });
    if !supported {
        assert_eq!(
            server
                .access_rules()
                .await
                .expect_err("an older tmux has no access list")
                .kind(),
            libtmux::ErrorKind::UnsupportedVersion,
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
        return;
    }

    // Whoever started the server owns it, and an owner may always act.
    let rules = server.access_rules().await.expect("access list");
    let owner = rules.first().expect("the owner is listed");
    assert_eq!(rules.len(), 1);
    assert_eq!(owner.mode(), libtmux::AccessMode::Write);
    assert!(!owner.user().is_empty());

    // tmux refuses to change the owner's own entry, so a caller cannot lock
    // itself out of the server it just started.
    let user = owner.user().to_owned();
    for attempt in [
        server
            .grant_access(&user, libtmux::AccessMode::ReadOnly)
            .await,
        server.revoke_access(&user).await,
    ] {
        let error = attempt.expect_err("tmux refuses to change the owner");
        assert!(
            error.to_string().contains("owns the server"),
            "the refusal says why: {error}",
        );
    }

    // And the list is unchanged by the refusals.
    assert_eq!(server.access_rules().await.expect("access list"), rules);

    guard.shutdown().await.expect("tmux fixture shuts down");
}
