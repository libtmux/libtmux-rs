//! Thread and cancellation helpers shared by blocking integration tests.
#![allow(dead_code, reason = "each test binary uses a different subset")]
#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use std::time::{Duration, Instant};

#[path = "../../examples/common/mod.rs"]
mod deadline;
#[allow(
    unused_imports,
    reason = "not every integration binary needs a deadline"
)]
pub(crate) use deadline::with_deadline;

pub(crate) fn cancellable(
    server: &libtmux::Server,
) -> (libtmux::Server, libtmux::CancellationToken) {
    let token = libtmux::CancellationToken::new();
    let handle = server.with_cancellation_token(token.clone());
    (handle, token)
}

pub(crate) fn until(within: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "condition did not become ready within {within:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(crate) fn join<T>(task: std::thread::JoinHandle<T>) -> std::thread::Result<T> {
    until(Duration::from_secs(10), || task.is_finished());
    task.join()
}
