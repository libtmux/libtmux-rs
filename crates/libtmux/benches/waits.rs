//! What waiting on a pane costs, measured against real tmux.
//!
//! ```console
//! $ cargo bench --features test-support,control-mode --bench waits
//! ```
//!
//! Two lanes answer the same question, so the gap between them is a number
//! rather than an argument. `polled` is [`libtmux::Pane::wait_for_text`],
//! which captures on an interval and is what a default build gets.
//! `streamed` is [`libtmux::Pane::stream_output`], the control-mode `%output`
//! doorbell, timed with its connection already open — the optimistic case for
//! it, because opening one costs a client attach that polling never pays.
//!
//! `flood` is the case the two paths were expected to diverge on: output
//! arriving faster than either can read it.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, criterion_main};
use libtmux::control::PaneOutput;
use libtmux::test::TestServer;
use libtmux::{Pane, PaneWait};

/// How long a wait is given before it is called a failure rather than a
/// measurement. Generous: a sample that times out is a broken bench, not a
/// slow one.
const CEILING: Duration = Duration::from_secs(20);

/// Distinguishes one dispatch from the next, so no wait can be answered by an
/// earlier iteration's output still in the scrollback.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A command to dispatch and the needle that only its *output* contains.
///
/// tmux echoes what is typed into the pane, so a needle appearing in the
/// command matches on the way in. `printf 'RDY-%s\n' 7` shows `RDY-%s`
/// echoed and `RDY-7` printed, and only the second is waited for.
fn dispatch() -> (String, String) {
    let tag = NEXT.fetch_add(1, Ordering::Relaxed);
    (format!("printf 'RDY-%s\\n' {tag}"), format!("RDY-{tag}"))
}

/// Whether `haystack` holds `needle`, which `[u8]` does not answer itself.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// One tmux server holding one shell pane to dispatch into.
struct Fixture {
    guard: TestServer,
    pane: Pane,
}

async fn build() -> Fixture {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("waits")
        .await
        .expect("a session");
    let pane = session.panes().await.expect("panes").remove(0);

    Fixture { guard, pane }
}

/// Dispatch a marker and poll the pane until it arrives.
async fn polled(pane: &Pane) -> Duration {
    let (command, needle) = dispatch();

    let started = Instant::now();
    pane.send_line(command).await.expect("keys are sent");
    let outcome = pane
        .wait_for_text(&needle, CEILING)
        .await
        .expect("waiting is not an error");
    let waited = started.elapsed();

    assert_eq!(outcome, PaneWait::Arrived, "the marker was printed");
    waited
}

/// Dispatch a marker and read the doorbell until it carries the marker.
///
/// The connection is already open, so this times the notification path alone.
async fn streamed(pane: &Pane, output: &mut PaneOutput) -> Duration {
    let (command, needle) = dispatch();

    let started = Instant::now();
    pane.send_line(command).await.expect("keys are sent");

    let mut seen = Vec::new();
    while let Some(chunk) = output.next_chunk().await {
        seen.extend_from_slice(&chunk);
        if contains(&seen, needle.as_bytes()) {
            return started.elapsed();
        }
    }

    panic!("the stream ended before the marker arrived");
}

/// The command that floods a pane with `lines` lines, and the marker printed
/// after the last of them.
///
/// The marker is what makes this a flood measurement rather than a race with
/// the echo: waiting for `200000` would match the `seq 1 200000` tmux echoes
/// before the shell has printed anything at all.
fn flood(lines: u64) -> (String, String) {
    let (_, needle) = dispatch();
    (format!("seq 1 {lines}; printf '%s\\n' {needle}"), needle)
}

/// Print `lines` lines and poll until the marker after the last of them.
async fn flooded(pane: &Pane, lines: u64) -> Duration {
    let (command, needle) = flood(lines);

    let started = Instant::now();
    pane.send_line(command).await.expect("keys are sent");
    let outcome = pane
        .wait_for_text(&needle, CEILING)
        .await
        .expect("waiting is not an error");
    let waited = started.elapsed();

    assert_eq!(
        outcome,
        PaneWait::Arrived,
        "the flood ended with its marker"
    );
    waited
}

/// Print `lines` lines and read the doorbell until the marker arrives.
///
/// This is the case the doorbell was expected to lose: polling costs one
/// capture per interval whatever the pane is doing, while a notification path
/// is handed every byte.
async fn flooded_stream(pane: &Pane, output: &mut PaneOutput, lines: u64) -> Duration {
    let (command, needle) = flood(lines);

    let started = Instant::now();
    pane.send_line(command).await.expect("keys are sent");

    // Only the tail can hold the marker, and keeping the whole flood would
    // measure this bench's own reallocation. A window twice the needle
    // catches it however tmux splits the chunks.
    let mut tail = Vec::new();
    while let Some(chunk) = output.next_chunk().await {
        tail.extend_from_slice(&chunk);
        if contains(&tail, needle.as_bytes()) {
            return started.elapsed();
        }
        let keep = tail.len().saturating_sub(needle.len() * 2);
        tail.drain(..keep);
    }

    panic!("the stream ended before the flood's marker arrived");
}

fn waits(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");

    let fixture = runtime.block_on(build());
    let pane = &fixture.pane;

    let mut group = criterion.benchmark_group("marker");
    // Every sample dispatches a command into a real shell, so the default
    // sample count would spend minutes measuring the same round trip.
    group
        .sample_size(20)
        .measurement_time(Duration::from_secs(20));

    group.bench_function("polled", |bench| {
        bench
            .to_async(&runtime)
            .iter_custom(|iterations| async move {
                let mut total = Duration::ZERO;
                for _ in 0..iterations {
                    total += polled(pane).await;
                }
                total
            });
    });

    // Subscribed inside the batch and closed at the end of it, never held
    // across another lane: a subscription nobody is reading holds tmux back,
    // because the actor stops reading the connection once its queue fills and
    // the pane it watches stops producing. Left open across the polled lanes
    // that turns them into timeouts rather than measurements. Attaching is
    // outside the accumulated span, so it costs wall-clock, not the number.
    group.bench_function("streamed", |bench| {
        bench
            .to_async(&runtime)
            .iter_custom(|iterations| async move {
                let mut output = subscribe(pane).await;
                let mut total = Duration::ZERO;
                for _ in 0..iterations {
                    total += streamed(pane, &mut output).await;
                }
                unsubscribe(output).await;
                total
            });
    });

    group.finish();

    let mut group = criterion.benchmark_group("flood");
    // A sample here prints tens of thousands of lines. Ten of them is already
    // half a minute of tmux writing to a pane.
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(30));

    for lines in [20_000_u64, 200_000] {
        group.bench_with_input(BenchmarkId::new("polled", lines), &lines, |bench, lines| {
            let lines = *lines;
            bench
                .to_async(&runtime)
                .iter_custom(move |iterations| async move {
                    let mut total = Duration::ZERO;
                    for _ in 0..iterations {
                        total += flooded(pane, lines).await;
                    }
                    total
                });
        });

        group.bench_with_input(
            BenchmarkId::new("streamed", lines),
            &lines,
            |bench, lines| {
                let lines = *lines;
                bench
                    .to_async(&runtime)
                    .iter_custom(move |iterations| async move {
                        let mut output = subscribe(pane).await;
                        let mut total = Duration::ZERO;
                        for _ in 0..iterations {
                            total += flooded_stream(pane, &mut output, lines).await;
                        }
                        unsubscribe(output).await;
                        total
                    });
            },
        );
    }

    group.finish();

    runtime
        .block_on(fixture.guard.shutdown())
        .expect("tmux fixture shuts down");
}

/// Open a doorbell on the pane for the batch about to be timed.
async fn subscribe(pane: &Pane) -> PaneOutput {
    pane.stream_output().await.expect("a control connection")
}

/// Close it, so no lane runs beside a subscription nobody is draining.
async fn unsubscribe(output: PaneOutput) {
    output
        .shutdown()
        .await
        .expect("the control connection closes");
}

/// Register the group. Generated by the macro, so it is documented here.
#[allow(missing_docs)]
mod generated {
    criterion::criterion_group!(benches, super::waits);
}

use generated::benches;
criterion_main!(benches);
