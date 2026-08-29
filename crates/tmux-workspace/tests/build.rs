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

    let windows = session.windows().await.expect("windows list");
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
  - window_name: pane-overridden
    start_directory: {root}
    panes:
      - start_directory: {nested}
        shell_command: sleep 300
",
        root = root.path().display(),
        nested = nested.display(),
    ))
    .expect("configuration parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("workspace builds");

    let windows = session.windows().await.expect("windows list");
    let canonical_root = root.path().canonicalize().expect("canonical root");
    let canonical_nested = nested.canonicalize().expect("canonical nested");

    for (window, expected) in
        windows
            .iter()
            .zip([canonical_root, canonical_nested.clone(), canonical_nested])
    {
        let panes = window.panes().await.expect("panes list");
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
async fn failed_lookup_after_a_completed_build_is_a_partial_effect() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server
        .set_hook("after-new-session", "kill-server")
        .await
        .expect("hook is installed");

    let workspace = Workspace::from_yaml("session_name: committed").expect("configuration parses");
    let error = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect_err("the completed build cannot be looked up");

    let BuildError::Tmux(error) = error else {
        panic!("lookup failure must remain a libtmux error");
    };
    assert_eq!(error.kind(), libtmux::ErrorKind::PartialEffect);

    let libtmux::Error::AfterEffect {
        operation, source, ..
    } = error
    else {
        panic!("the post-build lookup must identify its committed boundary");
    };
    assert_eq!(operation, "workspace-build");
    assert_eq!(source.kind(), libtmux::ErrorKind::ServerGone);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn refusal_after_session_creation_is_a_partial_effect() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let workspace = Workspace::from_yaml(
        "
session_name: committed-refusal
options:
  option-that-tmux-does-not-have: on
",
    )
    .expect("configuration parses");

    let error = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect_err("the option is refused after the session is created");

    let BuildError::Tmux(error) = error else {
        panic!("a refusal after creation must remain a libtmux error");
    };
    assert_eq!(error.kind(), libtmux::ErrorKind::PartialEffect);
    assert!(matches!(
        error,
        libtmux::Error::AfterEffect {
            operation: "workspace-build",
            source,
            ..
        } if source.kind() == libtmux::ErrorKind::Refused
    ));
    assert!(
        server
            .session("committed-refusal")
            .await
            .expect("the server remains queryable")
            .is_some(),
        "the failed build left the session it created",
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

    let windows = session.windows().await.expect("windows");
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
    let pane = first.panes().await.expect("panes").remove(0);
    assert_eq!(pane.current_command().map(text).as_deref(), Some("sleep"));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn panes_start_with_the_environment_the_file_gives_them() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let workspace = Workspace::from_yaml(
        r"
session_name: environed
windows:
  - window_name: main
    panes:
      - environment:
          PANE_MARKER: first-pane
        shell_command: echo first:$PANE_MARKER:$WINDOW_MARKER; sleep 300
      - environment:
          PANE_MARKER: second-pane
        shell_command: echo second:$PANE_MARKER:$WINDOW_MARKER; sleep 300
      - shell_command: echo third:$PANE_MARKER:$WINDOW_MARKER; sleep 300
    environment:
      PANE_MARKER: window
      WINDOW_MARKER: inherited
",
    )
    .expect("the workspace parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    let window = session.windows().await.expect("windows").remove(0);
    let panes = window.panes().await.expect("panes");
    assert_eq!(panes.len(), 3);

    // The pane prints the variable it was started with, so this reads what
    // tmux actually put in the process rather than what was asked for. The
    // search is for the value rather than a whole line, because the pane also
    // echoes a prompt and the command that was typed.
    for marker in [
        "first:first-pane:inherited",
        "second:second-pane:inherited",
        "third:window:inherited",
    ] {
        let printed = libtmux::test::retry_until(std::time::Duration::from_secs(30), async || {
            for pane in &panes {
                if pane.capture().await.is_ok_and(|lines| {
                    lines
                        .iter()
                        .any(|line| line.to_string_lossy().contains(marker))
                }) {
                    return true;
                }
            }
            false
        })
        .await;
        assert!(printed.is_ok(), "{marker} reached its pane");
    }

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
        .windows()
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

    let mut without_configured_panes = workspace.clone();
    without_configured_panes.windows[0].panes.clear();
    assert!(
        builder.plan(&without_configured_panes).len() < plan.len(),
        "a public window with no pane configuration still lowers",
    );

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

/// A value that is present and wrong is a different workspace, not a default.
///
/// tmuxp files are hand-written, and the failure mode being guarded here is
/// quiet: `focus: "tru"` used to build a session that was valid and focused
/// the wrong pane, with nothing to say so.
#[test]
fn a_present_but_invalid_value_is_refused_rather_than_defaulted() {
    for (source, expected) in [
        (
            "session_name: s\nwindows:\n  - focus: \"tru\"\n",
            "windows[0].focus",
        ),
        (
            "session_name: s\nwindows:\n  - panes:\n      - enter: maybe\n",
            "windows[0].panes[0].enter",
        ),
        (
            "session_name: s\nstart_directory: 123\nwindows: []\n",
            "start_directory",
        ),
        (
            "session_name: s\nwindows:\n  - layout: [not, a, string]\n",
            "windows[0].layout",
        ),
        ("session_name: s\nwindows:\n  - scalar\n", "windows[0]"),
    ] {
        let error = Workspace::from_yaml(source).expect_err("the value is refused");
        let message = error.to_string();
        assert!(
            message.contains(expected),
            "the error names where it happened: expected {expected:?} in {message:?}",
        );
    }

    // Absence still defaults, which is the whole distinction.
    let workspace = Workspace::from_yaml("session_name: s\nwindows:\n  - window_name: w\n")
        .expect("an absent value defaults");
    assert!(!workspace.windows[0].focus);
}

#[test]
fn rendered_scalars_round_trip_control_and_line_separator_characters() {
    let mut workspace =
        Workspace::from_yaml("session_name: seed\n").expect("the seed workspace parses");
    let controls = (0_u8..=31)
        .chain(127..=159)
        .map(char::from)
        .chain(['\u{2028}', '\u{2029}'])
        .collect::<String>();
    workspace.session_name = format!("before{controls}after");

    let rendered = workspace.to_yaml();
    let reparsed = Workspace::from_yaml(&rendered).expect("the rendered YAML parses");

    assert_eq!(reparsed, workspace);
    assert!(
        !rendered.chars().any(|character| {
            character != '\n'
                && (character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
        }),
        "rendered scalars escape control characters",
    );
}

/// The two directions have to meet: a session built from a file, frozen back
/// to a workspace, and built again must produce the same shape. Anything the
/// freeze cannot recover shows up here as a difference.
#[tokio::test]
async fn a_session_freezes_back_into_a_workspace_that_rebuilds_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let workspace = Workspace::from_yaml(
        "
session_name: original
windows:
  - window_name: editor
    panes:
      - sleep 400
      - sleep 401
  - window_name: logs
    panes:
      - sleep 402
",
    )
    .expect("the workspace parses");

    let built = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    let frozen = tmux_workspace::freeze(&built)
        .await
        .expect("the session freezes");

    assert_eq!(frozen.session_name, "original");
    assert_eq!(frozen.windows.len(), 2);
    assert_eq!(
        frozen
            .windows
            .iter()
            .map(|window| window.panes.len())
            .collect::<Vec<_>>(),
        vec![2, 1],
    );
    // Exactly one window and one pane per window are focused, because that is
    // what tmux tracks and what a rebuild needs to reproduce.
    assert_eq!(
        frozen.windows.iter().filter(|window| window.focus).count(),
        1,
    );

    // The file it renders is a file this crate reads.
    let yaml = frozen.to_yaml();
    let reparsed = Workspace::from_yaml(&yaml).expect("the rendered YAML parses");
    assert_eq!(reparsed, frozen, "rendering and parsing are inverses");

    // And building from it gives the same shape back.
    let rebuilt_config = Workspace {
        session_name: "rebuilt".to_owned(),
        ..reparsed
    };
    let rebuilt = WorkspaceBuilder::new(server)
        .build(&rebuilt_config)
        .await
        .expect("the frozen workspace rebuilds");

    let rebuilt_windows = rebuilt.windows().await.expect("windows");
    assert_eq!(rebuilt_windows.len(), 2);
    let mut counts = Vec::new();
    for window in &rebuilt_windows {
        counts.push(window.panes().await.expect("panes").len());
    }
    assert_eq!(counts, vec![2, 1], "the rebuilt session has the same shape");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_name_from_the_file_cannot_run_a_command() {
    // tmux expands a name as a format before storing it, so `#(command)` runs
    // a shell. A workspace file is not this program's own text: whoever wrote
    // it would otherwise choose what runs.
    let directory = tempfile::tempdir().expect("a temporary directory");
    let marker = directory.path().join("marker");
    let workspace = Workspace::from_yaml(&format!(
        "
session_name: \"#(touch {0})\"
windows:
  - window_name: \"#(touch {0})\"
    panes:
      - sleep 300
",
        marker.display()
    ))
    .expect("configuration parses");

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    assert!(!marker.exists(), "a name from the file ran a command");

    // The name survives as the text it was, rather than being dropped.
    let windows = session.windows().await.expect("windows");
    assert_eq!(
        text(windows[0].name()),
        format!("#(touch {})", marker.display()),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_start_directory_from_the_file_cannot_run_a_command() {
    // tmux expands the `-c` start directory as a format too, not only a name,
    // so a workspace file could choose what ran through the one field that
    // looks least like text tmux would interpret.
    let directory = tempfile::tempdir().expect("a temporary directory");
    let marker = directory.path().join("marker");
    let real = directory.path().join("work");
    std::fs::create_dir(&real).expect("a directory to start in");

    let workspace = Workspace::from_yaml(&format!(
        "
session_name: dirs
start_directory: \"#(touch {0}){1}\"
windows:
  - window_name: one
    panes:
      - sleep 300
",
        marker.display(),
        real.display(),
    ))
    .expect("configuration parses");

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = WorkspaceBuilder::new(guard.server())
        .build(&workspace)
        .await
        .expect("the workspace builds");

    assert!(
        !marker.exists(),
        "a start directory from the file ran a command",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
    drop(session);
}
