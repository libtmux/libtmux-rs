//! Reap tmux servers left behind by abandoned test fixtures.
//!
//! A fixture cleans up after itself. One whose process was killed mid-run
//! cannot, and the daemon it leaves keeps a pseudo-terminal per pane. A
//! system has a few thousand, so enough abandoned runs and the next `fork`
//! fails with `No space left on device` somewhere unrelated.
//!
//! ```console
//! $ cargo run --example sweep --features test-support
//! ```
//!
//! Only this crate's own fixtures are considered, and only those whose owning
//! process is gone, so it is safe to run while tests are running.

#![allow(clippy::print_stdout, reason = "an example")]

use std::time::Duration;

fn main() {
    // Zero, because abandonment is decided by whether the owning process is
    // still there rather than by age. A fixture a running test owns is spared
    // however old it is.
    match libtmux::test::reap_abandoned_servers(Duration::ZERO) {
        Ok(reaped) if reaped.is_empty() => println!("nothing to reap"),
        Ok(reaped) => {
            println!("reaped {} abandoned fixtures:", reaped.len());
            for path in reaped {
                println!("  {}", path.display());
            }
        }
        Err(error) => eprintln!("could not read the temporary root: {error:?}"),
    }
}
