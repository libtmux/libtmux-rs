//! Tell the failures apart when something disappears under you.
//!
//! ```console
//! $ cargo run --example recover --features test-support
//! ```
//!
//! Panes and windows go away while a program is holding them: somebody types
//! `exit`, a build script kills a pane, a session is torn down. The question a
//! caller has to answer is not "did this fail" but "is this worth trying
//! again", and the answers look alike until you ask.
//!
//! This makes each failure happen rather than describing it, because the arms
//! that matter are the ones a healthy server never reaches.

use libtmux::test::unique_name;
use libtmux::{NewWindowOptions, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An example must not build sessions on whatever server the reader happens
    // to be using, so this one gets a socket of its own, in a directory this
    // project owns rather than straight into the temporary directory.
    let root = std::path::Path::new("/tmp/libtmux-rs-dev");
    std::fs::create_dir_all(root)?;
    let socket = root.join(format!("{}.sock", unique_name("libtmux-recover")));
    let server = Server::builder().socket_path(&socket).build()?;
    println!("server on {}", socket.display());

    let home = server.new_session(unique_name("home").as_str()).await?;

    // A window killed out from under a handle. The handle still names it, and
    // the name is now the only thing left.
    let doomed = home.new_window(NewWindowOptions::new("doomed")).await?;
    let doomed_id = doomed.id().clone();
    // Kept deliberately: `kill` consumes the handle, and the point here is
    // what a handle that outlived its window answers.
    let stale = doomed.clone();
    doomed.kill().await?;

    match server.window_by_id(&doomed_id).await {
        Ok(Some(_)) => println!("  still there, which would be a bug"),
        Ok(None) => println!("  {doomed_id} is gone: look it up again or give up"),
        Err(error) if error.is_transient() => println!("  worth retrying: {error}"),
        Err(error) => return Err(error.into()),
    }

    // Refreshing a handle to it says the same thing in the form a caller
    // usually meets: an error whose kind is the decision.
    let refreshed = stale.refreshed().await;
    match refreshed {
        Ok(_) => println!("  refreshed a killed window, which would be a bug"),
        Err(error) => println!(
            "  refresh says gone={}, retryable={}",
            error.is_object_gone(),
            error.is_transient()
        ),
    }

    // A link is not an object, and this is the distinction that costs people.
    // One window, linked into two sessions: dropping one link leaves the
    // window running in the other, so "the link is gone" must not read as
    // "the window is gone" or a caller discards a handle that still works.
    let shared = home.new_window(NewWindowOptions::new("shared")).await?;
    let elsewhere = server
        .new_session(unique_name("elsewhere").as_str())
        .await?;
    shared.link_to(&elsewhere, None).await?;

    let links = server.windows().await?;
    let count = links.iter().filter(|w| w.id() == shared.id()).count();
    println!("  {} is linked {count} times", shared.id());

    let from_home = elsewhere
        .windows()
        .await?
        .into_iter()
        .find(|w| w.id() == shared.id())
        .ok_or("the shared window is linked here")?;
    from_home.unlink().await?;

    // Unlinking the same link twice: the second attempt fails, and what it
    // says decides whether the caller should stop.
    let again = elsewhere
        .windows()
        .await?
        .into_iter()
        .find(|w| w.id() == shared.id());
    if again.is_some() {
        println!("  still linked, which would be a bug");
    } else {
        let alive = server.window_by_id(shared.id()).await?.is_some();
        println!("  the link is gone and the window is alive: {alive}");
    }

    home.kill().await?;
    elsewhere.kill().await?;
    server.kill().await?;
    std::fs::remove_file(&socket).ok();
    println!("told three failures apart, server gone, socket removed");
    Ok(())
}
