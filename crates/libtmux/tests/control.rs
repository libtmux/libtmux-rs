//! Control mode against real tmux.

#![cfg(all(feature = "control-mode", feature = "test-support"))]

use std::time::Duration;

use libtmux::control::{ControlEvents, ControlMode, ControlSender, Event, Subscription};
use libtmux::test::TestServer;
use libtmux::{Command, NewWindowOptions};
use static_assertions::assert_impl_all;
use tokio_stream::StreamExt as _;

// Unpin is what lets these compose without Box::pin at every step, and Send
// is what lets a caller watch from a task that is not the one that attached.
assert_impl_all!(ControlSender: Clone, Send, Sync, Unpin);
assert_impl_all!(ControlEvents: Send, Sync, Unpin, futures_core::Stream);
assert_impl_all!(ControlMode: Send, Sync, Unpin);

/// Report whether an event says a window has appeared.
///
/// tmux picks between the linked and unlinked forms by whether the attached
/// session holds the window, and follows either with a rename once the window
/// takes its name, so a test naming one of the three is testing tmux's
/// bookkeeping rather than its own subject.
fn is_a_new_window(event: &Event) -> bool {
    matches!(
        event,
        Event::WindowAdded { .. } | Event::UnlinkedWindowAdded { .. } | Event::WindowRenamed { .. }
    )
}

/// Wait for the first event a caller cares about, or give up.
///
/// Control mode reports plenty that a given test did not ask about, and the
/// exact set differs between tmux releases, so a test names what it wants
/// rather than asserting on the next event to arrive.
async fn wait_for(
    events: &mut ControlEvents,
    mut wanted: impl FnMut(&Event) -> bool,
) -> Option<Event> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, events.next_event()).await {
            Ok(Some(event)) if wanted(&event) => return Some(event),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

#[tokio::test]
async fn commands_travel_down_one_connection() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("control").await.expect("session");

    let control = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches");

    // No process spawn per command: these go down the connection already open.
    let listed = control
        .send(Command::new("list-windows").arg("-F").arg("#{window_name}"))
        .await
        .expect("the command is answered");
    assert!(listed.succeeded());
    assert_eq!(listed.output().len(), 1);

    // A refused command reports failure with tmux's message, not silence.
    let refused = control
        .send(Command::new("kill-window").arg("-t").arg("@999"))
        .await
        .expect("the command is answered");
    assert!(!refused.succeeded(), "tmux closed the block with %error");
    assert!(!refused.output().is_empty(), "the reason is preserved");

    // Correlation is by the number tmux assigns, so the two differ.
    assert_ne!(listed.number(), refused.number());

    control.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_server_reports_changes_as_they_happen() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("watched").await.expect("session");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    // Change the server from outside the connection. Control mode reports it
    // without being asked, which is the whole point: no polling.
    session
        .new_window(NewWindowOptions::new("appeared").command("sleep 300"))
        .await
        .expect("window is created");

    let reported = wait_for(&mut events, is_a_new_window).await;
    assert!(
        reported.is_some(),
        "a window appearing is reported without polling",
    );

    // Acting on what arrived is the reason the halves are separate: this send
    // happens while the event stream is still borrowed by the loop above.
    let listed = commands
        .send(Command::new("list-windows").arg("-F").arg("#{window_name}"))
        .await
        .expect("a command sent in reaction to an event is answered");
    assert!(listed.succeeded());
    assert_eq!(listed.output().len(), 2);

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn watching_and_sending_run_at_the_same_time() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("concurrent").await.expect("session");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    // One task watches while another sends. Neither waits for the other, and
    // the sender is cloned rather than shared, because it is just a channel.
    let sender = commands.clone();
    let watcher =
        tokio::spawn(async move { wait_for(&mut events, is_a_new_window).await.map(|_| events) });

    let created = sender
        .send(
            Command::new("new-window")
                .arg("-d")
                .arg("-n")
                .arg("concurrent")
                .arg("sleep 300"),
        )
        .await
        .expect("the command is answered");
    assert!(created.succeeded());

    let events = watcher
        .await
        .expect("the watcher task finishes")
        .expect("the watcher saw the window the sender created");

    drop((commands, sender));
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn pane_output_keeps_bytes_no_string_would_hold() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("binary").await.expect("session");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    // tmux escapes only what would break the line protocol, so these bytes
    // arrive raw and the line is not UTF-8. Reading control mode as text
    // fails the whole connection the first time a pane prints one of these.
    commands
        .send(
            Command::new("new-window")
                .arg("-d")
                .arg("-n")
                // The pane has to outlive its output: tmux drops what a pane
                // has buffered when the pane exits, so a command that writes
                // and returns can be reported as nothing at all.
                .arg("emitting")
                .arg(r"printf '\377\303\050'; sleep 300"),
        )
        .await
        .expect("the command is answered");

    let output = wait_for(
        &mut events,
        |event| matches!(event, Event::Output { bytes, .. } if bytes.contains(&0xff)),
    )
    .await
    .expect("the pane's output arrives");

    let Event::Output { bytes, .. } = output else {
        panic!("the matched event is pane output");
    };
    let start = bytes
        .windows(3)
        .position(|window| window == [0xff, 0xc3, b'('])
        .expect("the exact bytes the pane wrote are preserved");
    assert_eq!(&bytes[start..start + 3], [0xff, 0xc3, b'(']);

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn events_compose_with_the_async_ecosystem() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("streamed").await.expect("session");

    let (commands, events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    session
        .new_window(NewWindowOptions::new("streamed").command("sleep 300"))
        .await
        .expect("window is created");

    // Events are a Stream, so the ecosystem's combinators apply and no loop
    // of this crate's own design is required. The pin is tokio's timer's
    // requirement, not this crate's: ControlEvents is Unpin on its own.
    let named = events
        .timeout(Duration::from_secs(10))
        .filter_map(Result::ok)
        .filter(is_a_new_window);
    let mut named = std::pin::pin!(named);

    assert!(
        named.next().await.is_some(),
        "the stream yields the window notification",
    );

    drop(commands);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn attaching_finishes_before_anything_can_be_missed() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("racing").await.expect("session");

    // Attach and change the server with nothing in between. If attach were
    // to return as soon as the process started, tmux would still be attaching
    // while the window appeared, and the notification would never be sent --
    // a race that only shows up under load, and looks like nothing at all.
    for round in 0..20 {
        let (commands, mut events) = ControlMode::attach(server, session.id())
            .await
            .expect("control mode attaches")
            .split();

        let name = format!("round-{round}");
        session
            .new_window(NewWindowOptions::new(name.as_str()).command("sleep 300"))
            .await
            .expect("window is created");

        assert!(
            wait_for(&mut events, |event| { is_a_new_window(event) })
                .await
                .is_some(),
            "round {round}: the window that appeared right after attaching is reported",
        );

        drop(commands);
        events.shutdown().await.expect("control mode shuts down");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn either_half_keeps_the_connection_alive_on_its_own() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("halves").await.expect("session");

    // A caller who only watches has no use for the sender. Dropping it must
    // not take the connection with it.
    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();
    drop(commands);

    session
        .new_window(NewWindowOptions::new("watched").command("sleep 300"))
        .await
        .expect("window is created");
    assert!(
        wait_for(&mut events, |event| { is_a_new_window(event) })
            .await
            .is_some(),
        "events still arrive after the sender is gone",
    );
    events.shutdown().await.expect("control mode shuts down");

    // The reverse: a caller who only sends drops the events, and commands
    // still work.
    let (commands, events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();
    drop(events);
    assert!(
        commands
            .send(Command::new("list-windows"))
            .await
            .expect("the connection outlives the events handle")
            .succeeded(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn shutting_down_does_not_wait_for_a_sender_that_is_still_alive() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("stopping").await.expect("session");

    let (commands, events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    // Shutting down means now, not once every other handle agrees. A caller
    // holding a live sender in another task must not be able to hang this.
    tokio::time::timeout(Duration::from_secs(5), events.shutdown())
        .await
        .expect("shutdown does not wait on the sender")
        .expect("control mode shuts down");

    // The sender outlives it and reports the connection as gone rather than
    // waiting for an answer that is not coming.
    assert!(
        commands.send(Command::new("list-windows")).await.is_err(),
        "a command sent after shutdown fails rather than hangs",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn one_pane_can_be_watched_without_the_protocol_showing() {
    use libtmux::SplitDirection;
    use libtmux::SplitOptions;
    use libtmux::control::PaneOutput;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("streaming").await.expect("session");

    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    // Two panes writing at once, so filtering to one is doing real work.
    let watched = window
        .split(
            SplitOptions::new(SplitDirection::Below)
                .command(r"while true; do printf 'watched '; sleep 0.1; done"),
        )
        .await
        .expect("pane is created");
    window
        .split(
            SplitOptions::new(SplitDirection::Right)
                .command(r"while true; do printf 'ignored '; sleep 0.1; done"),
        )
        .await
        .expect("pane is created");

    let mut output: PaneOutput = watched.stream_output().await.expect("the pane streams");
    assert_eq!(output.pane(), watched.id());

    let mut collected = Vec::new();
    while collected.len() < 64 {
        let chunk = tokio::time::timeout(Duration::from_secs(10), output.next_chunk())
            .await
            .expect("the pane keeps writing")
            .expect("the pane is still open");
        collected.extend_from_slice(&chunk);
    }

    let seen = String::from_utf8_lossy(&collected);
    assert!(
        seen.contains("watched"),
        "the watched pane's output: {seen}"
    );
    assert!(
        !seen.contains("ignored"),
        "the other pane's output is filtered out: {seen}",
    );

    output.shutdown().await.expect("the connection shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_command_holding_spaces_survives_the_text_protocol() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("quoting").await.expect("session");

    let control = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches");

    // Control mode is a line protocol, so a token with a space has to survive
    // tmux reparsing it.
    let result = control
        .send(
            Command::new("set-option")
                .arg("-s")
                .arg("@spaced")
                .arg("a b  c"),
        )
        .await
        .expect("the command is answered");
    assert!(result.succeeded(), "tmux parsed the quoted token");

    assert_eq!(
        server
            .get_option("@spaced")
            .await
            .expect("read")
            .expect("the option is set"),
        "a b  c",
    );

    control.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_command_that_cannot_be_a_line_is_refused_before_it_is_sent() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server
        .new_session("unrepresentable")
        .await
        .expect("session");

    let control = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches");

    // Control mode carries text, so an argument that is not UTF-8 cannot go
    // down it even though the same command runs fine as a subprocess. The
    // error says which, rather than looking like a dropped connection.
    let error = control
        .send(
            Command::new("set-option")
                .arg("-s")
                .arg("@binary")
                .arg(OsString::from_vec(vec![0xff])),
        )
        .await
        .expect_err("a non-UTF-8 argument cannot be a control-mode line");
    assert!(
        matches!(
            error,
            libtmux::Error::ControlMode {
                kind: libtmux::ControlModeErrorKind::UnrepresentableCommand,
                ..
            },
        ),
        "the reason is distinguishable from the connection closing: {error:?}",
    );

    // The connection is untouched: nothing was written.
    assert!(
        control
            .send(Command::new("list-windows"))
            .await
            .expect("the connection still works")
            .succeeded(),
    );

    control.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn detaching_a_session_removes_the_clients_attached_to_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("busy").await.expect("session");

    // Control mode attaches a real client, which is what makes this
    // observable: a headless fixture otherwise has nobody to detach, and a
    // test that only checks the call returns Ok would pass against a method
    // that did nothing.
    let control = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches");
    let (sender, events) = control.split();
    assert_eq!(server.clients().await.expect("clients").len(), 1);

    session.detach_clients().await.expect("clients detach");
    assert!(
        server.clients().await.expect("clients").is_empty(),
        "the attached client is gone",
    );

    // And asking again, with nobody left, is accepted rather than refused.
    session
        .detach_clients()
        .await
        .expect("detaching nobody is a no-op");

    drop(sender);
    let _ = events.shutdown().await;
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_client_reports_the_session_window_and_pane_it_is_attached_to() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("attached").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("windows")
        .expect("a session has a window");
    let pane = window
        .active_pane()
        .await
        .expect("panes")
        .expect("a window has a pane");

    // Control mode attaches a real client. Without one there is nothing to
    // ask, and a test against a detached fixture would pass against a method
    // that always answered `None`.
    let control = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches");

    let client = server
        .clients()
        .await
        .expect("clients")
        .into_iter()
        .next()
        .expect("the control-mode client");

    // Resolved through `#{session_id}` rather than `#{client_session}`, which
    // is a name. The ids are what make these real handles.
    assert_eq!(
        client
            .attached_session()
            .await
            .expect("session resolves")
            .expect("it is attached")
            .id(),
        session.id(),
    );
    assert_eq!(
        client
            .attached_window()
            .await
            .expect("window resolves")
            .expect("it is attached")
            .id(),
        window.id(),
    );
    assert_eq!(
        client
            .attached_pane()
            .await
            .expect("pane resolves")
            .expect("it is attached")
            .id(),
        pane.id(),
    );

    // The same client is reachable by the name tmux gave it.
    let by_name = server
        .client(client.name().as_bytes())
        .await
        .expect("lookup")
        .expect("the client is listed");
    assert_eq!(by_name.name(), client.name());

    control.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A flooding pane must not reach a connection that asked for another one.
///
/// Filtering after the bytes arrive looks identical to this when the
/// neighbours are quiet, which is why the noise is part of the fixture: one
/// `yes` moves tens of megabytes a second, and every one of them was being
/// read, allocated and dropped.
#[tokio::test]
async fn a_muted_pane_never_reaches_this_connection() {
    use libtmux::{SplitDirection, SplitOptions};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("muting").await.expect("session");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    let watched = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");
    let noisy = window
        .split(SplitOptions::new(SplitDirection::Right).command("sleep 300"))
        .await
        .expect("pane is created");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    // Muted before anything floods, so a byte arriving is a real failure
    // rather than one that raced the command.
    commands
        .watch_only(std::slice::from_ref(watched.id()))
        .await
        .expect("the connection narrows to one pane");

    noisy
        .send_keys("yes flooding-the-neighbour")
        .await
        .expect("the neighbour floods");
    watched
        .send_keys("echo watched-pane-marker")
        .await
        .expect("the watched pane writes");

    let mut saw_marker = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !saw_marker {
        let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.next_event()).await else {
            break;
        };
        if let Event::Output { pane, bytes } = &event {
            assert_ne!(
                pane,
                noisy.id(),
                "a muted pane reached this connection: {}",
                String::from_utf8_lossy(bytes),
            );
            if pane == watched.id() {
                saw_marker |= String::from_utf8_lossy(bytes).contains("watched-pane-marker");
            }
        }
    }

    assert!(saw_marker, "the watched pane's own output still arrives");

    events.shutdown().await.expect("the connection shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A pane split into being after the narrowing must be muted too.
///
/// tmux publishes no notification for a pane appearing, so this is repaired
/// from `%layout-change` -- which a detached split reports even though the
/// active pane never changes.
#[tokio::test]
async fn a_pane_created_after_narrowing_is_muted_too() {
    use libtmux::control::PaneOutput;
    use libtmux::{SplitDirection, SplitOptions};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("late-pane").await.expect("session");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    let watched = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");

    let mut output: PaneOutput = watched.stream_output().await.expect("the pane streams");

    // Detached, so the only thing tmux reports is the layout change.
    let latecomer = window
        .split(SplitOptions::new(SplitDirection::Right).command("sleep 300"))
        .await
        .expect("pane is created");
    latecomer
        .send_keys("yes flooding-from-a-late-pane")
        .await
        .expect("the latecomer floods");

    // Drive the loop so the repair is reached, and check the watched pane is
    // still the only one heard from.
    watched
        .send_keys("echo still-mine")
        .await
        .expect("the watched pane writes");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("still-mine") {
        let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, output.next_chunk()).await else {
            panic!("the watched pane's marker never arrived");
        };
        seen.extend_from_slice(&chunk);
    }

    // The repair is asynchronous, so give it a moment, then require that the
    // connection has gone quiet rather than still carrying the flood.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let quiet = tokio::time::timeout(Duration::from_millis(500), output.next_chunk()).await;
    assert!(
        quiet.is_err(),
        "the late pane is still flooding this connection",
    );

    output.shutdown().await.expect("the connection shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Command output whose every line begins with `%` must survive the block.
///
/// A pane id is spelled `%0`, so `list-panes -F '#{pane_id}'` produces rows
/// indistinguishable from notifications outside a block. Reading them as
/// notifications loses the whole answer and reports events that never
/// happened.
#[tokio::test]
async fn a_block_carrying_pane_ids_keeps_them_as_output() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("ids").await.expect("session");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    let listed = commands
        .send(
            Command::new("list-panes")
                .arg("-a")
                .arg("-F")
                .arg("#{pane_id}"),
        )
        .await
        .expect("the command runs");

    assert!(listed.succeeded(), "list-panes succeeded");
    let rows: Vec<_> = listed
        .output()
        .iter()
        .map(|row| row.as_str().expect("a pane id is ASCII").to_owned())
        .collect();
    assert!(
        rows.iter().all(|row| row.starts_with('%')),
        "every row is a pane id: {rows:?}",
    );
    assert!(!rows.is_empty(), "the session has at least one pane");

    // The rows must not have been reported as notifications instead.
    let stray = tokio::time::timeout(Duration::from_millis(200), events.next_event()).await;
    if let Ok(Some(event)) = stray {
        assert!(
            !matches!(&event, Event::Other { name, .. } if name.chars().all(char::is_numeric)),
            "a pane id was reported as a notification: {event:?}",
        );
    }

    events.shutdown().await.expect("the connection shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Narrowing must complete on a server that is already flooding.
///
/// The command's reply arrives behind whatever output tmux has queued, so a
/// connection that stops reading once its event buffer fills can never finish
/// the very call that would quieten it.
/// A reply must not wait on the caller draining events.
///
/// The reply to a command arrives on the connection the events arrive on, so
/// a connection that stops reading because nobody is taking its events has
/// stopped reading the reply too. Nothing times out and `is_closed` stays
/// false, so a caller cannot tell a stalled connection from a quiet server.
#[tokio::test]
async fn a_reply_arrives_while_a_pane_floods_and_nobody_reads() {
    use libtmux::{SplitDirection, SplitOptions};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("unread").await.expect("session");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    let (commands, events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    // Enough to fill the queue many times over and no more. The queue holds
    // 256, so what matters is exceeding it, not the size of the backlog left
    // for teardown to kill: a larger flood buys nothing here and costs every
    // test sharing the machine.
    window
        .split(SplitOptions::new(SplitDirection::Below).command("seq 1 20000"))
        .await
        .expect("pane is created");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // `events` is held and never polled, which is what a caller awaiting a
    // reply does for as long as the await lasts.
    // Either answer is fine and the point is that one arrives. A caller who
    // will not read has asked for something the connection cannot always give
    // -- a reply travels the way the events do -- so past what it can hold it
    // says so rather than waiting to be rescued by the caller who is waiting
    // for it. What it must never do is neither.
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        commands.send(Command::new("list-windows")),
    )
    .await
    .expect("an answer arrives rather than a wait that never ends");

    match outcome {
        Ok(answered) => assert!(answered.succeeded()),
        Err(refused) => assert_eq!(
            refused.kind(),
            libtmux::ErrorKind::Refused,
            "the connection says why rather than stalling: {refused:?}",
        ),
    }
    assert!(
        !commands.is_closed(),
        "the connection is not closed either way"
    );

    // Bounded, because everything else here is. An unbounded teardown turns a
    // regression into a run that never ends, and a test that hangs its own
    // suite reports nothing at all.
    drop(commands);
    tokio::time::timeout(Duration::from_secs(30), events.shutdown())
        .await
        .expect("the connection shuts down rather than hanging the suite")
        .expect("control mode shuts down");
    tokio::time::timeout(Duration::from_secs(30), guard.shutdown())
        .await
        .expect("the fixture shuts down rather than hanging the suite")
        .expect("tmux fixture shuts down");
}

/// The same, with no pane output at all.
///
/// This is what says the stall is not about volume. Every one of these
/// commands raises notifications of its own, so a caller who subscribes to
/// nothing and floods nothing still fills the queue with the consequences of
/// its own work, and the connection it filled is the one carrying its replies.
#[tokio::test]
async fn replies_arrive_when_a_caller_only_ever_sends() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("sending").await.expect("session");

    let (commands, events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    // Far past the event queue, which is where this used to stop.
    for index in 0..200 {
        let created = tokio::time::timeout(
            Duration::from_secs(10),
            commands.send(Command::new("new-window").arg("-d")),
        )
        .await
        .unwrap_or_else(|_| panic!("command {index} was answered"))
        .expect("the command is answered");
        assert!(created.succeeded(), "command {index} succeeded");
    }

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A slow reader of one pane's output must not be given gaps.
///
/// `Pane::stream_output` documents itself as the bytes that pane produced, in
/// order. Today the connection parking on a full event queue is what makes
/// that true: the reader stops, tmux stops writing, and nothing is lost. Any
/// change that keeps the reader moving under backpressure threatens it, and a
/// gap here is invisible without a counted sequence to check against.
#[tokio::test]
async fn a_slow_reader_of_one_pane_is_given_every_byte() {
    use libtmux::{SplitDirection, SplitOptions};

    const LAST: u32 = 50_000;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("counted").await.expect("session");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    // The pane waits before it counts and stays alive after, so the stream is
    // attached for the whole sequence: a pane that has already finished has
    // nothing left to send, and one that exits takes its window with it.
    let counted = window
        .split(
            SplitOptions::new(SplitDirection::Below)
                .command(format!("sh -c 'sleep 2; seq 1 {LAST}; sleep 60'")),
        )
        .await
        .expect("pane is created");

    let mut output = counted.stream_output().await.expect("the pane streams");

    // Reading slower than the pane writes is the whole condition: a reader
    // that keeps up never fills the queue and never exercises the policy.
    let mut received = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while tokio::time::Instant::now() < deadline {
        let Ok(chunk) = tokio::time::timeout(Duration::from_secs(5), output.next_chunk()).await
        else {
            break;
        };
        let Some(chunk) = chunk else { break };
        received.extend_from_slice(&chunk);
        tokio::time::sleep(Duration::from_millis(5)).await;
        if received_reaches(&received, LAST) {
            break;
        }
    }

    // tmux wraps pane output at the pane width, so the bytes carry the
    // sequence with line endings the terminal chose rather than the ones
    // `seq` wrote. Checking that every number appears in order tolerates that
    // without tolerating a missing number.
    let text = String::from_utf8_lossy(&received);
    let mut expected = 1u32;
    for number in text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
    {
        if number.parse::<u32>() == Ok(expected) {
            expected += 1;
        }
    }
    assert_eq!(
        expected - 1,
        LAST,
        "the stream skipped from {} onward; {} bytes arrived",
        expected,
        received.len(),
    );

    output.shutdown().await.expect("the connection shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Whether the counted sequence has been seen through to its last number.
fn received_reaches(received: &[u8], last: u32) -> bool {
    let text = String::from_utf8_lossy(received);
    text.split(|c: char| !c.is_ascii_digit())
        .any(|piece| piece.parse::<u32>() == Ok(last))
}

#[tokio::test]
async fn a_stream_opens_on_a_server_that_is_already_flooding() {
    use libtmux::control::PaneOutput;
    use libtmux::{SplitDirection, SplitOptions};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("busy").await.expect("session");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    let watched = window
        .split(SplitOptions::new(SplitDirection::Below).command("sleep 300"))
        .await
        .expect("pane is created");
    let noisy = window
        .split(SplitOptions::new(SplitDirection::Right).command("sleep 300"))
        .await
        .expect("pane is created");

    // Flooding before the attach, so the queue is filling as it opens.
    noisy
        .send_keys("yes flooding-before-the-attach")
        .await
        .expect("the neighbour floods");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let output: PaneOutput = tokio::time::timeout(Duration::from_secs(20), watched.stream_output())
        .await
        .expect("opening the stream does not hang")
        .expect("the pane streams");

    assert_eq!(output.pane(), watched.id());

    output.shutdown().await.expect("the connection shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Muting a pane that is already producing must not take the server with it.
///
/// Below [`libtmux::since::CONTROL_PANE_OFF`], `refresh-client -A <pane>:off`
/// leaves the output blocks already queued for that pane in place while the
/// pane stops holding the server's read buffer back. Writing them later reads
/// past the end of what the server has since drained, and the server
/// segfaults, so every command after it reports `server exited unexpectedly`.
///
/// The queue is the whole condition: muting an idle pane never reaches it,
/// which is why the flood test above passes on every release. So the panes
/// run their flood as their own command rather than being typed into, the
/// mute waits until that flood has reached the connection, and the reader is
/// slower than the panes it reads.
#[tokio::test]
async fn real_tmux_compat_muting_a_producing_pane_leaves_the_server_up() {
    use libtmux::{SplitDirection, SplitOptions};

    const MARKER: &str = "libtmux-mute-regression";

    let mut guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard.session("mute").await.expect("session is created");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    // The flood is the pane's command, so it starts with the pane rather than
    // after a shell that may not be reading yet.
    let mut noisy = Vec::new();
    for _ in 0..3 {
        noisy.push(
            window
                .split(SplitOptions::new(SplitDirection::Below).command(format!("yes {MARKER}")))
                .await
                .expect("pane is created"),
        );
    }

    let (commands, mut events) = ControlMode::attach(guard.server(), session.id())
        .await
        .expect("control mode attaches")
        .split();

    // Muting a pane that has written nothing tests nothing, so this waits for
    // the flood rather than assuming it started.
    wait_for(&mut events, |event| {
        matches!(event, Event::Output { bytes, .. }
            if String::from_utf8_lossy(bytes).contains(MARKER))
    })
    .await
    .expect("a flooding pane reaches the connection");

    // Drained as fast as this can, because the queue does not need help: the
    // panes write faster than anything reads, and a reader that falls far
    // enough behind stalls the connection this test drives.
    let reader = tokio::spawn(async move { while events.next_event().await.is_some() {} });

    // Muting is what reaches the defect. The server writes the blocks that
    // were queued for a pane after the mute has been answered, so the failure
    // can land on any command after it, and every one of them is reported
    // against the daemon rather than as a closed connection: the second says
    // nothing about why.
    for pane in &noisy {
        let muted = commands.mute_pane(pane.id()).await;
        let daemon = guard.daemon_state();
        assert!(
            daemon.is_running() && muted.is_ok(),
            "muting {} mid-write left the fixture daemon {daemon} and answered {muted:?}",
            pane.id(),
        );
    }

    for probe in 0..10 {
        let answered = commands
            .send(Command::new("display-message").arg("-p").arg("up"))
            .await;
        let daemon = guard.daemon_state();
        assert!(
            daemon.is_running() && answered.is_ok(),
            "probe {probe} after muting left the fixture daemon {daemon} and \
             answered {answered:?}",
        );
    }

    for pane in &noisy {
        let unmuted = commands.unmute_pane(pane.id()).await;
        let daemon = guard.daemon_state();
        assert!(
            daemon.is_running() && unmuted.is_ok(),
            "unmuting {} left the fixture daemon {daemon} and answered {unmuted:?}",
            pane.id(),
        );
    }

    drop(commands);
    reader.abort();
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Wait for the next report from one subscription, ignoring everything else.
///
/// tmux coalesces reports to at most once a second, so a caller watching for
/// one is waiting on that interval rather than on the change itself.
async fn next_report(events: &mut ControlEvents, name: &str, within: Duration) -> Option<String> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let event = tokio::time::timeout_at(deadline, events.next_event())
            .await
            .ok()??;
        if let Event::SubscriptionChanged {
            name: reported,
            value,
            ..
        } = event
            && reported.as_str().is_ok_and(|reported| reported == name)
        {
            return value.as_str().ok().map(str::to_owned);
        }
    }
}

/// A subscription must report the format it was given, under the name it was
/// given, when the thing it names changes.
///
/// `Event::SubscriptionChanged` was parsed long before anything could cause
/// one: a caller could receive the event and had no way to ask for it. This is
/// the round trip that closes that.
#[tokio::test]
async fn a_subscription_reports_a_change_under_its_own_name() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("watched").await.expect("session");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    commands
        .subscribe("title", &Subscription::Session, "#{session_name}")
        .await
        .expect("the subscription is accepted");

    assert_eq!(
        next_report(&mut events, "title", Duration::from_secs(15)).await,
        Some("watched".to_owned()),
        "the first report carries the value as it already is"
    );

    commands
        .send(Command::new("rename-session").arg("renamed"))
        .await
        .expect("the session is renamed");

    assert_eq!(
        next_report(&mut events, "title", Duration::from_secs(15)).await,
        Some("renamed".to_owned()),
        "a change is reported under the same name"
    );

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Removing a subscription must stop its reports.
///
/// tmux removes one when the name is given with no colon after it, which is
/// why this cannot be spelled as a subscribe with an empty format: that
/// replaces the subscription rather than removing it.
#[tokio::test]
async fn unsubscribing_stops_the_reports() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("watched").await.expect("session");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    commands
        .subscribe("title", &Subscription::Session, "#{session_name}")
        .await
        .expect("the subscription is accepted");
    assert!(
        next_report(&mut events, "title", Duration::from_secs(15))
            .await
            .is_some(),
        "the subscription reports before it is removed"
    );

    commands
        .unsubscribe("title")
        .await
        .expect("the subscription is removed");
    commands
        .send(Command::new("rename-session").arg("renamed"))
        .await
        .expect("the session is renamed");

    // Three times the interval tmux coalesces to, so a report that was merely
    // slow would have arrived.
    assert_eq!(
        next_report(&mut events, "title", Duration::from_secs(3)).await,
        None,
        "nothing is reported once the subscription is gone"
    );

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// The format is the last field, so colons inside it belong to it.
///
/// tmux splits the argument twice and stops, which is what makes a conditional
/// like `#{?a,b,c}` or a two-part format safe to subscribe to and a colon in
/// the *name* unsafe.
#[tokio::test]
async fn a_subscription_format_may_contain_colons() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("colonised").await.expect("session");

    let (commands, mut events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    commands
        .subscribe(
            "pair",
            &Subscription::Session,
            "#{session_name}:#{session_windows}",
        )
        .await
        .expect("the subscription is accepted");

    let reported = next_report(&mut events, "pair", Duration::from_secs(15))
        .await
        .expect("the subscription reports");
    assert_eq!(
        reported, "colonised:1",
        "both halves of the format arrive, colon and all"
    );

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// A name tmux would read as something else is refused before it is sent.
///
/// tmux accepts `a:b:#{x}` and reports under `a`, and accepts `a:b` as a
/// removal of `a`. Both are silent, so the check has to happen here.
#[tokio::test]
async fn a_subscription_name_tmux_would_misread_is_refused() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("guarded").await.expect("session");

    let (commands, events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    for name in ["with:colon", ""] {
        let refused = commands
            .subscribe(name, &Subscription::Session, "#{session_name}")
            .await
            .expect_err("a name tmux would misread is refused");
        assert_eq!(refused.kind(), libtmux::ErrorKind::InvalidInput);

        let refused = commands
            .unsubscribe(name)
            .await
            .expect_err("removal refuses the same names");
        assert_eq!(refused.kind(), libtmux::ErrorKind::InvalidInput);
    }

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// The pause threshold and the resume that answers it must both dispatch.
///
/// `pause_after` asks tmux to pause a pane rather than disconnect a client
/// that falls behind, and `resume_pane` is what restarts one it paused --
/// which is a different thing from `unmute_pane`, that being the counterpart
/// to a mute the caller asked for.
///
/// What is covered here is that both are built and accepted. Driving a real
/// pause means falling far enough behind for tmux to notice, which is a
/// wall-clock race, and a test that sometimes does not pause would report a
/// broken resume as a passing one.
#[tokio::test]
async fn the_pause_threshold_and_its_resume_are_accepted() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("paused").await.expect("session");
    let pane = session.panes().await.expect("panes").remove(0);

    let (commands, events) = ControlMode::attach(server, session.id())
        .await
        .expect("control mode attaches")
        .split();

    commands
        .pause_after(Duration::from_secs(1))
        .await
        .expect("the pause threshold is accepted");

    // A pane tmux never paused is not an error to resume: the flag says what
    // the stream should do from here, not what it was doing.
    commands
        .resume_pane(pane.id())
        .await
        .expect("resuming is accepted");

    drop(commands);
    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}
