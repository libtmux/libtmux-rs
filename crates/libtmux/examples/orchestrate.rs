//! Run several jobs in panes and wait for all of them, without polling.
//!
//! ```console
//! $ cargo run --example orchestrate --features test-support
//! ```
//!
//! The shape most orchestration wants: start work in more than one pane, find
//! out when each finishes, and collect what it produced. The waiting is the
//! part that is easy to get wrong, so it is the part this shows.
//!
//! Each job ends with `tmux wait-for -S <channel>`, and this waits on that
//! channel. Nothing polls and nothing sleeps: tmux releases the wait when the
//! job signals it. A job that finishes before the wait starts is fine, because
//! tmux keeps the signal -- which is what makes this safe to write in the
//! obvious order rather than having to start every watcher first.
//!
//! The alternative, capturing a pane in a loop until its output looks done,
//! costs a command per attempt and cannot tell "still running" from "finished
//! and printed nothing".

use std::time::Duration;

use libtmux::test::unique_name;
use libtmux::{ChannelWait, NewWindowOptions, Server};

/// What to run, and the name it answers to.
const JOBS: [(&str, &str); 3] = [
    ("build", "printf 'built 3 targets\\n'"),
    ("test", "printf '12 passed\\n'"),
    ("lint", "printf 'no findings\\n'"),
];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An example must not build sessions on whatever server the reader happens
    // to be using, so this one gets a socket of its own, in a directory this
    // project owns rather than straight into the temporary directory.
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let socket = root.join(format!("{}.sock", unique_name("libtmux-orchestrate")));
    let server = Server::builder().socket_path(&socket).build()?;
    println!("server on {}", socket.display());

    // A handle to wait through. `with_session` borrows the server for the
    // whole scope, and `Session` does not carry one, so the waiter is cloned
    // out first -- a `Server` is a handle, and cloning shares the connection
    // rather than starting a second one.
    let waiter = server.clone();

    // The scope kills the session whether the body succeeds or fails, so a
    // job that misbehaves does not leave a session behind.
    let finished = server
        .with_session(unique_name("orchestrate").as_str(), async |session| {
            let mut channels = Vec::new();

            for (name, work) in JOBS {
                // The channel name has to be unique on the server, not just in
                // this session: a channel is a name the whole server shares.
                let channel = unique_name(&format!("done-{name}"));

                // `wait-for -S` runs inside the pane, where tmux finds its own
                // server through `$TMUX`. Held together with `;` rather than
                // `&&` so a failing job still reports that it ended -- an
                // orchestrator wants to hear about a failure, not wait out its
                // deadline.
                let script = format!("{work}; tmux wait-for -S {channel}");
                let window = session
                    .new_window(NewWindowOptions::new(name).command(script))
                    .await?;

                println!("  {name} started in window {}", window.id());
                channels.push((name, channel));
            }

            // Every job is running by now. Waiting for them in order costs
            // nothing extra: the last one to finish sets the total, and the
            // ones that finished already have their signals waiting.
            let mut finished = 0;
            for (name, channel) in channels {
                match waiter
                    .wait_for_channel(&channel, Duration::from_secs(10))
                    .await?
                {
                    ChannelWait::Signalled => {
                        println!("  {name} finished");
                        finished += 1;
                    }
                    // A deadline is an outcome rather than an error, so an
                    // orchestrator can report the job that hung and still act
                    // on the ones that did not.
                    _ => println!("  {name} did not finish in time"),
                }
            }

            // `with_session` is generic over the body's error, so the body
            // has to name one. Everything here fails as a `libtmux::Error`.
            Ok::<_, libtmux::Error>(finished)
        })
        .await?;

    println!("{finished} of {} jobs finished", JOBS.len());

    // tmux does not unlink its socket when the server exits, so whatever named
    // one owns removing it.
    server.kill().await?;
    std::fs::remove_file(&socket).ok();
    println!("server gone, socket removed");
    Ok(())
}
