//! Control mode against real tmux.

#![cfg(all(feature = "control-mode", feature = "test-support"))]

use std::time::Duration;

use libtmux::control::{ControlEvents, ControlMode, ControlSender, Event};
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
