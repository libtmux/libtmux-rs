//! Budgets that bound what one server may consume.
//!
//! These assert the ceiling exists and is reported, which is the whole point:
//! an unbounded read lets tmux decide how much memory this process uses, and
//! an unbounded dispatch count lets the caller's fan-out decide how many
//! processes the machine runs.

#![cfg(feature = "test-support")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::{Duration, Instant};

use libtmux::test::TestServer;
use libtmux::{Command, DispatchLimits, OutputLimits};

#[tokio::test]
async fn output_past_the_budget_fails_rather_than_arriving_short() {
    let guard = TestServer::builder()
        // Above an ordinary listing row -- the crate's own snapshot projection
        // is a few hundred bytes, and a budget under that breaks every
        // command -- and far below the payload asked for next.
        .output_limits(OutputLimits::default().max_stdout_bytes(4096))
        .start()
        .await
        .expect("tmux starts");
    let server = guard.server();
    server.new_session("budget").await.expect("session");

    // Comfortably inside the budget.
    let small = server
        .cmd(Command::new("display-message").arg("-p").arg("short"))
        .await
        .expect("a small answer is read");
    assert_eq!(small.stdout_lossy().trim_end(), "short");

    // Past it. A buffer loaded from a file is the deterministic way to make
    // tmux emit a lot: a large *argument* is refused by tmux itself, and
    // `run-shell` output is unavailable on some releases.
    let payload = std::env::temp_dir().join("libtmux-rs-test/limits-payload");
    std::fs::write(&payload, vec![b'x'; 16 * 1024]).expect("payload is written");
    server
        .cmd(
            Command::new("load-buffer")
                .arg("-b")
                .arg("budget")
                .arg(payload.as_os_str()),
        )
        .await
        .expect("the buffer loads");
    let _ = std::fs::remove_file(&payload);

    let error = server
        .cmd(Command::new("show-buffer").arg("-b").arg("budget"))
        .await
        .expect_err("an answer past the budget is refused");

    assert!(
        matches!(&error, libtmux::Error::OutputLimitExceeded { stream, limit, .. }
            if *stream == "stdout" && *limit == 4096),
        "got {error:?}",
    );

    // Truncation would have been worse than failing: a shortened listing
    // decodes cleanly and reports fewer objects than exist.
    assert_eq!(error.kind(), libtmux::ErrorKind::Refused);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_server_runs_no_more_dispatches_at_once_than_it_admits() {
    // Concurrency is not observable from outside the executor, so it is
    // measured by the clock: twelve dispatches that each hold a tmux client
    // for ~120ms cannot finish in less than six rounds through two permits.
    // Unbounded, they would all overlap and finish in roughly one round.
    const TASKS: usize = 12;
    const PERMITS: usize = 2;
    const HELD: Duration = Duration::from_millis(120);

    let guard = TestServer::builder()
        .dispatch_limits(DispatchLimits::default().max_in_flight(PERMITS))
        .start()
        .await
        .expect("tmux starts");
    let server = guard.server();
    server.new_session("admission").await.expect("session");

    let started = Instant::now();
    let mut handles = Vec::new();
    for _ in 0..TASKS {
        let server = server.clone();
        handles.push(tokio::spawn(async move {
            server
                .cmd(Command::new("run-shell").arg("sleep 0.12"))
                .await
                .map(|_| ())
        }));
    }
    for handle in handles {
        handle.await.expect("task joins").expect("dispatch runs");
    }
    let elapsed = started.elapsed();

    let rounds = TASKS.div_ceil(PERMITS);
    let floor = HELD * u32::try_from(rounds).expect("a small round count") / 2;
    assert!(
        elapsed >= floor,
        "twelve {HELD:?} dispatches through {PERMITS} permits took {elapsed:?}, \
         which is less than {floor:?}: they were not being admitted two at a time",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_full_server_says_so_instead_of_waiting_forever() {
    let guard = TestServer::builder()
        .dispatch_limits(
            DispatchLimits::default()
                .max_in_flight(1)
                .acquire_timeout(Some(Duration::from_millis(50))),
        )
        .start()
        .await
        .expect("tmux starts");
    let server = guard.server();
    server.new_session("overload").await.expect("session");

    // One long dispatch holds the only permit.
    let holder = {
        let server = server.clone();
        tokio::spawn(async move {
            server
                .cmd(Command::new("run-shell").arg("sleep 1"))
                .await
                .map(|_| ())
        })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;

    let error = server
        .cmd(Command::new("display-message").arg("-p").arg("hello"))
        .await
        .expect_err("the server is full");

    // Overload is not a timeout: nothing reached tmux, so retrying is safe.
    assert!(
        matches!(&error, libtmux::Error::Overloaded { in_flight, .. } if *in_flight == 1),
        "got {error:?}",
    );

    holder.await.expect("the holder joins").expect("it ran");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "control-mode")]
#[tokio::test]
async fn a_control_mode_frame_past_its_budget_ends_the_connection() {
    use libtmux::ControlLimits;
    use libtmux::control::ControlMode;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("frames").await.expect("session");

    // Small enough that an ordinary command's own response block exceeds it,
    // which is the point: the budget is the only thing bounding a connection
    // that reads from a process which keeps running.
    let control = ControlMode::attach_with_limits(
        server,
        session.id(),
        ControlLimits::default().max_block_bytes(16),
    )
    .await;

    // Attaching itself reads tmux's opening block, so a 16-byte budget may
    // fail there; either way the connection refuses rather than growing.
    match control {
        Err(error) => assert!(
            matches!(&error, libtmux::Error::ControlModeFrameTooLarge { frame, limit, .. }
                if *frame == "block" && *limit == 16),
            "got {error:?}",
        ),
        Ok(control) => {
            let error = control
                .send(Command::new("list-panes").arg("-a"))
                .await
                .expect_err("the answer is past the budget");
            assert!(
                matches!(&error, libtmux::Error::ControlModeFrameTooLarge { .. }),
                "got {error:?}",
            );
            assert_eq!(error.kind(), libtmux::ErrorKind::Decode);
            let _ = control.shutdown().await;
        }
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}
