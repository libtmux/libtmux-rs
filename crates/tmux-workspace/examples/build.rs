//! Read a workspace, see what it would do, build it, and read it back.
//!
//! The command does all of this for a file; a program does it with the
//! library when the workspace is data it made itself. Everything runs against
//! an isolated tmux under `/tmp/libtmux-rs-test/`, removed at the end.
//!
//! ```console
//! $ cargo run --example build
//! ```

use libtmux::test::TestServer;
use tmux_workspace::{Workspace, WorkspaceBuilder, freeze};

const WORKSPACE: &str = "
session_name: api
windows:
  - window_name: editor
    layout: even-horizontal
    panes:
      - echo editing
      - echo tests
  - window_name: logs
    panes:
      - echo tailing
";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = Workspace::from_yaml(WORKSPACE)?;
    let guard = TestServer::new().await?;
    let builder = WorkspaceBuilder::new(guard.server());

    // Every step the build will take, before any of them runs. A step whose
    // target is created by an earlier one has no id to show until then.
    let plan = builder.plan(&workspace);
    println!("{} steps", plan.len());
    for (step, rendered) in plan.steps().iter().zip(plan.preview()) {
        match rendered {
            Some(command) => println!("  {}", command.summary()),
            None => println!(
                "  {} on an object an earlier step creates",
                step.kind().name()
            ),
        }
    }

    let session = builder.build(&workspace).await?;
    for window in session.windows().await? {
        let panes = window.panes().await?.len();
        let noun = if panes == 1 { "pane" } else { "panes" };
        println!("built {}: {panes} {noun}", window.name().to_string_lossy());
    }

    // What is running now, as a workspace again: the shape, not the history.
    println!("{}", freeze(&session).await?.to_yaml());

    guard.shutdown().await?;
    Ok(())
}
