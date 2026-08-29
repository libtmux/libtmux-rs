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

/// Where `TestServer` puts its sockets, and so where abandoned ones collect.
const ROOT: &str = "/tmp/libtmux-rs-test";

fn main() {
    // Zero, because abandonment is decided by whether the owning process is
    // still there rather than by age. A fixture a running test owns is spared
    // however old it is.
    match libtmux::test::reap_abandoned_servers(Duration::ZERO) {
        Ok(reaped) if reaped.is_empty() => {
            // The usual answer, and the one that teaches least, so say what
            // would have shown up here rather than stopping at "nothing".
            println!("nothing to reap: every fixture under {ROOT} has a living owner");
            println!();
            println!("A fixture cleans up after itself. One whose process was");
            println!("killed -- SIGKILL, a crashed runner, a closed laptop lid --");
            println!("cannot, and the tmux daemon it left keeps a pseudo-terminal");
            println!("per pane. Run this after a test run that died and it lists");
            println!("what it removed.");
        }
        Ok(reaped) => {
            println!("reaped {} abandoned fixtures:", reaped.len());
            for path in reaped {
                println!("  {}", path.display());
            }
        }
        Err(error) => eprintln!("could not read the temporary root: {error:?}"),
    }
}
