//! Building real tmux workspaces from configuration.

// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::TmuxText;
use libtmux::plan::Planner;
use libtmux::test::TestServer;
use tmux_workspace::{
    BuildError, ConfigError, PaneConfig, ShellCommand, Workspace, WorkspaceBuilder,
};

#[tokio::test]
async fn layout_preflight_precedes_workspace_creation() {
    let guard = TestServer::new().await.unwrap();
    let keeper = guard.session("layout-builder-keeper").await.unwrap();
    let workspace = Workspace::from_yaml(
        "session_name: layout-invalid\nwindows:\n- layout: b25d,80x24,0,0,0\n  panes: ['true', 'true']\n",
    ).unwrap();
    assert!(
        !WorkspaceBuilder::new(guard.server())
            .plan(&workspace)
            .is_empty()
    );
    assert!(
        WorkspaceBuilder::new(guard.server())
            .build(&workspace)
            .await
            .is_err()
    );
    let sessions = guard.server().sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id(), keeper.id());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn layout_preflight_empty_layout_keeps_default() {
    let guard = TestServer::new().await.unwrap();
    let workspace = Workspace::from_yaml(
        "session_name: empty-layout\nwindows:\n- layout: ''\n  panes: ['true', 'true']\n",
    )
    .unwrap();
    let session = WorkspaceBuilder::new(guard.server())
        .build(&workspace)
        .await
        .unwrap();
    assert_eq!(session.panes().await.unwrap().len(), 2);
    guard.shutdown().await.unwrap();
}

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
    assert_eq!(panes[0].shell_commands, [ShellCommand::new("echo bare")]);
    assert_eq!(panes[1].shell_commands, [ShellCommand::new("echo single")]);
    assert_eq!(
        panes[2].shell_commands,
        [
            ShellCommand::new("echo first"),
            ShellCommand::new("echo second")
        ]
    );
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
    assert!(matches!(error, ConfigError::Invalid { .. },));
}

#[test]
fn a_session_name_tmux_could_not_address_is_rejected() {
    // tmux stores the name verbatim; `:` and `.` are `-t`'s window and
    // pane separators, so a name with either becomes unaddressable.
    for name in ["a:b", "a.b"] {
        let error = Workspace::from_yaml(&format!("session_name: {name:?}\nwindows: []"))
            .expect_err("an unaddressable session_name should be refused");
        assert!(
            matches!(error, tmux_workspace::ConfigError::Invalid { .. }),
            "{name}: {error:?}"
        );
    }
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
        session.typed_option("base-index").await.expect("read"),
        Some(libtmux::OptionValue::Number(3)),
    );

    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");
    assert_eq!(
        window.typed_option("main-pane-width").await.expect("read"),
        Some(libtmux::OptionValue::from("42")),
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
      - blank
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

    // Typed commands start once each shell reads them.
    let settled = libtmux::test::retry_until(std::time::Duration::from_secs(30), async || {
        let panes = built.panes().await.unwrap_or_default();
        panes.len() == 3
            && panes.iter().skip(1).all(|pane| {
                pane.current_command()
                    .is_some_and(|command| command.to_string_lossy() == "sleep")
            })
    })
    .await;
    assert!(settled.is_ok(), "the typed commands are running");

    let frozen = tmux_workspace::freeze(&built)
        .await
        .expect("the session freezes");

    // A pane at its prompt freezes to no command: recording the shell would
    // start a shell inside it on the way back. A pane running something
    // freezes to that command's name.
    assert!(
        frozen.windows[0].panes[0].shell_commands.is_empty(),
        "{:?}",
        frozen.windows[0].panes[0].shell_commands,
    );
    assert_eq!(
        frozen.windows[0].panes[1].shell_commands,
        [ShellCommand::new("sleep")]
    );

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
    // A dotted marker path would trip session_name's own refusal of `.`,
    // for a reason this test is not about, so it gets a dot-free directory.
    let session_directory = tempfile::Builder::new()
        .prefix("session-name-guard")
        .tempdir()
        .expect("a temporary directory without a dot in its name");
    let session_marker = session_directory.path().join("marker");
    let workspace = Workspace::from_yaml(&format!(
        "
session_name: \"#(touch {0})\"
windows:
  - window_name: \"#(touch {1})\"
    panes:
      - sleep 300
",
        session_marker.display(),
        marker.display(),
    ))
    .expect("configuration parses");

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    assert!(!marker.exists(), "a name from the file ran a command");
    assert!(
        !session_marker.exists(),
        "a name from the file ran a command"
    );

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

/// The line some pane of `session` shows holding exactly `text`, once one does.
///
/// Callers point `default-command` at `cat`, so a pane shows exactly what was
/// typed into it: a command kept out of history is the line that starts with
/// a space.
async fn typed_line(session: &libtmux::Session, text: &str) -> String {
    let mut seen = None;
    let settled = libtmux::test::retry_until(std::time::Duration::from_secs(30), async || {
        for pane in session.panes().await.unwrap_or_default() {
            seen = pane.capture().await.ok().and_then(|lines| {
                lines
                    .iter()
                    .map(|line| line.to_string_lossy().trim_end().to_owned())
                    .find(|line| line.trim_start() == text)
            });
            if seen.is_some() {
                return true;
            }
        }
        false
    })
    .await;
    assert!(settled.is_ok(), "{text:?} reached a pane");
    seen.expect("the line was seen")
}

/// tmuxp keeps a file's commands out of shell history unless it says not to,
/// by typing each with a leading space.
#[tokio::test]
async fn commands_stay_out_of_history_unless_the_file_says_otherwise() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut silent = Workspace::from_yaml(
        "session_name: silent\nwindows:\n  - panes:\n      - typed by default\n",
    )
    .expect("configuration parses");
    // tmuxp's own example: the session records, one window and one pane opt
    // back out.
    let mut example = Workspace::from_yaml(include_str!("fixtures/tmuxp/suppress-history.yaml"))
        .expect("tmuxp's example parses");
    for workspace in [&mut silent, &mut example] {
        workspace
            .global_options
            .push(("default-command".to_owned(), "exec cat".to_owned()));
    }

    let silent = WorkspaceBuilder::new(server)
        .build(&silent)
        .await
        .expect("workspace builds");
    assert_eq!(
        typed_line(&silent, "typed by default").await,
        " typed by default"
    );

    let example = WorkspaceBuilder::new(server)
        .build(&example)
        .await
        .expect("tmuxp's example builds");
    for (command, suppressed) in [
        (r#"echo "window in the history!""#, false),
        (r#"echo "window not in the history!""#, true),
        (r#"echo "session in the history!""#, false),
        (r#"echo "command in the history!""#, false),
        (r#"echo "command not in the history!""#, true),
    ] {
        let line = typed_line(&example, command).await;
        assert_eq!(line.starts_with(' '), suppressed, "{line:?}");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Whether some pane of `session` shows `text`, waiting for it to.
async fn shows(session: &libtmux::Session, text: &str) -> bool {
    libtmux::test::retry_until(std::time::Duration::from_secs(30), async || {
        for pane in session.panes().await.unwrap_or_default() {
            if pane.capture().await.is_ok_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.to_string_lossy().contains(text))
            }) {
                return true;
            }
        }
        false
    })
    .await
    .is_ok()
}

/// `- vim` is tmuxp's commonest pane, and it runs `vim`.
#[tokio::test]
async fn a_bare_command_pane_runs_its_command() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let workspace = Workspace::from_yaml(
        "session_name: bare\nwindows:\n  - panes:\n      - echo ran-$((20+22))\n",
    )
    .expect("configuration parses");

    let session = WorkspaceBuilder::new(guard.server())
        .build(&workspace)
        .await
        .expect("workspace builds");
    // Only the shell's arithmetic prints `ran-42`; the typed text does not.
    assert!(shows(&session, "ran-42").await, "the command ran");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Every example tmuxp ships reads here.
#[test]
fn every_tmuxp_example_parses() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tmuxp");
    let mut read = 0;
    for entry in std::fs::read_dir(&directory).expect("the fixtures are present") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_none_or(|extension| extension != "yaml") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("the fixture reads");
        if let Err(error) = Workspace::from_yaml(&source) {
            panic!("{}: {error}", path.display());
        }
        read += 1;
    }
    assert!(read >= 20, "only {read} examples were found to read");
}

/// tmuxp reads an empty entry, `pane` and `blank` as a pane with no command.
#[test]
fn a_blank_pane_is_a_pane_without_a_command() {
    let minimal = Workspace::from_yaml(include_str!("fixtures/tmuxp/minimal.yaml"))
        .expect("tmuxp's minimal example parses");
    assert_eq!(minimal.windows[0].panes, [PaneConfig::default()]);

    let blank = Workspace::from_yaml(include_str!("fixtures/tmuxp/blank-panes.yaml"))
        .expect("tmuxp's blank-pane example parses");
    let shapes: Vec<Vec<Vec<ShellCommand>>> = blank
        .windows
        .iter()
        .map(|window| {
            window
                .panes
                .iter()
                .map(|pane| pane.shell_commands.clone())
                .collect()
        })
        .collect();
    let enter = || vec![ShellCommand::new("")];
    assert_eq!(
        shapes,
        [
            vec![vec![], vec![], vec![]],
            vec![vec![], vec![], vec![]],
            // An empty string is a command: it presses Enter.
            vec![enter(), enter(), enter()],
            vec![vec![], vec![]],
        ],
    );

    let focus = Workspace::from_yaml(include_str!("fixtures/tmuxp/focus-window-and-panes.yaml"))
        .expect("tmuxp's focus example parses");
    assert!(focus.windows[1].panes[0].shell_commands.is_empty());

    // Only a lone blank is blank: among other commands the word is typed.
    let listed =
        Workspace::from_yaml("session_name: s\nwindows:\n  - panes:\n      - [ls, pane]\n")
            .expect("a pane may be a list of commands");
    assert_eq!(
        listed.windows[0].panes[0].shell_commands,
        [ShellCommand::new("ls"), ShellCommand::new("pane")],
    );
    let message = Workspace::from_yaml(
        "session_name: s\nwindows:\n  - panes:\n      - shell_command: [ls, null]\n",
    )
    .expect_err("tmuxp fails on a null among commands")
    .to_string();
    assert!(
        message.contains("windows[0].panes[0].shell_command[1] is empty among other commands"),
        "{message}",
    );
}

#[tokio::test]
async fn tmuxp_blank_pane_examples_build() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let builder = WorkspaceBuilder::new(guard.server());

    for (source, panes) in [
        (include_str!("fixtures/tmuxp/minimal.yaml"), vec![1]),
        (
            include_str!("fixtures/tmuxp/blank-panes.yaml"),
            vec![3, 3, 3, 2],
        ),
    ] {
        let workspace = Workspace::from_yaml(source).expect("tmuxp's example parses");
        let session = builder.build(&workspace).await.expect("the example builds");
        let mut counts = Vec::new();
        for window in session.windows().await.expect("windows list") {
            counts.push(window.pane_count());
        }
        assert_eq!(counts, panes, "{}", workspace.session_name);
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// tmuxp's per-command form: `cmd` with its own `enter` and sleeps.
#[test]
fn a_command_may_carry_its_own_settings() {
    let seconds = |seconds| Some(std::time::Duration::from_secs(seconds));

    let skip = Workspace::from_yaml(include_str!("fixtures/tmuxp/skip-send.yaml"))
        .expect("tmuxp's skip-send example parses");
    assert_eq!(
        skip.windows[0].panes[0].shell_commands[1],
        ShellCommand {
            cmd: r#"echo "___$((1 + 3))___""#.to_owned(),
            enter: Some(false),
            ..ShellCommand::default()
        },
    );
    let pane_level = Workspace::from_yaml(include_str!("fixtures/tmuxp/skip-send-pane-level.yaml"))
        .expect("tmuxp's pane-level skip-send example parses");
    assert!(pane_level.windows[0].panes.iter().all(|pane| !pane.enter));

    let sleep = Workspace::from_yaml(include_str!("fixtures/tmuxp/sleep.yaml"))
        .expect("tmuxp's sleep example parses");
    let commands = &sleep.windows[0].panes[0].shell_commands;
    assert_eq!(commands[1].sleep_before, seconds(2));
    assert_eq!(commands[3].sleep_after, seconds(2));
    let sleep_pane = Workspace::from_yaml(include_str!("fixtures/tmuxp/sleep-pane-level.yaml"))
        .expect("tmuxp's pane-level sleep example parses");
    assert_eq!(sleep_pane.windows[0].panes[0].sleep_before, seconds(2));
    let venv = Workspace::from_yaml(include_str!("fixtures/tmuxp/sleep-virtualenv.yaml"))
        .expect("tmuxp's virtualenv example parses");
    assert_eq!(
        venv.shell_command_before,
        [ShellCommand {
            cmd: "source .venv/bin/activate".to_owned(),
            sleep_before: seconds(1),
            sleep_after: seconds(1),
            ..ShellCommand::default()
        }],
    );

    for workspace in [skip, pane_level, sleep, sleep_pane, venv] {
        assert_eq!(
            Workspace::from_yaml(&workspace.to_yaml()).expect("the rendered YAML parses"),
            workspace,
        );
    }

    let fractional = Workspace::from_yaml(
        "session_name: s\nwindows:\n  - panes:\n      - shell_command: {cmd: ls, sleep_after: 0.25}\n",
    )
    .expect("a fraction of a second is a sleep");
    assert_eq!(
        fractional.windows[0].panes[0].shell_commands[0].sleep_after,
        Some(std::time::Duration::from_millis(250)),
    );
    let message = Workspace::from_yaml(
        "session_name: s\nwindows:\n  - panes:\n      - shell_command: [{cmd: ls, sleep_before: -1}]\n",
    )
    .expect_err("a negative sleep is refused")
    .to_string();
    assert!(
        message.contains("shell_command[0].sleep_before must be a number of seconds"),
        "{message}",
    );
}

/// `enter: false` types a command and leaves it, and holds for the commands
/// after it until one says otherwise, the `shell_command_before` ones
/// included: that is what tmuxp does.
#[tokio::test]
async fn enter_false_types_without_running_until_a_command_says_otherwise() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let mut workspace = Workspace::from_yaml(
        "
session_name: typed
windows:
  - shell_command_before: [before]
    panes:
      - shell_command:
          - cmd: first
            enter: false
          - second
          - cmd: third
            enter: true
      - enter: false
        shell_command:
          - cmd: last
            enter: true
",
    )
    .expect("configuration parses");
    workspace
        .global_options
        .push(("default-command".to_owned(), "exec cat".to_owned()));

    let session = WorkspaceBuilder::new(guard.server())
        .build(&workspace)
        .await
        .expect("workspace builds");

    // Each command is typed after a space that keeps it out of history, so
    // commands sent without Enter share a line, separated by those spaces.
    assert_eq!(typed_line(&session, "before").await, " before");
    assert_eq!(
        typed_line(&session, "first second third").await,
        " first second third"
    );
    assert_eq!(typed_line(&session, "before last").await, " before last");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// tmuxp expands `~` and variables in names, directories and values, joins a
/// window's relative directory onto the session's, and starts a `.` path
/// from the directory it inherits.
#[test]
fn start_directories_and_names_expand_as_tmuxp_does() {
    let home = std::env::var("HOME").expect("tests run with HOME set");
    let home_path = std::path::PathBuf::from(&home);
    let current = std::env::current_dir().expect("a current directory");
    let workspace = Workspace::from_yaml(
        "
session_name: dirs-${HOME}
start_directory: ~/code
environment:
  WHERE: $HOME/x
windows:
  - window_name: w $TMUX_WORKSPACE_UNSET_VARIABLE
    start_directory: ${HOME}
  - start_directory: src
    panes:
      - start_directory: ./tests
      - start_directory: tests
      - echo $HOME
",
    )
    .expect("configuration parses");

    assert_eq!(workspace.session_name, format!("dirs-{home}"));
    assert_eq!(workspace.start_directory, Some(home_path.join("code")));
    assert_eq!(
        workspace.environment,
        [("WHERE".to_owned(), format!("{home}/x"))]
    );
    let windows = &workspace.windows;
    assert_eq!(
        windows[0].window_name.as_deref(),
        Some("w $TMUX_WORKSPACE_UNSET_VARIABLE"),
        "an unset variable stays as written",
    );
    assert_eq!(windows[0].start_directory, Some(home_path.clone()));
    assert_eq!(
        windows[1].start_directory,
        Some(home_path.join("code/src")),
        "a window's relative directory joins the session's",
    );
    let panes = &windows[1].panes;
    assert_eq!(
        panes[0].start_directory,
        Some(home_path.join("code/src/tests")),
        "a `.` path starts from the directory it inherits",
    );
    assert_eq!(
        panes[1].start_directory,
        Some(current.join("tests")),
        "tmuxp leaves a pane's other relative path to tmux, which starts from here",
    );
    assert_eq!(
        panes[2].shell_commands,
        [ShellCommand::new("echo $HOME")],
        "a command is the pane shell's to expand",
    );

    let top = Workspace::from_yaml("session_name: s\nstart_directory: ./\n")
        .expect("configuration parses");
    assert_eq!(top.start_directory, Some(current));

    let message = Workspace::from_yaml("session_name: s\nstart_directory: ~root/x\n")
        .expect_err("`~name` is refused")
        .to_string();
    assert!(
        message.contains("start_directory starts with `~name`"),
        "{message}"
    );
}

/// The first pane of each window starts where the file says.
async fn first_pane_directories(session: &libtmux::Session) -> Vec<String> {
    let mut directories = Vec::new();
    for window in session.windows().await.expect("windows list") {
        let panes = window.panes().await.expect("panes list");
        directories.push(text_optional(panes[0].current_path()));
    }
    directories
}

fn canonical(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref()
        .canonicalize()
        .expect("the directory exists")
        .display()
        .to_string()
}

#[tokio::test]
async fn a_dot_start_directory_is_relative_to_the_file_it_is_in() {
    let root = tempfile::tempdir().expect("temporary directory");
    std::fs::create_dir_all(root.path().join("nested/deeper")).expect("nested directories");
    let file = root.path().join("workspace.yaml");
    std::fs::write(
        &file,
        "
session_name: relative
start_directory: ./
windows:
  - window_name: file
  - window_name: joined
    start_directory: nested
  - window_name: dotted
    start_directory: ./nested
    panes:
      - start_directory: ./deeper
",
    )
    .expect("the file is written");

    // The tests run from the crate's directory, which has no `nested`, so a
    // path resolved from there lands tmux in its fallback directory.
    let workspace = Workspace::from_file(&file).expect("the file parses");
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = WorkspaceBuilder::new(guard.server())
        .build(&workspace)
        .await
        .expect("workspace builds");

    assert_eq!(
        first_pane_directories(&session).await,
        [
            canonical(root.path()),
            canonical(root.path().join("nested")),
            canonical(root.path().join("nested/deeper")),
        ],
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// tmuxp's own start-directory example, which its window names describe.
///
/// The last window is named for the file's directory, and tmuxp's loader
/// puts it in the session's: a `.` path starts from the directory it would
/// otherwise inherit. This follows the loader.
#[tokio::test]
async fn tmuxp_start_directory_example_builds_where_tmuxp_does() {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tmuxp/start-directory.yaml");
    let workspace = Workspace::from_file(&file).expect("tmuxp's example parses");
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = WorkspaceBuilder::new(guard.server())
        .build(&workspace)
        .await
        .expect("tmuxp's example builds");

    let home = std::env::var("HOME").expect("tests run with HOME set");
    assert_eq!(
        first_pane_directories(&session).await,
        [
            canonical("/var"),
            canonical("/var/log"),
            canonical(home),
            canonical("/bin"),
            canonical("/var"),
        ],
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[test]
fn a_missing_file_is_named() {
    let error = Workspace::from_file("/nonexistent/tmux-workspace.yaml")
        .expect_err("there is no such file");
    assert!(matches!(error, ConfigError::Read { .. }), "{error:?}");
    assert_eq!(
        error.to_string(),
        "cannot read workspace file /nonexistent/tmux-workspace.yaml",
    );
}

/// A file is fixed in an editor, so an error names the line to go to.
#[test]
fn an_error_names_the_line_and_column_to_fix() {
    let syntax = Workspace::from_yaml(
        "session_name: s\nwindows:\n  - window_name: a\n    panes:\n      - vim\n     - htop\n",
    )
    .expect_err("a misindented entry is not YAML");
    assert!(
        matches!(
            syntax,
            ConfigError::Yaml {
                line: 6,
                column: 6,
                ..
            }
        ),
        "{syntax:?}",
    );
    assert_eq!(
        syntax.to_string(),
        "workspace configuration is not valid YAML at line 6, column 6: \
         while parsing a block mapping, did not find expected key",
    );

    for (source, expected) in [
        (
            "session_name: s\nwindows:\n  - window_name: a\n    focus: tru\n",
            "at line 4, column 12: windows[0].focus must be a boolean, found \"tru\"",
        ),
        (
            "session_name: s\nwindows:\n  - {panes: [a, 5]}\n",
            "at line 3, column 17: windows[0].panes[1] must be",
        ),
        // A missing key is reported at the mapping that should hold it.
        ("windows: []\n", "at line 1, column 1: session_name must be"),
        // The parser marks an entry with nothing after its `-` at whatever
        // token follows, two lines down here, rather than at the `-`.
        (
            "session_name: s\nwindows:\n  - window_name: a\n  -\n  -\n  - window_name: b\n",
            "at line 4, column 3: windows[1] must be a mapping",
        ),
    ] {
        let message = Workspace::from_yaml(source)
            .expect_err("the value is refused")
            .to_string();
        assert!(
            message.contains(expected),
            "expected {expected:?} in {message:?}"
        );
    }
}

/// tmuxp's sleeps become pauses around the commands they belong to, and hold
/// for the later commands in the pane as `enter` does. The pause happens in
/// tmux, so a folded build waits too.
#[tokio::test]
async fn sleeps_pause_the_build_between_commands() {
    use std::time::{Duration, Instant};

    use libtmux::plan::Op;

    let workspace = Workspace::from_yaml(
        "
session_name: sleepy
windows:
  - panes:
      - sleep_before: 0.1
        shell_command:
          - echo one
          - cmd: echo two
            sleep_after: 0.05
          - echo three
",
    )
    .expect("configuration parses");

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let builder = WorkspaceBuilder::new(guard.server());

    let shape: Vec<String> = builder
        .plan(&workspace)
        .steps()
        .iter()
        .map(|op| match op {
            Op::Pause(pause) => format!("pause {:?}", pause.duration()),
            other => other.name().to_owned(),
        })
        .collect();
    assert_eq!(
        shape,
        [
            "new-session",
            "new-window",
            "pause 100ms",
            "send-keys",
            "pause 100ms",
            "send-keys",
            "pause 50ms",
            "pause 100ms",
            "send-keys",
            "pause 50ms",
            "kill-window",
        ],
    );

    let started = Instant::now();
    builder
        .build(&workspace)
        .await
        .expect("the workspace builds");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(400),
        "built in {elapsed:?}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn freeze_omits_shell_command_for_the_default_shell_and_lists_others() {
    // The default shell round-trips with shell_command omitted; emitting
    // it would reload the shell as an explicit command inside itself.
    use libtmux::SplitDirection;

    let guard = TestServer::new().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("freeze-shell")
        .await
        .expect("session starts");
    let window = session
        .active_window()
        .await
        .expect("window lookup")
        .expect("a session has a window");
    // Typed rather than passed as split-window's own command: some
    // shells (dash) don't exec-replace a `-c` command, which would
    // leave the wrapping shell as pane_current_command forever.
    let other = window.split(SplitDirection::Below).await.expect("split");
    other.send_line("sleep 300").await.expect("type command");

    let settled = libtmux::test::retry_until(std::time::Duration::from_secs(10), async || {
        let Ok(refreshed) = other.refreshed().await else {
            return false;
        };
        refreshed
            .current_command()
            .is_some_and(|command| command.to_string_lossy() == "sleep")
    })
    .await;
    assert!(settled.is_ok(), "the split pane's command settled");

    let frozen = tmux_workspace::freeze(&session)
        .await
        .expect("the session freezes");

    assert!(
        frozen.windows[0].panes[0].shell_commands.is_empty(),
        "the untouched pane runs the default shell and should omit shell_command: {:?}",
        frozen.windows[0].panes[0].shell_commands
    );
    // pane_current_command is the command name only, not its arguments.
    assert_eq!(
        frozen.windows[0].panes[1].shell_commands,
        [tmux_workspace::ShellCommand::new("sleep")]
    );

    let yaml = frozen.to_yaml();
    assert!(
        yaml.contains("shell_command:\n") && yaml.contains("- \"sleep\""),
        "a single shell_command must render as a YAML list:\n{yaml}"
    );
    assert!(
        !yaml.contains("shell_command: \"sleep\""),
        "shell_command must never render as a bare scalar:\n{yaml}"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn library_builder_places_panes_after_the_first_in_config_order() {
    // `-t <window>` always divides the active pane, which a detached
    // split never changes, so splitting the window repeatedly reverses
    // everything after the first pane.
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let workspace = Workspace::from_yaml(
        "
session_name: libpaneorder
windows:
  - window_name: plain
    panes:
      - printf 'MARK-A\\n'; sleep 300
      - printf 'MARK-B\\n'; sleep 300
      - printf 'MARK-C\\n'; sleep 300
      - printf 'MARK-D\\n'; sleep 300
",
    )
    .expect("the workspace parses");

    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    let window = session.windows().await.expect("windows").remove(0);
    let panes = window.panes().await.expect("panes");
    assert_eq!(panes.len(), 4);

    for (index, (pane, marker)) in panes
        .iter()
        .zip(["MARK-A", "MARK-B", "MARK-C", "MARK-D"])
        .enumerate()
    {
        let seen = libtmux::test::retry_until(std::time::Duration::from_secs(15), async || {
            pane.capture().await.is_ok_and(|lines| {
                lines
                    .iter()
                    .any(|line| line.to_string_lossy().contains(marker))
            })
        })
        .await;
        assert!(seen.is_ok(), "pane index {index} should show {marker}");
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn freeze_recognizes_a_default_shell_whose_running_name_differs() {
    // default-shell is a path, but on macOS /bin/sh runs as bash, so a
    // bare pane's current command differs from its basename.
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server
        .set_global_option("default-command", "/bin/bash -i")
        .await
        .expect("override default-command");

    let workspace =
        Workspace::from_yaml("session_name: shellalias\nwindows:\n  - panes:\n      - blank\n")
            .expect("the workspace parses");
    let session = WorkspaceBuilder::new(server)
        .build(&workspace)
        .await
        .expect("the workspace builds");

    let pane = session.panes().await.expect("panes").remove(0);
    let settled = libtmux::test::retry_until(std::time::Duration::from_secs(10), async || {
        let Ok(refreshed) = pane.refreshed().await else {
            return false;
        };
        refreshed
            .current_command()
            .is_some_and(|command| command.to_string_lossy() == "bash")
    })
    .await;
    assert!(settled.is_ok(), "the pane's command settled to bash");

    let frozen = tmux_workspace::freeze(&session)
        .await
        .expect("the session freezes");

    assert!(
        frozen.windows[0].panes[0].shell_commands.is_empty(),
        "an ordinary interactive shell should omit shell_command even when \
         its reported name differs from default-shell's basename: {:?}",
        frozen.windows[0].panes[0].shell_commands
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
