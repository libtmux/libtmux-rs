//! One workload, every execution mode, side by side.
//!
//! The same plan runs six ways. What changes is the price and what the run
//! can prove; what does not change is the tmux state it leaves or the query
//! that reads it back. Run it with:
//!
//! ```console
//! $ cargo run --example matrix \
//!     --features plan,control-mode,blocking,test-support,query
//! ```
//!
//! The `processes` column is counted, not asserted: every tmux the example
//! starts goes through a wrapper that logs each start before it runs the real
//! executable, and a row reports how many lines its run added.

#![allow(clippy::expect_used, clippy::print_stdout, reason = "an example")]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use libtmux::plan::{
    Attribution, NewSession, NewWindow, Outcome, Plan, Planner, SelectPane, SendKeys, SetOption,
};
use libtmux::query::{Filterable as _, QueryIteratorExt as _};
use libtmux::test::{TestServer, TestServerBuilder};
use libtmux::{Server, blocking};

/// One row of the comparison.
struct Row {
    mode: &'static str,
    feature: &'static str,
    dispatches: usize,
    processes: usize,
    elapsed: Duration,
    attribution: &'static str,
    query: String,
}

/// A `tmux` that logs each start, then runs the real one in its place.
///
/// The log gains one line per process, whatever its arguments hold, so a
/// mode that fell back to a subprocess, or spent two where it claims one,
/// changes the table rather than only the prose around it.
struct Witness {
    directory: tempfile::TempDir,
}

impl Witness {
    fn new() -> Self {
        let real = real_tmux();
        let directory = tempfile::tempdir().expect("a scratch directory");
        let witness = Self { directory };
        let script = format!(
            "#!/bin/sh\nprintf 'start\\n' >> '{log}'\nexec '{real}' \"$@\"\n",
            log = witness.log().display(),
            real = real.display(),
        );
        std::fs::write(witness.executable(), script).expect("the wrapper is written");
        std::fs::set_permissions(witness.executable(), std::fs::Permissions::from_mode(0o755))
            .expect("the wrapper is executable");
        std::fs::write(witness.log(), "").expect("the log starts empty");
        witness
    }

    fn executable(&self) -> PathBuf {
        self.directory.path().join("tmux")
    }

    fn log(&self) -> PathBuf {
        self.directory.path().join("started.log")
    }

    /// A fixture whose every tmux, daemon and clients alike, is this wrapper.
    fn server(&self) -> TestServerBuilder {
        TestServer::builder().tmux_executable(self.executable())
    }

    /// How many tmux processes have started so far.
    fn started(&self) -> usize {
        std::fs::read_to_string(self.log())
            .expect("the log is readable")
            .lines()
            .count()
    }
}

/// The tmux the fixture would have run: `LIBTMUX_TEST_TMUX`, else `PATH`.
fn real_tmux() -> PathBuf {
    if let Some(pinned) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        return PathBuf::from(pinned);
    }
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|directory| directory.join("tmux"))
        .find(|candidate| Path::is_file(candidate))
        .expect("tmux is on PATH")
}

/// The workload: build a session, split it, decorate the new pane, focus back.
///
/// It is deliberately ordinary. The point is not that it is clever but that
/// every mode below produces the same tmux state from it.
fn workload(name: &str) -> Plan {
    let mut plan = Plan::new();
    let session = plan.add(NewSession::new(name).window_name("editor"));
    // Focused, so the creation can share its invocation with the two
    // operations that decorate its pane.
    let window = plan.add(NewWindow::new(session).name("build").focus());
    plan.add(SendKeys::new(window.pane()).text("# built").enter());
    plan.add(SendKeys::new(window.pane()).text("# ready").enter());
    plan.add(SetOption::window(
        session.window(),
        "synchronize-panes",
        "off",
    ));
    plan.add(SelectPane::new(session.pane()));
    plan
}

/// Read the built session back the same way in every mode.
///
/// This is the "query output" column: if a mode changed what was built, this
/// is where it would show.
async fn query(server: &Server, session: &str) -> String {
    // Scoped to the session the workload built: the control-mode rows need a
    // session of their own to attach to, and counting the whole server would
    // compare that fixture rather than the work.
    let session = server
        .session(session)
        .await
        .expect("the session is readable")
        .expect("the workload built its session");
    let windows = session.windows().await.expect("windows list");
    let mut panes = Vec::new();
    for window in &windows {
        panes.extend(window.panes().await.expect("panes list"));
    }

    let fields = libtmux::Pane::filter_fields();
    let active = panes.iter().matching(&fields.pane_active.eq(true)).count();
    format!(
        "{} panes, {} windows, {active} active",
        panes.len(),
        windows.len()
    )
}

/// How each outcome column is summarised.
fn fidelity(outcomes: impl IntoIterator<Item = Outcome>, attribution: Attribution) -> &'static str {
    if outcomes
        .into_iter()
        .any(|outcome| outcome == Outcome::Unknown)
    {
        return "unknown";
    }
    match attribution {
        Attribution::PerCommand => "per-command",
        Attribution::Merged => "merged",
    }
}

async fn run_subprocess(mode: &'static str, planner: Planner, name: &str) -> Row {
    let witness = Witness::new();
    let guard = witness.server().start().await.expect("tmux starts");
    let server = guard.server();

    let plan = workload(name);
    let before = witness.started();
    let started = Instant::now();
    let result = plan.run(server, planner).await.expect("the plan runs");
    let elapsed = started.elapsed();
    let processes = witness.started() - before;

    let attribution = result
        .steps()
        .iter()
        .map(libtmux::plan::StepOutcome::attribution)
        .fold(Attribution::PerCommand, |worst, next| {
            if next == Attribution::Merged {
                next
            } else {
                worst
            }
        });
    let row = Row {
        mode,
        feature: "plan",
        dispatches: result.dispatches(),
        processes,
        elapsed,
        attribution: fidelity(
            result
                .operations()
                .iter()
                .map(libtmux::plan::OperationReport::outcome),
            attribution,
        ),
        query: query(server, name).await,
    };

    guard.shutdown().await.expect("tmux shuts down");
    row
}

/// Which way a plan reaches tmux over one control-mode connection.
#[derive(Clone, Copy)]
enum Connection {
    /// `Plan::run_over_control_mode`, a plan-only route.
    Streaming,
    /// `Plan::run` on a handle from `Server::over_control_mode`, the route
    /// the whole typed API can take.
    Routed,
}

async fn run_control_mode(connection: Connection, name: &str) -> Row {
    let witness = Witness::new();
    let guard = witness.server().start().await.expect("tmux starts");
    let server = guard.server();

    // Control mode attaches to a session, so the plan cannot be the thing that
    // creates its own connection. One session is made first; the workload then
    // builds its own beside it.
    let host = server
        .new_session("control-host")
        .await
        .expect("host session");

    // Counted from here, so the connection's own process is in the row.
    let before = witness.started();
    let control = libtmux::control::ControlMode::attach(server, host.id())
        .await
        .expect("control mode attaches");
    let (sender, events) = control.split();

    let plan = workload(name);
    let started = Instant::now();
    let (mode, result) = match connection {
        Connection::Streaming => (
            "control-mode/streaming",
            plan.run_over_control_mode(&sender).await,
        ),
        Connection::Routed => {
            let routed = server
                .over_control_mode(&sender)
                .await
                .expect("the connection reaches this server");
            (
                "control-mode/routed",
                plan.run(&routed, Planner::Sequential).await,
            )
        }
    };
    let result = result.expect("the plan runs");
    let elapsed = started.elapsed();
    let processes = witness.started() - before;

    let row = Row {
        mode,
        feature: "plan,control-mode",
        dispatches: result.dispatches(),
        processes,
        elapsed,
        attribution: fidelity(
            result
                .operations()
                .iter()
                .map(libtmux::plan::OperationReport::outcome),
            Attribution::PerCommand,
        ),
        query: query(server, name).await,
    };

    events.shutdown().await.expect("control mode shuts down");
    guard.shutdown().await.expect("tmux shuts down");
    row
}

fn run_blocking(name: &str) -> Row {
    let runtime = blocking::Runtime::new().expect("a runtime");
    runtime.run(run_subprocess(
        "blocking/sequential",
        Planner::Sequential,
        name,
    ))
}

#[tokio::main]
async fn main() {
    let mut rows = vec![
        run_subprocess("async/sequential", Planner::Sequential, "async-seq").await,
        run_subprocess("async/folded", Planner::Folding, "async-fold").await,
        run_subprocess("async/marked-fold", Planner::Marked, "async-marked").await,
        run_control_mode(Connection::Streaming, "control").await,
        run_control_mode(Connection::Routed, "routed").await,
    ];
    // The blocking runtime owns a reactor, so it cannot be built inside one.
    rows.insert(
        0,
        tokio::task::spawn_blocking(|| run_blocking("blocking-seq"))
            .await
            .expect("blocking run"),
    );

    println!(
        "{:<24} {:<18} {:>10} {:>9} {:>9}  {:<12} query output",
        "mode", "feature", "dispatches", "processes", "wall", "attribution",
    );
    println!("{}", "-".repeat(118));
    for row in &rows {
        println!(
            "{:<24} {:<18} {:>10} {:>9} {:>8.0?}  {:<12} {}",
            row.mode,
            row.feature,
            row.dispatches,
            row.processes,
            row.elapsed,
            row.attribution,
            row.query,
        );
    }

    let queries: Vec<&str> = rows.iter().map(|row| row.query.as_str()).collect();
    println!();
    println!(
        "every mode built the same thing: {}",
        queries.windows(2).all(|pair| pair[0] == pair[1]),
    );
    println!(
        "dispatches ranged {}..{}, processes ranged {}..{}",
        rows.iter().map(|row| row.dispatches).min().unwrap_or(0),
        rows.iter().map(|row| row.dispatches).max().unwrap_or(0),
        rows.iter().map(|row| row.processes).min().unwrap_or(0),
        rows.iter().map(|row| row.processes).max().unwrap_or(0),
    );

    println!();
    what_a_fold_costs_when_something_fails().await;
}

/// The price of sharing an invocation, paid only when something goes wrong.
///
/// Above, every mode succeeded and the folded rows read `merged` -- a shared
/// verdict that happened to be good news. This runs the same two operations
/// with a target tmux will refuse, so the difference is visible.
async fn what_a_fold_costs_when_something_fails() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let absent: libtmux::PaneId = "%999".parse().expect("a pane id");

    println!("the same failure, dispatched two ways:");
    for planner in [Planner::Sequential, Planner::Folding] {
        let mut plan = Plan::new();
        plan.add(libtmux::plan::KillPane::new(absent.clone()));
        plan.add(SendKeys::new(absent.clone()).text("never runs").enter());

        let result = plan.run(server, planner).await.expect("the plan runs");
        println!(
            "  {:<12} {} dispatch(es) -> {:?}",
            format!("{planner:?}"),
            result.dispatches(),
            result
                .operations()
                .iter()
                .map(libtmux::plan::OperationReport::outcome)
                .collect::<Vec<_>>(),
        );
    }
    println!("  Sequential names the failing operation; Folding cannot, because tmux");
    println!("  reports one status for the group whichever member failed.");

    guard.shutdown().await.expect("tmux shuts down");
}
