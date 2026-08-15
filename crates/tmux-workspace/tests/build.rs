//! Building real tmux workspaces from configuration.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::TmuxText;
use libtmux::plan::Planner;
use libtmux::test::TestServer;
use tmux_workspace::{BuildError, Workspace, WorkspaceBuilder};

fn text(value: &TmuxText) -> String {
    String::from_utf8(value.as_bytes().to_vec()).expect("fixture values are UTF-8")
}

/// The same, for a field tmux may genuinely not report.
fn text_optional(value: Option<&TmuxText>) -> String {
    text(value.expect("tmux reports the value"))
}

#[test]
fn a_bare_command_string_and_a_mapping_both_describe_a_pane() {
    let workspace = Workspace::from_yaml(
        "
session_name: shapes
windows:
  - window_name: mixed
    panes:
      - echo bare
      - shell_command: echo single
      - shell_command:
          - echo first
          - echo second
        focus: true
",
    )
    .expect("configuration parses");

    let panes = &workspace.windows[0].panes;
    assert_eq!(panes.len(), 3);
    assert_eq!(panes[0].shell_commands, ["echo bare"]);
    assert_eq!(panes[1].shell_commands, ["echo single"]);
    assert_eq!(panes[2].shell_commands, ["echo first", "echo second"]);
    assert!(panes[2].focus);
    assert!(!panes[0].focus);
}

#[test]
fn a_window_without_panes_still_has_the_one_tmux_creates() {
    let workspace = Workspace::from_yaml(
        "
session_name: implicit
windows:
  - window_name: alone
",
    )
    .expect("configuration parses");

    assert_eq!(workspace.windows[0].panes.len(), 1);
    assert!(workspace.windows[0].panes[0].shell_commands.is_empty());
}

#[test]
fn a_missing_session_name_is_rejected() {
    let error = Workspace::from_yaml("windows: []").expect_err("session_name is required");
    assert!(matches!(error, tmux_workspace::ConfigError::Invalid { .. },));
}

#[tokio::test]
async fn building_reproduces_the_configured_shape() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let workspace = Workspace::from_yaml(
        "
session_name: dev
windows:
  - window_name: editor
    panes:
      - sleep 300
      - sleep 300
  - window_name: logs
    focus: true
    panes:
      - sleep 300
      - sleep 300
      - sleep 300
",
    )
    .expect("configuration parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("workspace builds");

    assert_eq!(text(session.name()), "dev");
    assert_eq!(session.window_count(), 2);

    let windows = session.try_windows().await.expect("windows list");
    let names: Vec<_> = windows.iter().map(|window| text(window.name())).collect();
    assert_eq!(names, ["editor", "logs"]);
    assert_eq!(windows[0].pane_count(), 2);
    assert_eq!(windows[1].pane_count(), 3);

    // `focus: true` on the second window leaves it selected.
    let active = session
        .active_window()
        .await
        .expect("active window resolves")
        .expect("a session always has an active window");
    assert_eq!(text(active.name()), "logs");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_start_directory_is_inherited_and_overridden() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let root = tempfile::tempdir().expect("temporary directory");
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory");

    let workspace = Workspace::from_yaml(&format!(
        "
session_name: dirs
start_directory: {root}
windows:
  - window_name: inherited
    panes:
      - sleep 300
  - window_name: overridden
    start_directory: {nested}
    panes:
      - sleep 300
",
        root = root.path().display(),
        nested = nested.display(),
    ))
    .expect("configuration parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("workspace builds");

    let windows = session.try_windows().await.expect("windows list");
    let canonical_root = root.path().canonicalize().expect("canonical root");
    let canonical_nested = nested.canonicalize().expect("canonical nested");

    for (window, expected) in windows.iter().zip([canonical_root, canonical_nested]) {
        let panes = window.try_panes().await.expect("panes list");
        assert_eq!(
            text_optional(panes[0].current_path()),
            expected.display().to_string(),
            "window {} starts in its configured directory",
            text(window.name()),
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn building_over_an_existing_session_is_refused() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let workspace = Workspace::from_yaml("session_name: taken").expect("configuration parses");
    let builder = WorkspaceBuilder::new(server);

    builder.build(&workspace).await.expect("first build");
    let error = builder
        .build(&workspace)
        .await
        .expect_err("a second build is refused");

    assert!(
        matches!(error, BuildError::SessionExists { .. }),
        "building into an existing session would interleave windows, got {error:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_window_is_placed_and_started_as_the_file_says() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // window_index, window_shell, and per-window environment are the keys a
    // real tmuxp file uses to pin a layout down. Dropping them silently
    // produces a session that looks right and is not.
    let workspace = Workspace::from_yaml(
        r"
session_name: placed
windows:
  - window_name: second
    window_index: 2
    environment:
      WORKSPACE_WINDOW: yes
    panes:
      - shell_command: sleep 300
  - window_name: first
    window_index: 1
    window_shell: exec sleep 300
",
    )
    .expect("the workspace parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    let windows = session.try_windows().await.expect("windows");
    let mut placed: Vec<_> = windows
        .iter()
        .map(|window| (window.index(), text(window.name())))
        .collect();
    placed.sort_unstable();
    assert_eq!(
        placed,
        [(1, "first".to_owned()), (2, "second".to_owned())],
        "each window is at the index the file gave it, not the order it was created",
    );

    // window_shell replaces the shell, so that window runs exactly the
    // command and nothing had to be typed into it. The file says `exec` so
    // the pane's process is `sleep` rather than a shell waiting on it;
    // whether a shell would exec on its own is an optimization POSIX does not
    // require, and this assertion should not depend on which shell ran.
    let first = windows
        .iter()
        .find(|window| window.index() == 1)
        .expect("the first window");
    let pane = first.try_panes().await.expect("panes").remove(0);
    assert_eq!(pane.current_command().map(text).as_deref(), Some("sleep"));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_pane_starts_with_the_environment_the_file_gives_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let workspace = Workspace::from_yaml(
        r"
session_name: environed
windows:
  - window_name: main
    panes:
      - shell_command: sleep 300
      - environment:
          PANE_MARKER: second-pane
        shell_command: printenv PANE_MARKER; sleep 300
",
    )
    .expect("the workspace parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    let window = session.try_windows().await.expect("windows").remove(0);
    let panes = window.try_panes().await.expect("panes");
    assert_eq!(panes.len(), 2);

    // The pane prints the variable it was started with, so this reads what
    // tmux actually put in the process rather than what was asked for. The
    // search is for the value rather than a whole line, because the pane also
    // echoes a prompt and the command that was typed.
    let printed = libtmux::test::retry_until(std::time::Duration::from_secs(30), async || {
        panes[1].capture().await.is_ok_and(|lines| {
            lines
                .iter()
                .any(|line| line.to_string_lossy().contains("second-pane"))
        })
    })
    .await;
    assert!(printed.is_ok(), "the pane's own environment reached it");

    assert!(
        session
            .environment("PANE_MARKER")
            .await
            .expect("read")
            .is_none(),
        "a pane variable does not leak into the session",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[test]
fn keys_this_parser_ignores_are_reported_rather_than_dropped() {
    let workspace = Workspace::from_yaml(
        "
session_name: partial
plugins:
  - tmuxp_plugin_example
before_script: ./setup.sh
windows: []
",
    )
    .expect("configuration parses");

    // Loading a richer tmuxp file still works, but the caller can say what
    // was left out instead of finding out later.
    assert_eq!(workspace.unsupported_keys, ["plugins", "before_script"]);
}

#[test]
fn tmuxp_writes_booleans_as_bools_and_as_strings() {
    let workspace = Workspace::from_yaml(
        "
session_name: bools
suppress_history: true
windows:
  - window_name: one
    focus: 'true'
    panes:
      - shell_command: echo one
        enter: false
",
    )
    .expect("configuration parses");

    assert!(workspace.suppress_history);
    assert!(workspace.windows[0].focus, "a quoted true is still true");
    assert!(!workspace.windows[0].panes[0].enter);
}

#[test]
fn options_and_environment_accept_the_scalar_shapes_tmuxp_writes() {
    let workspace = Workspace::from_yaml(
        "
session_name: scalars
environment:
  EDITOR: vim
options:
  base-index: 1
  status: true
global_options:
  history-limit: 5000
",
    )
    .expect("configuration parses");

    assert_eq!(workspace.environment, [("EDITOR".into(), "vim".into())]);
    // A number and a bool both become the text tmux expects.
    assert_eq!(
        workspace.options,
        [
            ("base-index".to_owned(), "1".to_owned()),
            ("status".to_owned(), "on".to_owned()),
        ],
    );
    assert_eq!(
        workspace.global_options,
        [("history-limit".to_owned(), "5000".to_owned())],
    );
}

#[tokio::test]
async fn building_applies_environment_and_options() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let workspace = Workspace::from_yaml(
        "
session_name: configured
environment:
  LIBTMUX_WORKSPACE: applied
options:
  base-index: 3
windows:
  - window_name: only
    options:
      main-pane-width: 42
    panes:
      - sleep 300
",
    )
    .expect("configuration parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("workspace builds");

    assert!(matches!(
        session.environment("LIBTMUX_WORKSPACE").await.expect("read"),
        Some(libtmux::EnvironmentEntry::Set(value)) if value.as_bytes() == b"applied",
    ));
    assert_eq!(
        session
            .get_option("base-index")
            .await
            .expect("read")
            .expect("the option is set")
            .as_bytes(),
        b"3",
    );

    let window = session
        .try_windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");
    assert_eq!(
        window
            .get_option("main-pane-width")
            .await
            .expect("read")
            .expect("the option is set")
            .as_bytes(),
        b"42",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_workspace_is_inspectable_before_it_touches_tmux() {
    let workspace = Workspace::from_yaml(
        "
session_name: previewed
windows:
  - window_name: editor
    panes:
      - vim
      - htop
  - window_name: logs
    panes:
      - tail -f /dev/null
",
    )
    .expect("the workspace parses");

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let builder = WorkspaceBuilder::new(guard.server());
    let plan = builder.plan(&workspace);

    // Every object a later step addresses is a forward reference, so the whole
    // file lowers without asking tmux for a single id first.
    assert!(
        plan.len() > 5,
        "the file describes real work: {}",
        plan.len()
    );
    assert!(
        plan.preview()[0]
            .as_ref()
            .is_some_and(|command| command.summary().to_string().contains("new-session")),
        "the first command is known before anything runs",
    );

    // Grouping is a choice the caller can price, and it is not free to ignore:
    // folding costs fewer tmux processes than one per operation.
    let sequential = Planner::Sequential.steps(&plan).len();
    let marked = Planner::Marked.steps(&plan).len();
    assert_eq!(sequential, plan.len());
    assert!(
        marked < sequential,
        "folding is cheaper: {marked} against {sequential}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
