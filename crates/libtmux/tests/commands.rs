//! Buffers, keys, formats, and scoped operations against real tmux.

#![cfg(feature = "test-support")]

use std::time::Duration;

use libtmux::test::{TestServer, retry_until};
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
    retry_until(Duration::from_secs(10), async || {
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
    retry_until(Duration::from_secs(10), async || {
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
    retry_until(Duration::from_secs(10), async || {
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

/// A mode that is not copy mode must be leavable too.
///
/// `exit_mode` sent the cancel key, which reaches a pane through the copy-mode
/// key table. Clock mode and tree mode have none, so the key was answered "not
/// in a mode" while the pane stayed in one, and `is_in_mode` and `exit_mode`
/// disagreed about the same pane. `clock_mode` is a public way in, so the
/// crate could put a pane into a state only a raw command could clear.
///
/// The old test entered copy mode, which is the one mode the cancel key does
/// leave, so it passed throughout.
#[tokio::test]
async fn a_pane_leaves_a_mode_that_is_not_copy_mode() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("clocked").await.expect("session");
    let mut pane = session
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    pane.clock_mode().await.expect("clock mode is entered");
    pane.refresh().await.expect("the pane still exists");
    assert!(pane.is_in_mode(), "clock mode is a pane mode");

    pane.exit_mode().await.expect("clock mode is left");
    pane.refresh().await.expect("the pane still exists");
    assert!(
        !pane.is_in_mode(),
        "a mode that is not copy mode is left as well"
    );

    // A pane in no mode is left alone rather than refused, so a caller can
    // reach a known state without asking first.
    pane.exit_mode()
        .await
        .expect("leaving no mode at all is not a failure");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane's edge flags must follow the split that made it.
///
/// These read from the snapshot rather than dispatching, so nothing catches
/// them being wired to the wrong format field: `pane_at_left` and
/// `pane_at_right` differ only in a word.
#[tokio::test]
async fn edge_flags_follow_the_split_that_made_them() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("edges").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    let only = window.panes().await.expect("panes").remove(0);
    assert!(only.is_at_left(), "an undivided pane touches both edges");
    assert!(only.is_at_right(), "an undivided pane touches both edges");

    only.split(SplitOptions::new(SplitDirection::Right))
        .await
        .expect("the pane splits sideways");

    let panes = window.panes().await.expect("panes");
    assert_eq!(panes.len(), 2, "the split produced a second pane");
    let (left, right) = (&panes[0], &panes[1]);

    assert!(left.is_at_left(), "the original keeps the left edge");
    assert!(!left.is_at_right(), "and gives up the right one");
    assert!(right.is_at_right(), "the new pane takes the right edge");
    assert!(!right.is_at_left(), "and does not hold the left one");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A global window option must be read from the global table, not the window
/// the server happens to be pointing at.
///
/// A plain round trip cannot say that: writing and reading through the same
/// wrong scope agrees with itself. So this gives the window a different value
/// and checks the global read is unmoved by it.
///
/// What this cannot pin is the `-w` half of tmux's `-w -g`. tmux resolves an
/// option's table from its name, so `show-options -g main-pane-width` and
/// `-wg` return the same value and no assertion here can tell them apart.
/// Dropping `-w` in the scope leaves this test passing, which was checked
/// rather than assumed.
#[tokio::test]
async fn a_global_window_option_is_read_globally() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("options").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    server
        .set_global_window_option("main-pane-width", "123")
        .await
        .expect("the global window option is set");
    window
        .set_option("main-pane-width", "456")
        .await
        .expect("the window takes a value of its own");

    let read = server
        .get_global_window_option("main-pane-width")
        .await
        .expect("the option is readable")
        .expect("the option is set");
    assert_eq!(
        read.as_str().expect("the width is text"),
        "123",
        "the global read is not answered by the window's own value"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Piping a pane must start and stop with the same call.
///
/// `pipe-pane` toggles when given no command, so a caller who cannot stop it
/// leaves tmux writing into a process for the life of the pane.
#[tokio::test]
async fn a_pane_pipes_until_it_is_told_to_stop() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("piped").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let sink = guard.socket_path().with_extension("piped");
    pane.pipe(Some(format!("cat >{}", sink.display())))
        .await
        .expect("the pane pipes");

    pane.send_keys("printf 'piped\n'").await.expect("keys");
    pane.send_key_names(["Enter"]).await.expect("enter");

    retry_until(Duration::from_secs(10), async || {
        std::fs::read(&sink).is_ok_and(|seen| seen.windows(5).any(|word| word == b"piped"))
    })
    .await
    .expect("what the pane printed reaches the pipe");

    pane.pipe(None::<String>).await.expect("the pipe stops");
    let after = std::fs::metadata(&sink).map_or(0, |meta| meta.len());

    pane.send_keys("printf 'later\n'").await.expect("keys");
    pane.send_key_names(["Enter"]).await.expect("enter");
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        std::fs::metadata(&sink).map_or(0, |meta| meta.len()),
        after,
        "nothing more arrives once the pipe is stopped"
    );

    let _ = std::fs::remove_file(&sink);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane must report the terminal tmux gave it.
///
/// Reads from the snapshot, so a field wired to the wrong name would return
/// some other pane's string rather than fail.
#[tokio::test]
async fn a_pane_reports_its_own_terminal() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("ttys").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let reported = pane.tty().as_str().expect("a tty path is text").to_owned();
    let asked = server
        .format(Some(&pane), "#{pane_tty}")
        .await
        .expect("tmux answers for the same pane");

    assert_eq!(
        reported,
        asked.as_str().expect("a tty path is text"),
        "the accessor and tmux name the same terminal"
    );
    assert!(reported.starts_with("/dev/"), "and it is a device path");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A window's activity and bell flags must each read their own field.
///
/// They differ by a word and both are `bool`, so a swap type-checks. Reading
/// them on a fresh window cannot catch one: both are false there, and a swap
/// between two equal values is invisible. So this drives activity high and
/// leaves the bell low first, which was measured rather than assumed -- the
/// first version of this test passed with `has_bell` wired to the activity
/// field.
#[tokio::test]
async fn window_flags_each_read_their_own_field() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("flags").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    // Activity is only recorded for a window nobody is looking at, so the
    // second window is what makes the first one eligible.
    server
        .set_global_window_option("monitor-activity", "on")
        .await
        .expect("activity is monitored");
    session
        .new_window(NewWindowOptions::new("elsewhere"))
        .await
        .expect("a second window takes the focus");

    let panes = window.panes().await.expect("panes");
    panes[0].send_keys("printf 'noise\n'").await.expect("keys");
    panes[0].send_key_names(["Enter"]).await.expect("enter");

    let mut window = window;
    retry_until(Duration::from_secs(10), async || {
        window.refresh().await.is_ok() && window.has_activity()
    })
    .await
    .expect("the background window records activity");

    assert!(
        !window.has_bell(),
        "nothing rang a bell, so the two flags differ and a swap would show"
    );

    for (reported, format) in [
        (window.has_activity(), "#{window_activity_flag}"),
        (window.has_bell(), "#{window_bell_flag}"),
    ] {
        let asked = server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(window.id().to_string())
                    .arg(format),
            )
            .await
            .expect("tmux answers");
        let asked = asked.stdout_lossy().trim() == "1";
        assert_eq!(reported, asked, "{format} agrees with the accessor");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Pasting a buffer must put its bytes into the pane.
#[tokio::test]
async fn a_buffer_pastes_into_a_pane() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("pasted").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    server
        .set_buffer(Some("greeting"), "printf 'pasted-here\n'")
        .await
        .expect("the buffer is set");
    pane.paste_buffer(Some("greeting"))
        .await
        .expect("the buffer pastes");
    pane.send_key_names(["Enter"]).await.expect("enter");

    retry_until(Duration::from_secs(10), async || {
        pane.capture().await.is_ok_and(|lines| {
            lines
                .iter()
                .any(|line| line.as_bytes().windows(11).any(|w| w == b"pasted-here"))
        })
    })
    .await
    .expect("what was pasted runs in the pane");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Searching a window's panes must find one that matches.
///
/// The lenient form returns an empty vector rather than an error, so a search
/// that never matched and a search that failed look alike to a caller. This
/// covers the case where it does match, which is the one that would be silent.
#[cfg(feature = "query")]
#[tokio::test]
async fn searching_a_window_finds_the_pane_that_matches() {
    use libtmux::query::Filterable as _;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("searched").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");

    let panes = window.panes().await.expect("panes");
    let wanted = panes[0].id().to_string();

    let fields = libtmux::Pane::filter_fields();
    let found = window
        .search_panes_or_empty(fields.pane_id.eq(wanted.as_str()))
        .await;

    assert_eq!(found.len(), 1, "the pane that matches is returned");
    assert_eq!(found[0].id().to_string(), wanted);

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

    let seen = retry_until(Duration::from_secs(5), async || {
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

/// Prompt marks come from the shell, so a shell that emits none is the case
/// most callers meet: bash and zsh do not without shell integration, fish
/// does. An unmarked capture is an answer about the shell, not a failure.
#[tokio::test]
async fn real_tmux_compat_capture_line_flags_mark_prompts_when_the_shell_emits_them() {
    use libtmux::{CaptureOptions, since};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("prompts").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let supported = server
        .capabilities()
        .await
        .expect("capabilities")
        .tmux_version()
        .meets(&since::CAPTURE_LINE_FLAGS);

    if !supported {
        // Below 3.7 tmux accepts no `-F`, and saying so beats an empty answer.
        let refused = pane.capture_lines(CaptureOptions::visible()).await;
        assert!(
            matches!(
                refused.as_ref().map_err(libtmux::Error::kind),
                Err(libtmux::ErrorKind::UnsupportedVersion),
            ),
            "an older tmux reports the version rather than returning nothing: {refused:?}",
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
        return;
    }

    // Standing in for shell integration: the sequences a shell would emit
    // around its prompt and before its output.
    pane.send_keys(
        r"printf '\033]133;A\007'; echo THE-PROMPT; printf '\033]133;C\007'; echo the-output",
    )
    .await
    .expect("keys are sent");
    pane.send_key_names(["Enter"]).await.expect("Enter is sent");
    tokio::time::sleep(Duration::from_millis(600)).await;

    let lines = pane
        .capture_lines(CaptureOptions::history())
        .await
        .expect("the capture succeeds");

    let prompt = lines
        .iter()
        .position(|line| line.starts_prompt)
        .expect("the prompt mark is reported");
    let output = lines
        .iter()
        .position(|line| line.starts_output)
        .expect("the output mark is reported");

    assert!(prompt < output, "the prompt precedes its output");
    assert_eq!(
        lines[prompt].text.to_string_lossy().trim(),
        "THE-PROMPT",
        "the mark lands on the line the shell was drawing",
    );
    assert_eq!(lines[output].text.to_string_lossy().trim(), "the-output");

    // The flags are stripped rather than left at the front of the text.
    assert!(
        lines
            .iter()
            .all(|line| !line.text.to_string_lossy().starts_with("- ")),
        "no line keeps tmux's flag column in its text",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
