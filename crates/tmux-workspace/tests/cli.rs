//! Installed command grammar and process stream contracts.
#![cfg(feature = "cli")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Output};

#[tokio::test]
async fn outside_tmux_attach_requires_terminal_before_scripts_or_session_mutation() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("terminal-keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let marker = directory.path().join("before-script-ran");
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({
            "session_name":"terminal-workspace", "before_script":"touch before-script-ran",
            "windows":[{"panes":["blank"]}]
        })
        .to_string(),
    )
    .unwrap();
    let socket = guard.socket_path().to_str().unwrap();
    let output = at_pane(
        &["load", "workspace.json", "-S", socket],
        directory.path(),
        None,
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("attaching requires a terminal"));
    assert!(!marker.exists(), "terminal refusal ran the setup script");
    let sessions = guard.server().sessions().await.unwrap();
    assert_eq!(sessions.len(), 1, "terminal refusal created a session");
    assert_eq!(sessions[0].id(), keeper.id());
    assert_eq!(keeper.windows().await.unwrap().len(), 1);
    assert_eq!(current_pane(&keeper).await, pane);
    for append in [false, true] {
        let output = at_pane(
            &[
                "load",
                "workspace.json",
                "-S",
                socket,
                if append { "--append" } else { "-d" },
            ],
            directory.path(),
            append.then_some((&guard, pane.as_str())),
        );
        assert!(output.status.success(), "{output:?}");
        assert!(marker.exists(), "explicit nonattached load did not run");
        std::fs::remove_file(&marker).unwrap();
        if !append {
            guard
                .server()
                .session("terminal-workspace")
                .await
                .unwrap()
                .unwrap()
                .kill()
                .await
                .unwrap();
        }
    }
    assert_eq!(guard.server().sessions().await.unwrap().len(), 1);
    assert_eq!(keeper.windows().await.unwrap().len(), 2);
    guard.shutdown().await.unwrap();
}

/// Inside tmux on the target server, an attached load needs no terminal at
/// all: it switches the client that is already there. A control-mode client
/// is the only kind that attaches without one, which is what makes the
/// accepted context testable beside the refused ones.
#[tokio::test]
async fn inside_tmux_attach_switches_an_attached_client_without_a_terminal() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("terminal-keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let control = libtmux::control::ControlMode::attach(guard.server(), keeper.id())
        .await
        .unwrap();
    wait_for_client(guard.server(), keeper.id().as_ref()).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let marker = directory.path().join("before-script-ran");
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({
            "session_name":"terminal-workspace", "before_script":"touch before-script-ran",
            "windows":[{"panes":["blank"]}]
        })
        .to_string(),
    )
    .unwrap();
    let socket = guard.socket_path().to_str().unwrap();
    let output = at_pane(
        &["load", "workspace.json", "-S", socket],
        directory.path(),
        Some((&guard, pane.as_str())),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    assert!(!stderr.contains("use -d"), "{stderr}");
    assert!(marker.exists(), "inside tmux did not run before_script");
    let session = guard
        .server()
        .session("terminal-workspace")
        .await
        .unwrap()
        .expect("inside tmux did not build the session");
    drop(control);
    session.kill().await.unwrap();
    guard.shutdown().await.unwrap();
}

/// Every way the pane a load was invoked from can fail to be usable: the
/// variable that names the daemon is malformed or names a daemon that is
/// gone, the pane is not a pane or not on this server, and nothing is
/// attached to the pane's session. Each is settled before the first
/// mutation, so none of them leaves a session behind.
#[tokio::test]
async fn unusable_attach_context_refuses_before_building_anything() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("terminal-keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let socket = guard.socket_path().display().to_string();
    let live = format!("{socket},{},0", guard.daemon_pid());
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let marker = directory.path().join("before-script-ran");
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({
            "session_name":"probe", "before_script":"touch before-script-ran",
            "windows":[{"panes":["blank"]}]
        })
        .to_string(),
    )
    .unwrap();
    let cases = [
        (
            "stale daemon",
            format!("{socket},{},0", guard.daemon_pid().wrapping_add(1)),
            pane.clone(),
            "no longer",
        ),
        (
            "malformed variable",
            socket.clone(),
            pane.clone(),
            "socket,pid,session",
        ),
        (
            "pane is not an id",
            live.clone(),
            "not-a-pane".into(),
            "TMUX_PANE",
        ),
        (
            "pane is not here",
            live.clone(),
            "%9999".into(),
            "TMUX_PANE",
        ),
        ("nothing attached", live.clone(), pane.clone(), "attached"),
    ];
    for (case, context, current, needle) in cases {
        let output = command_at(
            &["load", "workspace.json", "-S", socket.as_str()],
            directory.path(),
        )
        .env("TMUX", &context)
        .env("TMUX_PANE", &current)
        .output()
        .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{case}: {output:?}");
        assert!(stderr.contains(needle), "{case}: {stderr}");
        assert!(!marker.exists(), "{case}: the refusal ran before_script");
        assert_eq!(
            guard.server().sessions().await.unwrap().len(),
            1,
            "{case}: the refusal built a session"
        );
    }
    assert_eq!(keeper.windows().await.unwrap().len(), 1);
    guard.shutdown().await.unwrap();
}

/// A socket path holding commas is one tmux cannot address but this command
/// can, and `freeze` and `load --append` both accept it. The attach gate
/// reads `TMUX` the same way, so an attached load accepts it too.
#[tokio::test]
async fn attach_accepts_matching_explicit_and_inherited_comma_socket_paths() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("terminal-keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let control = libtmux::control::ControlMode::attach(guard.server(), keeper.id())
        .await
        .unwrap();
    wait_for_client(guard.server(), keeper.id().as_ref()).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let alias = directory.path().join("socket,with,commas");
    std::os::unix::fs::symlink(guard.socket_path(), &alias).unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({"session_name":"comma-attached","windows":[{"panes":["blank"]}]})
            .to_string(),
    )
    .unwrap();
    let output = command_at(
        &["load", "workspace.json", "-S", alias.to_str().unwrap()],
        directory.path(),
    )
    .env(
        "TMUX",
        format!("{},{},0", alias.display(), guard.daemon_pid()),
    )
    .env("TMUX_PANE", &pane)
    .output()
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    assert!(!stderr.contains("use -d"), "{stderr}");
    assert!(
        guard
            .server()
            .session("comma-attached")
            .await
            .unwrap()
            .is_some(),
        "{stderr}"
    );
    drop(control);
    guard.shutdown().await.unwrap();
}

/// Waits until tmux lists a client on the session, which attaching reports
/// separately from registering it.
async fn wait_for_client(server: &libtmux::Server, session: &str) {
    for _ in 0..600 {
        let attached = server
            .cmd(
                libtmux::Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(session)
                    .arg("#{session_attached}"),
            )
            .await
            .unwrap();
        if attached.stdout_lossy().trim() != "0" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("no client attached to {session}");
}

/// Aimed at a server other than the current pane's, an attached load refuses
/// before building anything, the same way whether that server is already
/// running or has never been started.
#[tokio::test]
async fn cross_server_attach_refuses_before_building_whether_or_not_the_target_runs() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("terminal-keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({"session_name":"other-workspace","windows":[{"panes":["blank"]}]})
            .to_string(),
    )
    .unwrap();

    let other = libtmux::test::TestServer::new().await.unwrap();
    let output = at_pane(
        &[
            "load",
            "workspace.json",
            "-S",
            other.socket_path().to_str().unwrap(),
        ],
        directory.path(),
        Some((&guard, pane.as_str())),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("current pane's"), "{stderr}");
    assert!(stderr.contains("-d"), "{stderr}");
    assert_eq!(
        other.server().sessions().await.unwrap().len(),
        0,
        "cross-server refusal built a session on a running target"
    );
    other.shutdown().await.unwrap();

    let absent = directory.path().join("never-started.sock");
    let output = at_pane(
        &["load", "workspace.json", "-S", absent.to_str().unwrap()],
        directory.path(),
        Some((&guard, pane.as_str())),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        !absent.exists(),
        "cross-server refusal started the target server"
    );
    assert_eq!(
        guard.server().sessions().await.unwrap().len(),
        1,
        "cross-server refusal touched the current pane's own server"
    );
    guard.shutdown().await.unwrap();
}

/// `TMUX` set with no `TMUX_PANE`, as a `run-shell` key binding sees it: the
/// load still builds and switches, exit 0, no terminal needed.
#[tokio::test]
async fn run_shell_style_load_has_no_terminal_and_no_tmux_pane() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({"session_name":"runshell-built","windows":[{"panes":["blank"]}]})
            .to_string(),
    )
    .unwrap();
    let mut command = command_at(
        &[
            "load",
            "workspace.json",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--yes",
        ],
        directory.path(),
    );
    command.env(
        "TMUX",
        format!("{},{},0", guard.socket_path().display(), guard.daemon_pid()),
    );
    let output = command.output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("attaching requires a terminal"),
        "{stderr}"
    );
    assert!(!stderr.contains("current pane's"), "{stderr}");
    let built = guard.server().session("runshell-built").await.unwrap();
    let built =
        built.unwrap_or_else(|| panic!("run-shell style load did not build the session: {stderr}"));
    built.kill().await.unwrap();
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_sizes_a_created_session_from_tmuxp_environment_variables() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("ws.json"),
        serde_json::json!({"session_name":"sized","windows":[{"panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    let output = command_at(
        &[
            "load",
            "-d",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--json",
            "ws.json",
        ],
        directory.path(),
    )
    // The invoking terminal, when there is one, still wins over these; the
    // test's own stdout is a captured pipe, so it never is one here.
    .env("TMUXP_DEFAULT_COLUMNS", "111")
    .env("TMUXP_DEFAULT_ROWS", "33")
    .env_remove("COLUMNS")
    .env_remove("LINES")
    .env_remove("ROWS")
    .env_remove("TMUXP_DETECT_TERMINAL_SIZE")
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let session = guard.server().session("sized").await.unwrap().unwrap();
    let window = session.active_window().await.unwrap().unwrap();
    assert_eq!(
        window
            .format("#{window_width}x#{window_height}")
            .await
            .unwrap()
            .to_string_lossy(),
        "111x33"
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_passes_no_explicit_size_when_detection_is_disabled() {
    // With TMUXP_DETECT_TERMINAL_SIZE disabled, the session is not pinned to
    // TMUXP_DEFAULT_COLUMNS/ROWS or the 80x24 fallback: no -x/-y reaches
    // new-session at all, so tmux sizes it from the server's own
    // default-size instead.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard
        .server()
        .cmd(
            libtmux::Command::new("set-option")
                .arg("-g")
                .arg("default-size")
                .arg("222x55"),
        )
        .await
        .unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("ws.json"),
        serde_json::json!({"session_name":"undetected","windows":[{"panes":["blank"]}]})
            .to_string(),
    )
    .unwrap();
    let output = command_at(
        &[
            "load",
            "-d",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--json",
            "ws.json",
        ],
        directory.path(),
    )
    .env("TMUXP_DEFAULT_COLUMNS", "111")
    .env("TMUXP_DEFAULT_ROWS", "33")
    .env("TMUXP_DETECT_TERMINAL_SIZE", "0")
    .env_remove("COLUMNS")
    .env_remove("LINES")
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let session = guard.server().session("undetected").await.unwrap().unwrap();
    let window = session.active_window().await.unwrap().unwrap();
    assert_eq!(
        window
            .format("#{window_width}x#{window_height}")
            .await
            .unwrap()
            .to_string_lossy(),
        "222x55",
        "TMUXP_DEFAULT_COLUMNS/ROWS should not apply once detection is off"
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_rejects_an_out_of_range_dimension_before_any_target_lookup() {
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("ws.json"),
        serde_json::json!({"session_name":"never","windows":[{"panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    let output = command_at(
        &["load", "-d", "-S", "does-not-matter", "ws.json"],
        directory.path(),
    )
    .env("TMUXP_DEFAULT_COLUMNS", "not-a-number")
    .env_remove("COLUMNS")
    .output()
    .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("TMUXP_DEFAULT_COLUMNS"),
        "{output:?}"
    );
}

#[tokio::test]
async fn layout_preflight_checks_all_inputs_before_scripts_or_append() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("layout-cli-keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let marker = directory.path().join("layout-script");
    std::fs::write(
        directory.path().join("first.json"),
        serde_json::json!({
            "session_name":"layout-first", "before_script":"touch layout-script",
            "options":{"@layout-changed":"yes"}, "windows":[{"window_name":"first"}]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("second.json"),
        serde_json::json!({
            "session_name":"layout-second", "windows":[{
                "layout":"b25d,80x24,0,0,0", "panes":["blank","blank"]
            }]
        })
        .to_string(),
    )
    .unwrap();
    for append in [false, true] {
        let result = at_pane(
            &[
                "load",
                "first.json",
                "second.json",
                if append { "--append" } else { "-d" },
                "-S",
                guard.socket_path().to_str().unwrap(),
                "--json",
            ],
            directory.path(),
            append.then_some((&guard, pane.as_str())),
        );
        assert!(!marker.exists(), "earlier input ran a setup script");
        assert!(!result.status.success(), "invalid layout reported success");
        let sessions = guard.server().sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id(), keeper.id());
        assert!(
            keeper
                .get_option("@layout-changed")
                .await
                .unwrap()
                .is_none()
        );
    }
    guard.shutdown().await.unwrap();
}

#[test]
fn the_layout_corpus_matches_the_library_copy() {
    // Each published crate ships its own fixtures, so the corpus cannot be
    // shared by path; this is what keeps the two copies one corpus.
    let library = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../libtmux/tests/fixtures/layout-preflight.json");
    assert_eq!(
        include_str!("fixtures/layout-preflight.json"),
        std::fs::read_to_string(&library).unwrap(),
        "{}",
        library.display()
    );
}

#[tokio::test]
async fn real_tmux_compat_layout_preflight_corpus_preserves_keeper() {
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/layout-preflight.json")).unwrap();
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("layout-corpus-keeper").await.unwrap();
    let windows = keeper
        .windows()
        .await
        .unwrap()
        .iter()
        .map(|window| window.id().clone())
        .collect::<Vec<_>>();
    let panes = keeper
        .panes()
        .await
        .unwrap()
        .iter()
        .map(|pane| pane.id().clone())
        .collect::<Vec<_>>();
    let version = guard.server().capabilities().await.unwrap().tmux_version();
    let key = if version.meets(&libtmux::since::MIRRORED_LAYOUTS) {
        "3.7c"
    } else {
        "3.2a"
    };
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    for case in corpus.as_array().unwrap() {
        let count = usize::try_from(case["pane_count"].as_u64().unwrap()).unwrap();
        std::fs::write(directory.path().join("layout.json"), serde_json::json!({
            "session_name":"layout-case", "windows":[{"layout":case["layout"], "panes":vec!["blank"; count]}]
        }).to_string()).unwrap();
        let result = at(
            &[
                "load",
                "layout.json",
                "-d",
                "-S",
                guard.socket_path().to_str().unwrap(),
                "--json",
            ],
            directory.path(),
        );
        assert_eq!(
            result.status.success(),
            case["expected_valid"][key].as_bool().unwrap(),
            "{}: {} {}",
            case["id"],
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            guard
                .server()
                .format(None, "#{pid}")
                .await
                .unwrap()
                .to_string_lossy(),
            guard.daemon_pid().to_string()
        );
        assert_eq!(
            keeper
                .windows()
                .await
                .unwrap()
                .iter()
                .map(|window| window.id().clone())
                .collect::<Vec<_>>(),
            windows
        );
        assert_eq!(
            keeper
                .panes()
                .await
                .unwrap()
                .iter()
                .map(|pane| pane.id().clone())
                .collect::<Vec<_>>(),
            panes
        );
        for session in guard.server().sessions().await.unwrap() {
            if session.id() != keeper.id() {
                session.kill().await.unwrap();
            }
        }
    }
    guard.shutdown().await.unwrap();
}

fn at(arguments: &[&str], directory: &Path) -> Output {
    at_pane(arguments, directory, None)
}

fn at_pane(
    arguments: &[&str],
    directory: &Path,
    current: Option<(&libtmux::test::TestServer, &str)>,
) -> Output {
    let mut command = command_at(arguments, directory);
    if let Some((guard, pane)) = current {
        command.env("TMUX_PANE", pane).env(
            "TMUX",
            format!("{},{},0", guard.socket_path().display(), guard.daemon_pid()),
        );
    }
    command.output().unwrap()
}

fn command_at(arguments: &[&str], directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"));
    command
        .args(arguments)
        .current_dir(directory)
        .env("HOME", directory)
        .env("TMUXP_CONFIGDIR", directory.join(".tmuxp"))
        .env("XDG_CONFIG_HOME", directory.join(".config"))
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    command
}

async fn current_pane(session: &libtmux::Session) -> String {
    session
        .active_window()
        .await
        .unwrap()
        .unwrap()
        .active_pane()
        .await
        .unwrap()
        .unwrap()
        .id()
        .to_string()
}

/// Waits until a pane's shell has drawn a prompt and can receive input.
async fn prompt_ready(server: &libtmux::Server, pane: &str) {
    for _ in 0..600 {
        let reading = server
            .cmd(
                libtmux::Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(pane)
                    .arg("#{cursor_x},#{cursor_y}"),
            )
            .await
            .unwrap()
            .stdout_lossy()
            .trim()
            .to_owned();
        if !reading.is_empty() && reading != "0,0" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("pane {pane} never drew a prompt");
}

/// Drives one interactive load prompt from a real tmux pane: types the load
/// command, waits for the question, answers by keystroke, and waits for the
/// shell prompt to return, never by piping stdin.
struct PromptDriver<'a> {
    guard: &'a libtmux::test::TestServer,
    keeper: &'a libtmux::Session,
    binary: &'a str,
    socket: &'a str,
    directory: &'a Path,
}

impl PromptDriver<'_> {
    async fn ask(&self, load_args: &str, prompt_needle: &str, answer: char) -> libtmux::Pane {
        // An attached load resolves the client it would switch before it
        // builds anything, so the pane needs one; control mode is the only
        // kind that attaches without a terminal of its own.
        let control = libtmux::control::ControlMode::attach(self.guard.server(), self.keeper.id())
            .await
            .unwrap();
        wait_for_client(self.guard.server(), self.keeper.id().as_ref()).await;
        let window = self
            .keeper
            .new_window(
                libtmux::NewWindowOptions::unnamed().start_directory(self.directory.to_path_buf()),
            )
            .await
            .unwrap();
        let pane = window.active_pane().await.unwrap().unwrap();
        prompt_ready(self.guard.server(), pane.id().as_ref()).await;
        let binary = self.binary;
        let socket = self.socket;
        pane.send_line(format!("{binary} load -S {socket} {load_args}; echo RC=$?"))
            .await
            .unwrap();
        pane.wait_for_text(prompt_needle, std::time::Duration::from_secs(5))
            .await
            .unwrap();
        pane.send_line(answer.to_string()).await.unwrap();
        // Not `wait_for_text("RC=", ...)`: the shell echoes the typed
        // command, including the literal `echo RC=$?` it has not run yet,
        // so that text is on screen before the load even starts.
        pane.wait_for_quiet(
            std::time::Duration::from_millis(300),
            std::time::Duration::from_secs(10),
        )
        .await
        .unwrap();
        drop(control);
        pane
    }
}

const NEW_SESSION_PROMPT: &str = "load detached (n)";

/// New session, inside tmux, answered "n": builds detached, never appends.
#[tokio::test]
async fn new_session_prompt_answered_detached_builds_without_appending() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("keeper").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("new.json"),
        serde_json::json!({"session_name":"prompted-new","windows":[{"window_name":"pnew","panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    let driver = PromptDriver {
        guard: &guard,
        keeper: &keeper,
        binary: env!("CARGO_BIN_EXE_tmux-workspace"),
        socket: guard.socket_path().to_str().unwrap(),
        directory: directory.path(),
    };
    let pane = driver.ask("new.json", NEW_SESSION_PROMPT, 'n').await;
    let built = guard.server().session("prompted-new").await.unwrap();
    assert!(
        built.is_some(),
        "answering n did not build the detached session"
    );
    assert!(
        !keeper
            .windows()
            .await
            .unwrap()
            .iter()
            .any(|window| window.name().to_string_lossy() == "pnew"),
        "answering n appended instead of building detached"
    );
    built.unwrap().kill().await.unwrap();
    pane.window().await.unwrap().unwrap().kill().await.unwrap();
    guard.shutdown().await.unwrap();
}

/// New sessions, inside tmux, answered "a": appends every input into the
/// current session, as `--append` would, rather than building sessions named
/// by the workspaces.
#[tokio::test]
async fn new_session_prompt_answered_append_appends_the_current_session() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("keeper").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("new.json"),
        serde_json::json!({"session_name":"prompted-new","windows":[{"window_name":"pnew","panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("earlier.json"),
        serde_json::json!({"session_name":"prompted-earlier","windows":[{"window_name":"pearlier","panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    let driver = PromptDriver {
        guard: &guard,
        keeper: &keeper,
        binary: env!("CARGO_BIN_EXE_tmux-workspace"),
        socket: guard.socket_path().to_str().unwrap(),
        directory: directory.path(),
    };
    let pane = driver
        .ask("earlier.json new.json", NEW_SESSION_PROMPT, 'a')
        .await;
    let names: Vec<_> = keeper
        .windows()
        .await
        .unwrap()
        .iter()
        .map(|window| window.name().to_string_lossy().into_owned())
        .collect();
    for (session, window) in [("prompted-earlier", "pearlier"), ("prompted-new", "pnew")] {
        assert!(
            guard.server().session(session).await.unwrap().is_none(),
            "answering a still built {session}"
        );
        assert!(
            names.iter().any(|name| name == window),
            "answering a did not append {window}: {names:?}"
        );
    }
    pane.window().await.unwrap().unwrap().kill().await.unwrap();
    guard.shutdown().await.unwrap();
}

/// An already-running session, answered "n": nothing changes, exit 0.
#[tokio::test]
async fn exists_prompt_answered_no_changes_nothing() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("keeper").await.unwrap();
    let existing = guard.session("already-there").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("exists.json"),
        serde_json::json!({"session_name":"already-there","windows":[{"window_name":"unwanted","panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    let driver = PromptDriver {
        guard: &guard,
        keeper: &keeper,
        binary: env!("CARGO_BIN_EXE_tmux-workspace"),
        socket: guard.socket_path().to_str().unwrap(),
        directory: directory.path(),
    };
    let pane = driver
        .ask("exists.json", "already running. Attach?", 'n')
        .await;
    assert_eq!(
        existing.windows().await.unwrap().len(),
        1,
        "answering n to the exists prompt changed the session"
    );
    let text = pane.capture().await.unwrap();
    let text = text
        .iter()
        .map(|line| line.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("RC=0"), "{text}");
    pane.window().await.unwrap().unwrap().kill().await.unwrap();
    guard.shutdown().await.unwrap();
}

/// Two inputs, inside tmux: only the last is asked about. The first builds
/// detached without a question of its own, and declining the second leaves
/// its session as it was.
#[tokio::test]
async fn only_the_last_input_is_asked_about() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("keeper").await.unwrap();
    let existing = guard.session("already-there").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("new.json"),
        serde_json::json!({"session_name":"prompted-new","windows":[{"window_name":"pnew","panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("exists.json"),
        serde_json::json!({"session_name":"already-there","windows":[{"window_name":"unwanted","panes":["blank"]}]}).to_string(),
    )
    .unwrap();
    let driver = PromptDriver {
        guard: &guard,
        keeper: &keeper,
        binary: env!("CARGO_BIN_EXE_tmux-workspace"),
        socket: guard.socket_path().to_str().unwrap(),
        directory: directory.path(),
    };
    let pane = driver
        .ask("new.json exists.json", "already running. Attach?", 'n')
        .await;
    let text = pane.capture().await.unwrap();
    let text = text
        .iter()
        .map(|line| line.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains(NEW_SESSION_PROMPT), "{text}");
    // Asked before any progress frame is drawn, so no redraw erases the
    // question or the answer to it.
    assert!(text.contains("already running. Attach? [Y/n] n"), "{text}");
    assert!(text.contains("RC=0"), "{text}");
    assert!(
        guard
            .server()
            .session("prompted-new")
            .await
            .unwrap()
            .is_some(),
        "the first input was not built"
    );
    assert_eq!(existing.windows().await.unwrap().len(), 1);
    pane.window().await.unwrap().unwrap().kill().await.unwrap();
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn append_authenticates_inherited_and_selected_daemons_before_python_or_mutation() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let original = libtmux::test::TestServer::new().await.unwrap();
    let replacement = libtmux::test::TestServer::new().await.unwrap();
    let first = original.session("original").await.unwrap();
    let second = replacement.session("replacement").await.unwrap();
    let pane = current_pane(&first).await;
    assert_eq!(pane, current_pane(&second).await);
    assert_ne!(original.daemon_pid(), replacement.daemon_pid());
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let alias = directory.path().join("inherited");
    symlink(original.socket_path(), &alias).unwrap();
    let marker = directory.path().join("python-called");
    let python = directory.path().join("python-sentinel");
    std::fs::write(&python, "#!/bin/sh\n: > python-called\nexit 97\n").unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut failures = Vec::new();
    for retarget in [false, true] {
        if retarget {
            std::fs::remove_file(&alias).unwrap();
            symlink(replacement.socket_path(), &alias).unwrap();
        }
        let selected = if retarget {
            &alias
        } else {
            replacement.socket_path()
        };
        let live = libtmux::Server::builder()
            .socket_path(selected)
            .tmux_executable(original.server().tmux_executable())
            .build()
            .unwrap()
            .cmd(
                libtmux::Command::new("display-message")
                    .arg("-p")
                    .arg("#{pid}"),
            )
            .await
            .unwrap()
            .stdout_lossy()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert_eq!(live, replacement.daemon_pid());
        assert_ne!(live, original.daemon_pid());
        for bridge in [false, true] {
            let mut config = serde_json::json!({"session_name":"unwanted","windows":[{"window_name":"added","panes":["blank"]}]});
            if bridge {
                config["plugins"] = serde_json::json!(["never.Imported"]);
            }
            std::fs::write(directory.path().join("workspace.json"), config.to_string()).unwrap();
            for mode in ["--json", "--ndjson"] {
                let _ = std::fs::remove_file(&marker);
                let output = command_at(
                    &[
                        "load",
                        "-S",
                        selected.to_str().unwrap(),
                        "--append",
                        mode,
                        "workspace.json",
                    ],
                    directory.path(),
                )
                .env(
                    "TMUX",
                    format!("{},{},0", alias.display(), original.daemon_pid()),
                )
                .env("TMUX_PANE", &pane)
                .env("TMUX_WORKSPACE_PYTHON", &python)
                .output()
                .unwrap();
                if output.status.success()
                    || !output.stdout.is_empty()
                    || !String::from_utf8_lossy(&output.stderr).contains("\"usage\"")
                    || marker.exists()
                {
                    failures.push(format!("retarget={retarget} bridge={bridge} mode={mode}: {output:?}; Python invoked={}", marker.exists()));
                }
            }
        }
    }
    for session in [&first, &second] {
        if session.windows().await.unwrap().len() != 1 {
            failures.push(format!("borrowed session {} was mutated", session.id()));
        }
    }
    original.shutdown().await.unwrap();
    replacement.shutdown().await.unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn append_accepts_matching_explicit_and_inherited_comma_socket_paths() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard.session("borrowed").await.unwrap();
    let pane = current_pane(&session).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let alias = directory.path().join("socket,with,commas");
    std::os::unix::fs::symlink(guard.socket_path(), &alias).unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        r#"{"session_name":"ignored","windows":[{"window_name":"added","panes":["blank"]}]}"#,
    )
    .unwrap();
    for explicit in [true, false] {
        let mut args = vec!["load", "--append", "--json", "workspace.json"];
        if explicit {
            args.extend(["-S", alias.to_str().unwrap()]);
        }
        let output = command_at(&args, directory.path())
            .env(
                "TMUX",
                format!("{},{},0", alias.display(), guard.daemon_pid()),
            )
            .env("TMUX_PANE", &pane)
            .output()
            .unwrap();
        assert!(output.status.success(), "explicit={explicit}: {output:?}");
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            document["results"][0]["session_id"],
            session.id().to_string()
        );
        assert_eq!(document["results"][0]["owned_session"], false);
    }
    assert_eq!(guard.server().sessions().await.unwrap().len(), 1);
    assert_eq!(session.windows().await.unwrap().len(), 3);
    assert!(
        guard
            .server()
            .cmd(
                libtmux::Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(&pane)
                    .arg("#{pane_id}")
            )
            .await
            .unwrap()
            .stdout_lossy()
            .contains(&pane)
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn empty_tmux_context_allows_freezing_an_isolated_default_endpoint() {
    use std::os::unix::fs::PermissionsExt;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard.session("borrowed").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let sockets = directory
        .path()
        .join(format!("tmux-{}", rustix::process::getuid().as_raw()));
    std::fs::create_dir(&sockets).unwrap();
    std::fs::set_permissions(&sockets, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(guard.socket_path(), sockets.join("default")).unwrap();
    let output = command_at(&["freeze", "borrowed", "--json"], directory.path())
        .env("TMUX", "")
        .env("TMUX_TMPDIR", directory.path())
        .output()
        .unwrap();
    guard.shutdown().await.unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("borrowed"));
}

#[tokio::test]
async fn freeze_never_derives_a_destination_from_a_session_name() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    // tmux rewrites `.` and `:` in a session name on some releases and keeps
    // `/` on every one, so the separator under test is the one that survives.
    guard.session("sub/escaped").await.unwrap();
    guard.session("kept").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let work = directory.path().join("work");
    std::fs::create_dir_all(work.join("sub")).unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let escaped = at(&["freeze", "-S", socket, "-y", "-q", "sub/escaped"], &work);
    let plain = at(&["freeze", "-S", socket, "-y", "-q", "kept"], &work);
    let named = at(
        &[
            "freeze",
            "-S",
            socket,
            "-y",
            "-q",
            "-o",
            "kept.yaml",
            "kept",
        ],
        &work,
    );
    let machine = at(&["freeze", "-S", socket, "--json", "kept"], &work);
    guard.shutdown().await.unwrap();
    for refused in [&escaped, &plain] {
        assert_eq!(refused.status.code(), Some(2), "{refused:?}");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("--save-to"),
            "{refused:?}"
        );
    }
    assert!(!work.join("sub/escaped.yaml").exists());
    assert!(named.status.success(), "{named:?}");
    assert!(work.join("kept.yaml").exists());
    assert!(machine.status.success(), "{machine:?}");
    assert!(String::from_utf8_lossy(&machine.stdout).contains("kept"));
}

#[tokio::test]
async fn freeze_save_to_never_prompts_even_without_a_terminal_or_yes() {
    // command_at's Output::output() gives the child a piped, non-tty stdin,
    // so this alone already proves --save-to needs no --yes to succeed.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard.session("scripted").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let output = at(
        &[
            "freeze",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "-q",
            "-o",
            "scripted.yaml",
            "scripted",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    assert!(directory.path().join("scripted.yaml").exists());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn append_rechecks_after_python_runtime_and_before_script() {
    use std::os::unix::fs::PermissionsExt;
    for runtime in [false, true] {
        let original = libtmux::test::TestServer::new().await.unwrap();
        let replacement = libtmux::test::TestServer::new().await.unwrap();
        let first = original.session("original").await.unwrap();
        let second = replacement.session("replacement").await.unwrap();
        assert_eq!(first.id(), second.id());
        let pane = current_pane(&first).await;
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
        let alias = directory.path().join("socket");
        std::os::unix::fs::symlink(original.socket_path(), &alias).unwrap();
        std::fs::write(directory.path().join("retarget.sh"),
            "#!/bin/sh\nrm socket\nln -s \"$REPLACEMENT_SOCKET\" socket\nprintf done > script-ran\nprintf 1.74.0\n").unwrap();
        std::fs::set_permissions(
            directory.path().join("retarget.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut config = serde_json::json!({"session_name":"ignored", "environment":{"AFTER_SCRIPT":"unwanted"},
            "windows":[{"window_name":"unwanted","panes":["blank"]}]});
        if !runtime {
            config["before_script"] = serde_json::json!("sh retarget.sh");
        }
        std::fs::write(directory.path().join("workspace.json"), config.to_string()).unwrap();
        let mut args = vec!["load", "--append", "--json", "workspace.json"];
        if runtime {
            config["plugins"] = serde_json::json!(["never.Imported"]);
            std::fs::write(directory.path().join("extension.json"), config.to_string()).unwrap();
            args.push("extension.json");
        }
        let output = command_at(&args, directory.path())
            .env(
                "TMUX",
                format!("{},{},0", alias.display(), original.daemon_pid()),
            )
            .env("TMUX_PANE", pane)
            .env("REPLACEMENT_SOCKET", replacement.socket_path())
            .env(
                "TMUX_WORKSPACE_PYTHON",
                directory.path().join("retarget.sh"),
            )
            .output()
            .unwrap();
        assert!(directory.path().join("script-ran").exists(), "{output:?}");
        let mut changed = false;
        for session in [&first, &second] {
            changed |= session.windows().await.unwrap().len() != 1
                || session
                    .environment_all()
                    .await
                    .unwrap()
                    .contains_key("AFTER_SCRIPT");
        }
        original.shutdown().await.unwrap();
        replacement.shutdown().await.unwrap();
        assert!(
            !output.status.success() && !changed,
            "runtime={runtime}: {output:?}; changed={changed}"
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["errors"][0]["code"], "usage");
        assert_eq!(value["status"], if runtime { "error" } else { "partial" });
        assert_eq!(value["errors"][0]["effects"]["owned_session"], false);
        assert_eq!(value["errors"][0]["effects"]["session_name"], "original");
    }
}

fn cli(arguments: &[&str]) -> Output {
    let binary = env!("CARGO_BIN_EXE_tmux-workspace");
    Command::new(binary)
        .args(arguments)
        .env("PATH", "/nonexistent")
        .env_remove("LIBTMUX_TEST_TMUX")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run workspace CLI")
}

#[test]
fn help_and_version_do_not_require_tmux() {
    let output = cli(&["--help"]);
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "convert",
        "debug-info",
        "edit",
        "freeze",
        "import",
        "load",
        "ls",
        "search",
        "shell",
    ] {
        assert!(help.contains(command), "missing command {command}: {help}");
    }
    assert!(cli(&["--version"]).status.success());
}

#[test]
fn generated_metadata_completion_and_manual_use_the_command_graph() {
    let output = cli(&["--generate", "schema"]);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let commands = value["command"]["subcommands"].as_array().unwrap();
    assert_eq!(commands.len(), 9);
    let load = commands.iter().find(|c| c["name"] == "load").unwrap();
    let colors = load["arguments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "colors88")
        .unwrap();
    assert_eq!(colors["long"], "88-colors");
    assert!(
        colors["description"]
            .as_str()
            .unwrap()
            .contains("Reject legacy 88-color mode")
    );
    assert_eq!(
        commands.iter().find(|c| c["name"] == "import").unwrap()["subcommands"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for format in ["bash", "zsh", "fish", "powershell", "elvish", "man"] {
        let output = cli(&["--generate", format]);
        assert!(output.status.success(), "{format}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("tmux-workspace"));
        if format != "man" {
            assert!(String::from_utf8_lossy(&output.stdout).contains("88-colors"));
        }
    }
}

#[test]
fn malformed_invocations_fail_before_backend_work() {
    for args in [
        vec!["--json", "load", "-2", "-8", "missing"],
        vec!["--json", "load", "-8", "-2", "missing"],
        vec!["--json", "load", "-2", "--88-colors", "missing"],
        vec!["--json", "load", "--88-colors", "-2", "missing"],
        vec!["load", "--json", "--unknown", "missing"],
        vec!["--json", "import", "teamocil"],
        vec!["import", "tmuxinator", "--ndjson"],
        vec!["shell", "--json", "--code", "--ipython"],
        vec!["freeze", "--json", "-f", "xml"],
        vec!["search", "--json"],
        vec!["search", "--json", "["],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        assert!(output.stderr.starts_with(b"{"), "{args:?}: {output:?}");
        assert!(!output.stderr.contains(&0x1b), "{args:?}: {output:?}");
    }
}

#[test]
fn legacy_color_mode_is_rejected_before_reading_inputs() {
    for flag in ["-8", "--88-colors"] {
        for mode in [None, Some("--json"), Some("--ndjson")] {
            let mut args = vec!["load", "-d", flag, "missing-first", "missing-second"];
            args.extend(mode);
            let output = cli(&args);
            assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
            assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
            assert!(!output.stderr.contains(&0x1b));
            if mode.is_some() {
                let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
                assert_eq!(error["code"], "usage");
            } else {
                assert!(String::from_utf8_lossy(&output.stderr).contains("88-color"));
            }
        }
    }
}

#[tokio::test]
async fn color_validation_preserves_sessions_and_256_reaches_tmux() {
    use std::os::unix::fs::PermissionsExt as _;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("existing"))
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let trace = directory.path().join("trace");
    let wrapper = directory.path().join("tmux");
    let python = directory.path().join("python");
    std::fs::write(&wrapper, "#!/bin/sh\nprintf '<%s>' \"$@\" >> \"$WORKSPACE_TMUX_TRACE\"\nprintf '\\n' >> \"$WORKSPACE_TMUX_TRACE\"\nexec \"$WORKSPACE_REAL_TMUX\" \"$@\"\n").unwrap();
    std::fs::write(
        &python,
        "#!/bin/sh\nprintf 'python\\n' >> \"$WORKSPACE_TMUX_TRACE\"\nexit 97\n",
    )
    .unwrap();
    for executable in [&wrapper, &python] {
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for name in ["first", "second", "bridge"] {
        let mut workspace =
            serde_json::json!({"session_name":name,"windows":[{"panes":["blank"]}]});
        if name == "bridge" {
            workspace["plugins"] = serde_json::json!(["missing_plugin"]);
        }
        std::fs::write(
            directory.path().join(format!("{name}.json")),
            workspace.to_string(),
        )
        .unwrap();
    }
    let topology = || {
        guard.server().cmd(
            libtmux::Command::new("list-panes")
                .arg("-a")
                .arg("-F")
                .arg("#{session_id}:#{window_id}:#{pane_id}"),
        )
    };
    let before = topology().await.unwrap();
    assert!(before.success());
    let run = |options: &[&str], files: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
            .args([
                "load",
                "-d",
                "-S",
                guard.server().socket_path().to_str().unwrap(),
            ])
            .args(options)
            .args(files)
            .current_dir(directory.path())
            .env("LIBTMUX_TEST_TMUX", &wrapper)
            .env("TMUX_WORKSPACE_PYTHON", &python)
            .env("WORKSPACE_TMUX_TRACE", &trace)
            .env(
                "WORKSPACE_REAL_TMUX",
                std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into()),
            )
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap()
    };
    for flag in ["-8", "--88-colors"] {
        for mode in [None, Some("--json"), Some("--ndjson")] {
            let mut options = vec![flag];
            options.extend(mode);
            for last in ["second.json", "bridge.json"] {
                let output = run(&options, &["first.json", last]);
                assert_eq!(output.status.code(), Some(2), "{options:?}: {output:?}");
                assert!(output.stdout.is_empty(), "{output:?}");
                assert!(!trace.exists(), "validation invoked tmux or Python");
                assert_eq!(topology().await.unwrap().stdout(), before.stdout());
            }
        }
    }
    let output = run(&["--json", "-2"], &["first.json", "second.json"]);
    assert!(output.status.success(), "{output:?}");
    for name in ["first", "second"] {
        assert!(guard.server().has_session(name).await.unwrap());
    }
    let calls = std::fs::read_to_string(trace).unwrap();
    assert!(calls.contains("<-2>"), "{calls}");
    assert!(
        calls
            .lines()
            .all(|line| line == "<-V>" || line.contains("<-2>")),
        "{calls}"
    );
    guard.shutdown().await.unwrap();
}

#[test]
fn every_leaf_accepts_machine_flags_before_or_after_its_name() {
    for leaf in [
        vec!["convert"],
        vec!["debug-info"],
        vec!["edit"],
        vec!["freeze"],
        vec!["import", "teamocil"],
        vec!["import", "tmuxinator"],
        vec!["load"],
        vec!["ls"],
        vec!["search"],
        vec!["shell"],
    ] {
        for flag in ["--json", "--ndjson"] {
            for before in [true, false] {
                let mut args = leaf.clone();
                if before {
                    args.insert(0, flag);
                } else {
                    args.push(flag);
                }
                args.push("--help");
                let output = cli(&args);
                assert!(output.status.success(), "{args:?}: {output:?}");
                assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
            }
        }
    }
}

#[test]
fn empty_discovery_has_stable_json_and_ndjson_shapes() {
    let directory = tempfile::tempdir().unwrap();
    let output = at(&["ls", "--json"], directory.path());
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["workspaces"], serde_json::json!([]));
    assert!(value["global_workspace_dirs"].is_array());
    assert!(value["global_workspace_dirs"][0]["exists"].is_boolean());
    assert!(value["global_workspace_dirs"][0]["workspace_count"].is_number());
    let output = at(&["--json", "ls", "--ndjson"], directory.path());
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let output = at(&["search", "--json", "absent"], directory.path());
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn conversion_preserves_extension_values_and_protects_existing_files() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("project.yaml"), "session_name: demo\nwindows: []\ncustom:\n  n: 42\n  yes: true\n  values: [null, 'a\\nb', '雪']\n").unwrap();
    let output = at(&["convert", "project.yaml", "--json"], directory.path());
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["custom"]["n"], 42);
    assert_eq!(value["custom"]["yes"], true);
    assert_eq!(value["custom"]["values"][2], "雪");
    assert!(!directory.path().join("project.json").exists());
    std::fs::write(directory.path().join("kept.json"), "retained").unwrap();
    let output = at(
        &[
            "convert",
            "project.yaml",
            "--json",
            "--save-to",
            "kept.json",
        ],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("kept.json")).unwrap(),
        "retained"
    );
    let output = at(
        &[
            "convert",
            "project.yaml",
            "--ndjson",
            "--save-to",
            "kept.json",
            "--force",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("kept.json")).unwrap())
            .unwrap();
    assert_eq!(saved, value);
}

#[test]
fn importers_transform_native_source_documents() {
    let directory = tempfile::tempdir().unwrap();
    for (kind, source) in [
        // Teamocil evaluates no templates, so markup a tmuxinator source
        // could not carry is ordinary text here and survives the import.
        (
            "teamocil",
            "session:\n  name: demo\n  windows:\n    - name: editor\n      splits:\n        - cmd: echo <%= literal %>\n",
        ),
        (
            "tmuxinator",
            "name: demo\nroot: /tmp\nwindows:\n  - editor:\n      panes:\n        - echo hello\n",
        ),
    ] {
        std::fs::write(directory.path().join("input.yaml"), source).unwrap();
        let output = at(&["import", kind, "input.yaml", "--json"], directory.path());
        assert!(output.status.success(), "{kind}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["session_name"], "demo");
        assert_eq!(value["windows"][0]["window_name"], "editor");
        assert_eq!(value["windows"][0]["panes"].as_array().unwrap().len(), 1);
        if kind == "teamocil" {
            assert_eq!(
                value["windows"][0]["panes"][0]["shell_command"][0]["cmd"],
                "echo <%= literal %>"
            );
        }
    }
}

#[test]
fn importers_preserve_command_groups_and_saved_directory_context() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("saved")).unwrap();
    for (kind, source, expected) in [
        (
            "tmuxinator",
            serde_json::json!({"name":"demo", "pre_window":["false","echo continued"],
                "windows":[{"z":["echo first","echo second"]},{"a":{"pre":["echo pre","echo more"],"panes":[["blank","pane"],null]}}]}),
            serde_json::json!([{"cmd":"echo first"},{"cmd":"echo second"}]),
        ),
        (
            "teamocil",
            serde_json::json!({"session":{"name":"demo","windows":[{"name":"z","panes":[{"commands":["false","echo second"]}]},{"name":"a","panes":["echo last"]}]}}),
            serde_json::json!([{"cmd":"false; echo second"}]),
        ),
    ] {
        std::fs::write(directory.path().join("input.json"), source.to_string()).unwrap();
        let output = command_at(
            &[
                "import",
                kind,
                "input.json",
                "--json",
                "--workspace-format",
                "json",
                "--save-to",
                "saved/workspace.json",
                "--force",
            ],
            directory.path(),
        )
        .env("TMUX_WORKSPACE_PYTHON", "/unavailable/python")
        .env("PATH", "")
        .output()
        .unwrap();
        assert!(output.status.success(), "{kind}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("saved/workspace.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["start_directory"],
            directory.path().canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(value["windows"][0]["window_name"], "z");
        assert_eq!(value["windows"][1]["window_name"], "a");
        assert_eq!(value["windows"][0]["panes"].as_array().unwrap().len(), 1);
        assert_eq!(value["windows"][0]["panes"][0]["shell_command"], expected);
        assert_eq!(value["windows"][0]["panes"][0]["focus"], true);
        if kind == "tmuxinator" {
            assert_eq!(
                value["shell_command_before"],
                serde_json::json!([{"cmd":"false; echo continued"}])
            );
            assert_eq!(
                value["windows"][1]["shell_command_before"],
                serde_json::json!([{"cmd":"echo pre && echo more"}])
            );
            assert_eq!(
                value["windows"][1]["panes"][0]["shell_command"],
                serde_json::json!([{"cmd":"blank"},{"cmd":"pane"}])
            );
        }
    }
}

#[test]
fn importers_refuse_unrepresentable_fields_before_output_or_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let base = serde_json::json!({"name":"demo","windows":[{"main":"echo ready"}]});
    let mut cases = Vec::new();
    for key in [
        "pre",
        "post",
        "pre_tmux",
        "on_project_start",
        "cli_args",
        "tmux_options",
        "socket_name",
        "tmux_command",
        "rbenv",
        "unknown",
    ] {
        let mut value = base.clone();
        value[key] = serde_json::json!("unsupported");
        cases.push(("tmuxinator", value, key));
    }
    cases.extend([
        ("tmuxinator", serde_json::json!({"name":"demo","windows":[{"main":{"panes":[{"title":"echo ready"}]}}]}), "panes[0]"),
        ("tmuxinator", serde_json::json!({"name":"demo","windows":[{"main":{"synchronize":true,"panes":[null,null]}}]}), "synchronize"),
        ("tmuxinator", serde_json::json!({"name":"demo","windows":[{"main":null,"other":null}]}), "windows[0]"),
        ("teamocil", serde_json::json!({"session":{"name":"demo","windows":[{"name":"main","clear":true}]}}), "clear"),
        ("teamocil", serde_json::json!({"session":{"name":"demo","windows":[{"name":"main","filters":{"after":"echo after"}}]}}), "filters"),
        ("teamocil", serde_json::json!({"session":{"name":"demo","windows":[{"name":"main","panes":[{"cmd":"echo ready","width":37}]}]}}), "width"),
        ("teamocil", serde_json::json!({"session":{"name":"demo","windows":[{"name":"main","panes":[{"commands":[42]}]}]}}), "commands"),
        ("tmuxinator", serde_json::json!({"name":"demo","root":"<%= dynamic_root %>","windows":[{"main":null}]}), "ERB"),
        ("tmuxinator", serde_json::json!({"name":"demo","windows":[{"main":"echo <%= dynamic_command %>"}]}), "ERB"),
        ("tmuxinator", serde_json::json!({"name":"demo","windows":[{"<%= dynamic_window %>":null}]}), "ERB"),
        ("tmuxinator", serde_json::json!({"name":"demo","root":false,"windows":[{"main":null}]}), "root"),
        ("tmuxinator", serde_json::json!({"name":"demo","project_name":"other","windows":[{"main":null}]}), "name"),
        ("tmuxinator", serde_json::json!({"windows":[{"main":null}]}), "session_name"),
        ("tmuxinator", serde_json::json!({"name":"demo","windows":[]}), "window"),
        ("teamocil", serde_json::json!({"name":"demo","windows":[{"name":42}]}), "window_name"),
        ("teamocil", serde_json::json!({"name":"demo","windows":[{"focus":"yes"}]}), "focus"),
        ("teamocil", serde_json::json!({"name":"demo","windows":[{"options":{"@bad":[]}}]}), "@bad"),
        ("teamocil", serde_json::json!({"name":"demo","windows":[{"panes":[{"cmd":"echo old","commands":["echo new"]}]}]}), "commands"),
    ]);
    for (kind, value, key) in cases {
        std::fs::write(directory.path().join("input.json"), value.to_string()).unwrap();
        for save in [false, true] {
            std::fs::write(directory.path().join("kept.json"), "original bytes").unwrap();
            let mut args = vec!["import", kind, "input.json", "--json"];
            if save {
                args.extend(["--save-to", "kept.json", "--force"]);
            }
            let output = at(&args, directory.path());
            assert_eq!(output.status.code(), Some(1), "{kind}/{key}: {output:?}");
            assert!(output.stdout.is_empty(), "{kind}/{key}: {output:?}");
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(key),
                "{kind}/{key}: {output:?}"
            );
            assert_eq!(
                std::fs::read(directory.path().join("kept.json")).unwrap(),
                b"original bytes"
            );
        }
    }
}

fn imported_source(kind: &str) -> serde_json::Value {
    if kind == "tmuxinator" {
        serde_json::json!({"name":"imported","root":"project root",
                "pre_window":["false","printf continued > project-pre"],
                "windows":[{"z":["printf first > order","printf second >> order"]},
                    {"a":{"root":"window root","pre":["IMPORT_BEFORE=before","IMPORT_BEFORE=${IMPORT_BEFORE}more","false","touch blocked"],
                        "panes":["printf '%sfinal' \"$IMPORT_BEFORE\" > order",null],"synchronize":"after"}}]})
    } else {
        serde_json::json!({"session":{"name":"imported","root":"project root","windows":[
                {"name":"z","panes":[{"commands":["printf first > order","printf second >> order"]}]},
                {"name":"a","root":"window root","focus":true,"options":{"@import-option":"kept = value"},
                    "panes":[{"commands":["false","printf final > order"]},{"commands":[],"focus":true}]}]}})
    }
}

async fn imported_state(guard: &libtmux::test::TestServer, kind: &str) -> serde_json::Value {
    let session = guard.server().session("imported").await.unwrap().unwrap();
    let windows = session.windows().await.unwrap();
    let counts = [
        windows[0].panes().await.unwrap().len(),
        windows[1].panes().await.unwrap().len(),
    ];
    let active = session
        .active_window()
        .await
        .unwrap()
        .unwrap()
        .id()
        .to_string();
    let focused = windows[1]
        .active_pane()
        .await
        .unwrap()
        .unwrap()
        .id()
        .to_string();
    let panes = windows[1].panes().await.unwrap();
    // Conversion writes focus out explicitly, so an imported document always
    // claims a pane: teamocil's own second pane, and tmuxinator's first for
    // want of one in the source.
    let expected_focus = panes[usize::from(kind == "teamocil")].id().to_string();
    let option = windows[1]
        .get_option(if kind == "tmuxinator" {
            "synchronize-panes"
        } else {
            "@import-option"
        })
        .await
        .unwrap()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    serde_json::json!({"counts":counts,"active":active,"expected_active":windows[usize::from(kind == "teamocil")].id().to_string(),"focused":focused,"expected_focus":expected_focus,"option":option})
}

fn assert_imported_commands(kind: &str, root: &Path, runtime: &Path) {
    assert_eq!(
        std::fs::read_to_string(root.join("order")).unwrap(),
        "firstsecond"
    );
    let expected = if kind == "tmuxinator" {
        "beforemorefinal"
    } else {
        "final"
    };
    assert_eq!(
        std::fs::read_to_string(runtime.join("order")).unwrap(),
        expected
    );
    assert!(!root.join("blocked").exists());
    assert!(!runtime.join("blocked").exists());
    if kind == "tmuxinator" {
        for path in [&root, &runtime] {
            assert_eq!(
                std::fs::read_to_string(path.join("project-pre"))
                    .ok()
                    .as_deref(),
                Some("continued"),
                "project pre_window must continue after false"
            );
        }
    }
}

#[tokio::test]
async fn imported_workspaces_load_with_ordered_commands_and_relocated_roots() {
    for kind in ["tmuxinator", "teamocil"] {
        let guard = libtmux::test::TestServer::new().await.unwrap();
        let keeper = guard.session("import-keeper").await.unwrap();
        let keeper_id = keeper.id().to_string();
        let keeper_pane = current_pane(&keeper).await;
        guard
            .server()
            .set_global_option("default-shell", "/bin/sh")
            .await
            .unwrap();
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
        let root = directory.path().join("project root");
        let runtime = root.join("window root");
        let saved = directory.path().join("saved elsewhere");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir(&saved).unwrap();
        let source = imported_source(kind);
        std::fs::write(directory.path().join("input.json"), source.to_string()).unwrap();
        let output = at(
            &[
                "import",
                kind,
                "input.json",
                "--json",
                "--workspace-format",
                "json",
                "--save-to",
                "saved elsewhere/workspace.json",
            ],
            directory.path(),
        );
        assert!(output.status.success(), "{kind}: {output:?}");
        let loaded = at(
            &[
                "load",
                "-d",
                "--json",
                "-S",
                guard.socket_path().to_str().unwrap(),
                "workspace.json",
            ],
            &saved,
        );
        let mut observations = None;
        if loaded.status.success() {
            for _ in 0..200 {
                let first = std::fs::read_to_string(root.join("order")).unwrap_or_default();
                let second = std::fs::read_to_string(runtime.join("order")).unwrap_or_default();
                if first == "firstsecond" && second.ends_with("final") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            observations = Some(imported_state(&guard, kind).await);
        }
        let before_cleanup = current_pane(&keeper).await;
        let keeper_after = guard
            .server()
            .session("import-keeper")
            .await
            .unwrap()
            .unwrap()
            .id()
            .to_string();
        guard.shutdown().await.unwrap();
        assert!(loaded.status.success(), "{kind}: {loaded:?}");
        assert_imported_commands(kind, &root, &runtime);
        let state = observations.unwrap();
        assert_eq!(state["counts"], serde_json::json!([1, 2]));
        assert_eq!(state["active"], state["expected_active"]);
        assert_eq!(state["focused"], state["expected_focus"], "{kind}: {state}");
        assert_eq!(
            state["option"],
            if kind == "tmuxinator" {
                "on"
            } else {
                "kept = value"
            }
        );
        assert_eq!(before_cleanup, keeper_pane);
        assert_eq!(keeper_after, keeper_id);
    }
}

#[tokio::test]
async fn native_endpoint_fields_refuse_all_inputs_before_mutation() {
    for key in ["config", "socket_name"] {
        let guard = libtmux::test::TestServer::new().await.unwrap();
        let keeper = guard.session("endpoint-keeper").await.unwrap();
        let keeper_id = keeper.id().to_string();
        let pane = current_pane(&keeper).await;
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
        let first = serde_json::json!({"session_name":"first-endpoint","before_script":"touch marker","windows":[{}]});
        let mut second = serde_json::json!({"session_name":"second-endpoint","windows":[{}]});
        second[key] = serde_json::json!("not-the-selected-endpoint");
        std::fs::write(directory.path().join("first.json"), first.to_string()).unwrap();
        std::fs::write(directory.path().join("second.json"), second.to_string()).unwrap();
        let output = at(
            &[
                "load",
                "-d",
                "--json",
                "-S",
                guard.socket_path().to_str().unwrap(),
                "first.json",
                "second.json",
            ],
            directory.path(),
        );
        let marker = directory.path().join("marker").exists();
        let ids: Vec<_> = guard
            .server()
            .sessions()
            .await
            .unwrap()
            .iter()
            .map(|s| s.id().to_string())
            .collect();
        let after = current_pane(&keeper).await;
        guard.shutdown().await.unwrap();
        assert_eq!(output.status.code(), Some(1), "{key}: {output:?}");
        assert!(output.stdout.is_empty(), "{key}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(key),
            "{key}: {output:?}"
        );
        assert!(!marker, "{key}: earlier script ran");
        assert_eq!(ids, [keeper_id]);
        assert_eq!(after, pane);
    }
}

#[test]
fn discovery_and_search_use_workspace_fields_and_case_modes() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join(".tmuxp")).unwrap();
    std::fs::write(directory.path().join(".tmuxp/demo.yaml"), "session_name: Project\nwindows:\n  - window_name: Editor\n    panes:\n      - echo Needle\n").unwrap();
    let output = at(&["ls", "--json", "--full"], directory.path());
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["workspaces"].as_array().unwrap().len(), 1);
    assert_eq!(value["workspaces"][0]["session_name"], "Project");
    for query in [
        vec!["-i", "pane:needle"],
        vec!["-S", "session:project", "window:editor"],
        vec!["-F", "-f", "name", "demo"],
    ] {
        let mut args = vec!["search", "--json"];
        args.extend(query);
        let output = at(&args, directory.path());
        assert!(output.status.success(), "{args:?}: {output:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 1, "{args:?}: {value}");
        assert!(value[0]["matched_fields"].is_array());
    }
}

#[tokio::test]
async fn bootstrap_resolves_only_its_executable_from_the_config_directory() {
    use std::os::unix::fs::PermissionsExt;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let config_directory = directory.path().join("config directory");
    let caller = directory.path().join("caller directory");
    let runtime = config_directory.join("runtime directory");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::create_dir(&caller).unwrap();
    let config_directory = config_directory.canonicalize().unwrap();
    let caller = caller.canonicalize().unwrap();
    let runtime = runtime.canonicalize().unwrap();
    let script = config_directory.join("bootstrap script");
    std::fs::write(
        &script,
        "#!/bin/sh\nrecord=$1\nshift\nprintf '%s\\0' \"$PWD\" \"$#\" \"$@\" > \"$record\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut failures = Vec::new();
    let mut index = 0;
    for executable in [
        "./bootstrap script".to_owned(),
        "../config directory/bootstrap script".to_owned(),
        script.to_str().unwrap().to_owned(),
        "bootstrap script".to_owned(),
    ] {
        for start in [
            None,
            Some("."),
            Some("./runtime directory"),
            runtime.to_str(),
        ] {
            let name = format!("bootstrap-{index}");
            index += 1;
            let record = directory.path().join(&name);
            let mut config = serde_json::json!({
                "session_name": name,
                "before_script": format!("'{executable}' '{}' 'space argument' '' 'literal$(touch SHOULD_NOT_EXIST)'", record.display()),
                "windows": [{"panes": ["blank"]}],
            });
            if let Some(start) = start {
                config["start_directory"] = serde_json::json!(start);
            }
            let path = config_directory.join("workspace.json");
            std::fs::write(&path, config.to_string()).unwrap();
            let mut command = command_at(
                &[
                    "load",
                    "-S",
                    guard.socket_path().to_str().unwrap(),
                    "-d",
                    "--json",
                    "../config directory/workspace.json",
                ],
                &caller,
            );
            let mut search_paths = vec![config_directory.clone()];
            search_paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
            command.env("PATH", std::env::join_paths(search_paths).unwrap());
            let output = command.output().unwrap();
            let expected_cwd = match start {
                None => &caller,
                Some(".") => &config_directory,
                Some(_) => &runtime,
            };
            let expected = format!(
                "{}\0{}\0space argument\0\0literal$(touch SHOULD_NOT_EXIST)\0",
                expected_cwd.display(),
                3,
            );
            let recorded = std::fs::read(&record).ok();
            if !output.status.success() || recorded.as_deref() != Some(expected.as_bytes()) {
                failures.push(format!(
                    "{executable:?} / {start:?}: {output:?}; recorded={recorded:?}"
                ));
            }
        }
    }
    guard.shutdown().await.unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn native_load_freeze_reuse_and_append_preserve_owned_session_state() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("created");
    let source = serde_json::json!({"session_name":"cli-native","start_directory":directory.path(),"environment":{"SESSION_VALUE":"session"},"windows":[
        {"window_name":"zero","window_index":0,"environment":{"WINDOW_VALUE":"window"},"options":{"automatic-rename":false},"panes":[{"shell_command":format!("printf '%s' \"$SESSION_VALUE:$WINDOW_VALUE\" > {}; exec sleep 30",marker.display())},null]},
        {"window_name":"seven","window_index":7,"panes":["blank"]}
    ]});
    std::fs::write(directory.path().join("project.json"), source.to_string()).unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "--ndjson", "project.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.last().unwrap()["event"], "completed");
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "completed")
            .count(),
        1
    );
    assert!(events.iter().any(|event| event["event"] == "pane-created"));
    for pair in events.windows(2) {
        assert!(pair[0]["sequence"].as_u64().unwrap() < pair[1]["sequence"].as_u64().unwrap());
    }
    for _ in 0..100 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "session:window");
    let output = at(
        &["freeze", "-S", socket, "--json", "cli-native"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let captured: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(captured["windows"][0]["window_index"], 0);
    assert_eq!(captured["windows"][1]["window_index"], 7);
    assert_eq!(captured["windows"][0]["panes"].as_array().unwrap().len(), 2);
    let output = at(
        &["load", "-S", socket, "-d", "--json", "project.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    std::fs::write(
        directory.path().join("append.yaml"),
        "session_name: ignored\nwindows:\n  - window_name: appended\n    panes: [blank]\n",
    )
    .unwrap();
    let session = guard.server().session("cli-native").await.unwrap().unwrap();
    let pane = current_pane(&session).await;
    let output = at_pane(
        &[
            "load",
            "-S",
            socket,
            "-s",
            "ignored-rename",
            "--append",
            "--json",
            "append.yaml",
        ],
        directory.path(),
        Some((&guard, &pane)),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        guard
            .server()
            .session("cli-native")
            .await
            .unwrap()
            .unwrap()
            .windows()
            .await
            .unwrap()
            .len(),
        3
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_renames_only_the_last_input_and_retessellates_many_panes() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    for name in ["first", "second"] {
        std::fs::write(directory.path().join(format!("{name}.json")), serde_json::json!({"session_name":name,"windows":[{"window_name":"many","layout":"tiled","panes":vec!["blank";10]}]}).to_string()).unwrap();
    }
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &[
            "load",
            "-S",
            socket,
            "-d",
            "--json",
            "-s",
            "renamed",
            "first.json",
            "second.json",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["results"][0]["session_name"], "first");
    assert_eq!(value["results"][1]["session_name"], "renamed");
    let session = guard.server().session("renamed").await.unwrap().unwrap();
    assert_eq!(
        session
            .active_window()
            .await
            .unwrap()
            .unwrap()
            .panes()
            .await
            .unwrap()
            .len(),
        10
    );
    let output = at(
        &["load", "-S", socket, "--append", "--json", "first.json"],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(r#""code":"usage""#), "{stderr}");
    assert!(stderr.contains("TMUX_PANE"), "{stderr}");
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_logging_separates_metadata_from_captures_and_mandatory_errors() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    for (mode, level, status) in [
        ("human", "info", 0),
        ("json", "info", 9),
        ("json", "critical", 9),
    ] {
        let name = format!("logged-{mode}-{level}");
        let log = directory.path().join(format!("{name}.log"));
        let config = serde_json::json!({"session_name":name,"windows":[{"panes":["blank"]}],"before_script":format!("/bin/sh -c 'printf LOG-STDOUT; printf LOG-STDERR >&2; exit {status}'")});
        std::fs::write(directory.path().join("workspace.json"), config.to_string()).unwrap();
        let mut arguments = vec![
            "load",
            "-d",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--log-file",
            log.to_str().unwrap(),
            "--log-level",
            level,
        ];
        if mode == "json" {
            arguments.push("--json");
        }
        arguments.push("workspace.json");
        let output = bounded_cli_output(command_at(&arguments, directory.path())).await;
        assert_eq!(
            output.status.code(),
            Some(i32::from(status != 0)),
            "{output:?}"
        );
        let records = json_records(&std::fs::read(&log).unwrap());
        if level == "critical" {
            assert!(records.is_empty());
        } else {
            assert_eq!(
                records
                    .iter()
                    .filter(|row| matches!(row["event"].as_str(), Some("completed" | "failed")))
                    .count(),
                1
            );
        }
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record["sequence"], index + 1);
            assert_eq!(record["command"], "load");
            let metadata = record.to_string();
            assert!(
                !metadata.contains("script_output")
                    && !metadata.contains("LOG-STDOUT")
                    && !metadata.contains("LOG-STDERR"),
                "{record}"
            );
        }
        if mode == "human" {
            assert!(String::from_utf8_lossy(&output.stdout).contains("LOG-STDOUT"));
            assert!(String::from_utf8_lossy(&output.stderr).contains("LOG-STDERR"));
        } else {
            let values = json_records(&output.stdout);
            let effects = &values.last().unwrap()["errors"][0]["effects"];
            assert_eq!(effects["script_output"]["stdout"], "LOG-STDOUT");
            assert_eq!(effects["script_output"]["stderr"], "LOG-STDERR");
            assert_eq!(effects["script_output"]["child_status"], status);
            let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
            assert_eq!(diagnostic["code"], "script_failed");
        }
        assert_eq!(
            guard.server().has_session(&name).await.unwrap(),
            status == 0
        );
    }
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn load_logging_caps_utf8_per_stream_and_resets_for_the_next_child() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let limit = 1024 * 1024;
    for (stream, letter) in [("stdout", "a"), ("stderr", "b")] {
        std::fs::write(
            directory.path().join(stream),
            format!("{}éTAIL", letter.repeat(limit - 1)),
        )
        .unwrap();
    }
    for (name, script) in [
        ("first", "/bin/sh -c 'cat stdout; cat stderr >&2'"),
        (
            "second",
            "/bin/sh -c 'printf RESET-OUT; printf RESET-ERR >&2'",
        ),
    ] {
        let config = serde_json::json!({"session_name":name,"before_script":script,"windows":[{"panes":["blank"]}]});
        std::fs::write(
            directory.path().join(format!("{name}.json")),
            config.to_string(),
        )
        .unwrap();
    }
    let output = bounded_cli_output(command_at(
        &[
            "load",
            "-d",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--ndjson",
            "--log-file",
            "debug.log",
            "--log-level",
            "debug",
            "first.json",
            "second.json",
        ],
        directory.path(),
    ))
    .await;
    assert!(output.status.success(), "status={:?}", output.status.code());
    let records = json_records(&std::fs::read(directory.path().join("debug.log")).unwrap());
    assert_logged_children(&records, limit);
    let events = json_records(&output.stdout);
    assert_eq!(
        events
            .iter()
            .filter(|row| row["event"] == "completed")
            .count(),
        1
    );
    let summary = events.last().unwrap();
    assert_eq!(summary["results"][0]["script_output"]["truncated"], true);
    assert_eq!(
        summary["results"][1]["script_output"]["stdout"],
        "RESET-OUT"
    );
    for name in ["first", "second"] {
        assert!(guard.server().has_session(name).await.unwrap());
    }
    guard.shutdown().await.unwrap();
}

fn assert_logged_children(records: &[serde_json::Value], limit: usize) {
    let mut children: [Vec<&serde_json::Value>; 2] = Default::default();
    let mut index = 0;
    for record in records {
        if record["event"] == "workspace-started" {
            index = usize::try_from(record["data"]["input_index"].as_u64().unwrap()).unwrap();
        } else if record["event"] == "script-output" {
            children[index].push(record);
        }
    }
    for (stream, letter, reset) in [("stdout", b'a', "RESET-OUT"), ("stderr", b'b', "RESET-ERR")] {
        let first: Vec<_> = children[0]
            .iter()
            .filter(|row| row["data"]["stream"] == stream)
            .collect();
        let text: String = first
            .iter()
            .map(|row| row["data"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(text.len(), limit - 1);
        assert!(text.bytes().all(|byte| byte == letter));
        assert_eq!(
            first
                .iter()
                .filter(|row| row["data"]["truncated"] == true)
                .count(),
            1
        );
        let second: Vec<_> = children[1]
            .iter()
            .filter(|row| row["data"]["stream"] == stream)
            .collect();
        let text: String = second
            .iter()
            .map(|row| row["data"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(text, reset);
        assert!(second.iter().all(|row| row["data"]["truncated"] == false));
    }
    assert_eq!(
        records
            .iter()
            .filter(|row| row["event"] == "completed")
            .count(),
        1
    );
}

#[tokio::test]
async fn load_logging_failure_preserves_primary_status_and_diagnostic_order() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let cases = [
        ("human", 0, "merged", "info"),
        ("json", 9, "merged", "info"),
        ("json", 0, "stderr-closed", "info"),
        ("json", 0, "both-closed", "info"),
        ("json", 9, "both-closed", "info"),
        ("json", 9, "merged", "error"),
    ];
    for (number, (mode, status, streams, level)) in cases.into_iter().enumerate() {
        let name = format!("limited-{number}");
        let config = serde_json::json!({"session_name":name,"before_script":format!("/bin/sh -c 'printf captured; exit {status}'"),"windows":[{"panes":["blank"]}]});
        std::fs::write(directory.path().join("workspace.json"), config.to_string()).unwrap();
        let mut arguments = vec![
            "load",
            "-d",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--log-file",
            "limited.log",
            "--log-level",
            level,
        ];
        let mode_flag = format!("--{mode}");
        if mode != "human" {
            arguments.push(&mode_flag);
        }
        arguments.push("workspace.json");
        let (exit, bytes) =
            file_limited_output(&command_at(&arguments, directory.path()), streams).await;
        let expected = i32::from(status != 0 || streams == "both-closed");
        assert_eq!(exit.code(), Some(expected), "mode={mode} streams={streams}");
        assert_eq!(
            std::fs::metadata(directory.path().join("limited.log"))
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            guard.server().has_session(&name).await.unwrap(),
            status == 0
        );
        let text = String::from_utf8(bytes).unwrap();
        let warning = streams == "merged" && level == "info";
        assert_eq!(
            text.matches("log file disabled:").count(),
            usize::from(warning),
            "mode={mode} streams={streams} level={level}: {text}"
        );
        if warning && mode == "human" {
            assert!(text.find("Loaded").unwrap() < text.find("log file disabled:").unwrap());
        }
        if mode != "human" && streams != "both-closed" {
            let values = json_records(text.as_bytes());
            let summary = values
                .iter()
                .find(|row| row.get("results").is_some())
                .unwrap();
            // Each case is a lone input with an owned session; a
            // failure removes it, so nothing is retained and status is
            // error, not partial.
            assert_eq!(summary["status"], if status == 0 { "ok" } else { "error" });
            if status != 0 && streams == "merged" {
                if warning {
                    let primary = values
                        .iter()
                        .position(|row| row["code"] == "script_failed")
                        .unwrap();
                    let advisory = values
                        .iter()
                        .position(|row| row["code"] == "log_file_failed")
                        .unwrap();
                    assert!(primary < advisory);
                }
                let diagnostic = values
                    .iter()
                    .find(|row| row["code"] == "script_failed")
                    .unwrap();
                assert_eq!(
                    diagnostic["retained_state"]["errors"][0]["effects"]["script_output"]["child_status"],
                    9
                );
            }
        }
    }
    guard.shutdown().await.unwrap();
}

async fn file_limited_output(
    original: &Command,
    streams: &str,
) -> (std::process::ExitStatus, Vec<u8>) {
    use std::{
        io::Read,
        os::{fd::OwnedFd, unix::net::UnixStream},
        time::Duration,
    };

    let mut command = Command::new("/bin/sh");
    command
        .args([
            "-c",
            "trap '' XFSZ; ulimit -f 0 || exit 97; exec \"$@\"",
            "log-limit",
        ])
        .arg(original.get_program())
        .args(original.get_args())
        .current_dir(original.get_current_dir().unwrap());
    for (key, value) in original.get_envs() {
        if let Some(value) = value {
            command.env(key, value);
        } else {
            command.env_remove(key);
        }
    }
    let (mut reader, writer) = UnixStream::pair().unwrap();
    reader
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (closed_reader, closed_writer) = UnixStream::pair().unwrap();
    drop(closed_reader);
    let stdout = if streams == "both-closed" {
        closed_writer.try_clone().unwrap()
    } else {
        writer.try_clone().unwrap()
    };
    let stderr = if streams == "merged" {
        writer
    } else {
        drop(writer);
        closed_writer
    };
    command
        .stdout(OwnedFd::from(stdout))
        .stderr(OwnedFd::from(stderr));
    let mut child = tokio::process::Command::from(command)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    (exit, bytes)
}

async fn bounded_cli_output(mut command: Command) -> Output {
    command.stdin(std::process::Stdio::null());
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::from(command)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}

fn json_records(bytes: &[u8]) -> Vec<serde_json::Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn closed_load_output_preserves_completed_inputs_and_child_failure() {
    use std::os::{fd::OwnedFd, unix::net::UnixStream};

    for status in [0, 9] {
        let guard = libtmux::test::TestServer::new().await.unwrap();
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
        for name in ["first", "second"] {
            let mut config =
                serde_json::json!({"session_name":name,"windows":[{"panes":["blank"]}]});
            if name == "second" {
                config["before_script"] = serde_json::json!(format!(
                    "/bin/sh -c 'printf captured-out; printf captured-err >&2; exit {status}'"
                ));
            }
            std::fs::write(
                directory.path().join(format!("{name}.json")),
                config.to_string(),
            )
            .unwrap();
        }
        let (reader, writer) = UnixStream::pair().unwrap();
        drop(reader);
        let output = command_at(
            &[
                "load",
                "-d",
                "-S",
                guard.socket_path().to_str().unwrap(),
                "--json",
                "first.json",
                "second.json",
            ],
            directory.path(),
        )
        .stdin(std::process::Stdio::null())
        .stdout(OwnedFd::from(writer))
        .output()
        .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        let retained = &diagnostic["retained_state"];
        let results = retained["results"].as_array().expect("completed inputs");
        // One record per input attempted, the failed one included, so
        // "second" is here either way; only status == 0 leaves its session
        // standing to look up.
        assert_eq!(results.len(), 2, "{diagnostic}");
        let first_session = guard
            .server()
            .session(results[0]["session_name"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(results[0]["session_id"], first_session.id().as_ref());
        if status == 0 {
            let second_session = guard
                .server()
                .session(results[1]["session_name"].as_str().unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(results[1]["session_id"], second_session.id().as_ref());
        } else {
            assert_eq!(results[1]["session_name"], "second");
        }
        let second = if status == 0 {
            &results[1]
        } else {
            &retained["errors"][0]["effects"]
        };
        assert_eq!(second["script_output"]["child_status"], status);
        assert_eq!(second["script_output"]["stdout"], "captured-out");
        assert_eq!(second["script_output"]["stderr"], "captured-err");
        if status != 0 {
            assert_eq!(diagnostic["code"], "script_failed");
            assert!(
                diagnostic["message"]
                    .as_str()
                    .unwrap()
                    .contains("output failed")
            );
        }
        assert_eq!(
            guard.server().has_session("second").await.unwrap(),
            status == 0
        );
        guard.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn failing_before_script_removes_the_owned_session_and_reads_as_one_sentence() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("bf.yaml"),
        "session_name: bsfail\nbefore_script: /bin/sh -c 'exit 3'\nwindows:\n  - panes: [echo x]\n",
    )
    .unwrap();
    let output = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "bf.yaml",
        ],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!guard.server().has_session("bsfail").await.unwrap());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Human mode: one sentence, no machine record and no escaped newline.
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(!stderr.contains('{') && !stderr.contains("\\n"), "{stderr}");
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn before_script_that_cannot_start_removes_the_owned_session_too() {
    // tmuxp raises BeforeLoadScriptNotExists for a missing or
    // non-executable before_script; this port treats it exactly like a
    // nonzero exit, not a different failure. A path under a fresh tempdir
    // that nothing ever created stays missing without depending on any
    // fixed system binary (no /bin/false: macOS carries none).
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let missing = directory.path().join("missing-script");
    std::fs::write(
        directory.path().join("bf.yaml"),
        format!(
            "session_name: nostart\nbefore_script: {}\nwindows:\n  - panes: [echo x]\n",
            missing.display()
        ),
    )
    .unwrap();
    let output = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "--json",
            "bf.yaml",
        ],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!guard.server().has_session("nostart").await.unwrap());
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "script_failed", "{diagnostic}");
    let retained = &diagnostic["retained_state"];
    let effects = &retained["errors"][0]["effects"];
    assert_eq!(effects["owned_session"], true);
    assert_eq!(effects["stage"], "before-script");
    // Nothing completed and the owned session was removed, so this is
    // error, not partial, and the single attempted input still gets a
    // results[] record with the minimum fields.
    assert_eq!(retained["status"], "error", "{retained}");
    let results = retained["results"]
        .as_array()
        .expect("results[] present even on this failure");
    assert_eq!(results.len(), 1, "{retained}");
    assert_eq!(results[0]["input_index"], 0);
    assert!(results[0]["input"].as_str().unwrap().ends_with("bf.yaml"));
    assert_eq!(results[0]["session_name"], "nostart");
    assert_eq!(results[0]["reused"], false);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn appended_before_script_failure_leaves_the_borrowed_session_and_stays_partial() {
    // A failed input's session is never rolled back when it is
    // borrowed (append), so this is "partial" (effects left behind), not
    // "error", unlike the owned case above.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("owner"))
        .await
        .unwrap();
    let pane = current_pane(&session).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let missing = directory.path().join("missing-script");
    std::fs::write(
        directory.path().join("bf.yaml"),
        format!(
            "session_name: ignored\nbefore_script: {}\nwindows:\n  - panes: [echo x]\n",
            missing.display()
        ),
    )
    .unwrap();
    let output = at_pane(
        &[
            "load",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--append",
            "--json",
            "bf.yaml",
        ],
        directory.path(),
        Some((&guard, &pane)),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(guard.server().has_session("owner").await.unwrap());
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "script_failed", "{diagnostic}");
    let retained = &diagnostic["retained_state"];
    assert_eq!(retained["status"], "partial", "{retained}");
    assert_eq!(retained["errors"][0]["effects"]["owned_session"], false);
    let results = retained["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{retained}");
    assert_eq!(results[0]["session_name"], "owner");
    assert_eq!(results[0]["reused"], false);
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn closed_workspace_completed_event_retains_the_completed_input() {
    use tokio::io::AsyncReadExt;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let capture = "x".repeat(256 * 1024);
    std::fs::write(directory.path().join("capture"), &capture).unwrap();
    std::fs::write(directory.path().join("workspace.json"), serde_json::json!({"session_name":"streamed","before_script":"/bin/cat capture","windows":[{"panes":["blank"]}]}).to_string()).unwrap();
    let mut child = tokio::process::Command::from(command_at(
        &[
            "load",
            "-d",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--ndjson",
            "workspace.json",
        ],
        directory.path(),
    ))
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let marker = b"\"event\":\"workspace-completed\"";
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut received = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let count = stdout.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0, "workspace-completed event absent");
            received.extend_from_slice(&buffer[..count]);
            if received.windows(marker.len()).any(|part| part == marker) {
                break;
            }
            if received.len() > marker.len() {
                received.drain(..received.len() - marker.len());
            }
        }
    })
    .await
    .unwrap();
    drop(stdout);
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["retained_state"]["errors"][0]["input_index"], 0);
    assert_eq!(
        diagnostic["retained_state"]["errors"][0]["effects"]["stage"],
        "completed"
    );
    let results = diagnostic["retained_state"]["results"]
        .as_array()
        .expect("completed input");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["script_output"]["stdout"], capture);
    let session = guard.server().session("streamed").await.unwrap().unwrap();
    assert_eq!(results[0]["session_id"], session.id().as_ref());
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_append_reports_borrowed_session_and_created_ids() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("borrowed"))
        .await
        .unwrap();
    let pane = session
        .active_window()
        .await
        .unwrap()
        .unwrap()
        .active_pane()
        .await
        .unwrap()
        .unwrap()
        .id()
        .to_string();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("broken.yaml"), "session_name: different\nwindows:\n  - window_name: created\n    panes: [blank]\n    options_after:\n      invalid-workspace-test-option: true\n").unwrap();
    let output = at_pane(
        &[
            "load",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--append",
            "--json",
            "broken.yaml",
        ],
        directory.path(),
        Some((&guard, &pane)),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "partial");
    assert_eq!(value["errors"][0]["effects"]["session_name"], "borrowed");
    assert_eq!(value["errors"][0]["effects"]["owned_session"], false);
    assert_eq!(
        value["errors"][0]["effects"]["window_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(guard.server().has_session("borrowed").await.unwrap());
    guard.shutdown().await.unwrap();
}

/// An appended window whose `window_index` collides with one already in the
/// session states tmux's own exit status as a number, not `Option`'s `Debug`.
#[tokio::test]
async fn append_index_collision_states_the_exit_status_plainly() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard.session("borrowed").await.unwrap();
    let pane = current_pane(&session).await;
    let existing_index = session.active_window().await.unwrap().unwrap().index();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("collide.json"),
        serde_json::json!({
            "session_name":"ignored",
            "windows":[{"window_name":"created","window_index":existing_index,"panes":["blank"]}]
        })
        .to_string(),
    )
    .unwrap();
    let output = at_pane(
        &[
            "load",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--append",
            "collide.json",
        ],
        directory.path(),
        Some((&guard, &pane)),
    );
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("Some("), "{stderr}");
    assert!(
        stderr.contains(&format!("index {existing_index} in use")),
        "{stderr}"
    );
    guard.shutdown().await.unwrap();
}

/// `-d` always wins over `--append`: a detached load builds a new session
/// rather than appending into the current pane's, inside tmux and outside.
#[tokio::test]
async fn detached_flag_beats_append_inside_and_outside_tmux() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({"session_name":"detached-wins","windows":[{"panes":["blank"]}]})
            .to_string(),
    )
    .unwrap();
    let socket = guard.socket_path().to_str().unwrap();
    for inside in [false, true] {
        let output = at_pane(
            &["load", "workspace.json", "-S", socket, "-d", "--append"],
            directory.path(),
            inside.then_some((&guard, pane.as_str())),
        );
        assert!(output.status.success(), "inside={inside}: {output:?}");
        let built = guard.server().session("detached-wins").await.unwrap();
        let built = built.unwrap_or_else(|| {
            panic!("inside={inside}: -d --append did not build a detached session")
        });
        built.kill().await.unwrap();
        assert_eq!(
            keeper.windows().await.unwrap().len(),
            1,
            "inside={inside}: -d --append appended into the current session"
        );
    }
    guard.shutdown().await.unwrap();
}

/// After appending, the human summary names the session that received the
/// windows, with a single space, not `Loaded` with a doubled one.
#[tokio::test]
async fn append_summary_says_appended_with_a_single_space() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({"session_name":"ignored","windows":[{"window_name":"added","panes":["blank"]}]})
            .to_string(),
    )
    .unwrap();
    let output = at_pane(
        &[
            "load",
            "workspace.json",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--append",
        ],
        directory.path(),
        Some((&guard, pane.as_str())),
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Appended keeper\n"), "{stdout:?}");
    assert!(!stdout.contains("Loaded"), "{stdout:?}");
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn append_leaves_the_client_on_its_window_unless_one_appended_window_focuses() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("owner"))
        .await
        .unwrap();
    let original = session.active_window().await.unwrap().unwrap();
    let pane = current_pane(&session).await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("append.yaml"),
        "session_name: ignored\nwindows:\n  - window_name: one\n    panes: [blank]\n  - window_name: two\n    panes: [blank]\n",
    )
    .unwrap();
    let output = at_pane(
        &[
            "load",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--append",
            "--json",
            "append.yaml",
        ],
        directory.path(),
        Some((&guard, &pane)),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        session.active_window().await.unwrap().unwrap().id(),
        original.id(),
        "no appended window set focus, so the client should not have moved"
    );

    std::fs::write(
        directory.path().join("focused.yaml"),
        "session_name: ignored\nwindows:\n  - window_name: three\n    panes: [blank]\n  - window_name: four\n    focus: true\n    panes: [blank]\n",
    )
    .unwrap();
    let output = at_pane(
        &[
            "load",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--append",
            "--json",
            "focused.yaml",
        ],
        directory.path(),
        Some((&guard, &pane)),
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        session
            .active_window()
            .await
            .unwrap()
            .unwrap()
            .name()
            .to_string_lossy(),
        "four",
        "an appended window naming focus: true should still move the client"
    );
    guard.shutdown().await.unwrap();
}

#[test]
fn invalid_execution_models_are_rejected_before_creating_a_server() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("bad.yaml"), "session_name: bad\nwindows:\n  - panes:\n      - shell_command: [{cmd: hello, sleep_before: -1}]\n").unwrap();
    let output = cli(&[
        "load",
        "-d",
        "--json",
        directory.path().join("bad.yaml").to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sleep_before"),
        "{output:?}"
    );
}

#[tokio::test]
async fn machine_error_codes_match_the_ten_condition_table() {
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(directory.path().join("bad.yaml"), "a: [\n").unwrap();
    std::fs::write(
        directory.path().join("bogus.yaml"),
        "session_name: x\nbogus: 1\nwindows:\n  - panes: [echo hi]\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("ok.yaml"),
        "session_name: ok\nwindows:\n  - panes: [echo hi]\n",
    )
    .unwrap();

    let missing = cli(&[
        "load",
        "-d",
        "--json",
        directory.path().join("nosuchfile.yaml").to_str().unwrap(),
    ]);
    let malformed = cli(&[
        "load",
        "-d",
        "--json",
        directory.path().join("bad.yaml").to_str().unwrap(),
    ]);
    let unsupported = cli(&[
        "load",
        "-d",
        "--json",
        directory.path().join("bogus.yaml").to_str().unwrap(),
    ]);
    // `cli()` already runs with PATH stripped of tmux and no
    // LIBTMUX_TEST_TMUX override; an otherwise-valid document reaches the
    // point of actually starting it.
    let unavailable = cli(&[
        "load",
        "-d",
        "--json",
        directory.path().join("ok.yaml").to_str().unwrap(),
    ]);
    for (output, code) in [
        (&missing, "workspace_not_found"),
        (&malformed, "invalid_workspace"),
        (&unsupported, "unsupported_key"),
        (&unavailable, "tmux_unavailable"),
    ] {
        let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["code"], code, "{output:?}");
    }

    let guard = libtmux::test::TestServer::new().await.unwrap();

    std::fs::write(
        directory.path().join("badoption.yaml"),
        "session_name: bo\nwindows:\n  - options:\n      no-such-option-xyz: 1\n    panes: [echo hi]\n",
    )
    .unwrap();
    let refused = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "--json",
            "badoption.yaml",
        ],
        directory.path(),
    );
    let diagnostic: serde_json::Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(diagnostic["code"], "tmux_failed", "{refused:?}");

    guard.session("kept").await.unwrap();
    std::fs::write(directory.path().join("kept.yaml"), "exists").unwrap();
    let clobbered = at(
        &[
            "freeze",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--json",
            "-o",
            "kept.yaml",
            "kept",
        ],
        directory.path(),
    );
    let diagnostic: serde_json::Value = serde_json::from_slice(&clobbered.stderr).unwrap();
    assert_eq!(diagnostic["code"], "destination_exists", "{clobbered:?}");

    let output = at(
        &[
            "freeze",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--json",
            "nosuch",
        ],
        directory.path(),
    );
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "session_not_found", "{output:?}");
    guard.shutdown().await.unwrap();
}

/// A socket whose server has never started holds no session either, so freeze
/// gives the answer a name that is not running gets, not tmux's transport
/// error.
#[tokio::test]
async fn freezing_a_socket_with_no_server_names_the_missing_session() {
    let directory = tempfile::tempdir().unwrap();
    let cold = directory.path().join("cold.sock");
    let output = at(
        &["freeze", "-S", cold.to_str().unwrap(), "--json", "nosuch"],
        directory.path(),
    );
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "session_not_found", "{output:?}");
    assert!(
        !diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("no server"),
        "{output:?}"
    );

    // Same for the bare form, which picks the sole session when there is one.
    let output = at(
        &["freeze", "-S", cold.to_str().unwrap(), "--json"],
        directory.path(),
    );
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "session_not_found", "{output:?}");
    assert!(
        !diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("no server"),
        "{output:?}"
    );
}

#[tokio::test]
async fn unreadable_pane_state_is_reported_as_itself_rather_than_a_timeout() {
    use std::os::unix::fs::PermissionsExt;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let wrapper = directory.path().join("tmux-no-cursor");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = '#{cursor_x},#{cursor_y}' ]; then\n    echo 'pane probe refused' >&2\n    exit 1\n  fi\ndone\nexec \"$REAL_TMUX\" \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = serde_json::json!({
        "session_name":"unreadable-readiness",
        "workspace_builder_options":{"pane_readiness":"always"},
        "windows":[{"panes":["blank"]}]
    });
    std::fs::write(directory.path().join("project.json"), source.to_string()).unwrap();
    let started = std::time::Instant::now();
    let output = command_at(
        &[
            "load",
            "-d",
            "--ndjson",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "project.json",
        ],
        directory.path(),
    )
    .env("LIBTMUX_TEST_TMUX", &wrapper)
    .env("REAL_TMUX", guard.server().tmux_executable())
    .output()
    .unwrap();
    let elapsed = started.elapsed();
    guard.shutdown().await.unwrap();
    let warning = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["event"] == "warning")
        .unwrap_or_else(|| panic!("{output:?}"));
    assert_ne!(warning["code"], "pane_readiness_timeout", "{warning}");
    assert!(
        warning["message"]
            .as_str()
            .unwrap_or_default()
            .contains("pane probe refused"),
        "{warning}"
    );
    assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
}

/// A builder setting this one has no equivalent for names the key and loads
/// the document anyway: a workspace shared between tools stays loadable here.
#[tokio::test]
async fn a_builder_setting_this_tool_lacks_is_named_and_still_loads() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("other.json"),
        serde_json::json!({
            "session_name":"other-builder",
            "workspace_builder_options":{"pane_readines":"never"},
            "windows":[{"panes":["blank"]}]
        })
        .to_string(),
    )
    .unwrap();
    let output = at(
        &[
            "load",
            "-d",
            "--ndjson",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "other.json",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let warned = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.contains("\"warning\"") && line.contains("pane_readines"));
    assert!(warned, "{output:?}");
    assert!(
        guard
            .server()
            .session("other-builder")
            .await
            .unwrap()
            .is_some()
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn bootstrap_output_streams_escaped_records_before_the_child_finishes() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("bootstrap.sh"),
        "printf 'ready\\t雪\\033[31m\\n'\nwhile ! test -f released; do sleep 0.01; done\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("stream.yaml"),
        "session_name: streamed\nbefore_script: sh bootstrap.sh\nwindows:\n  - panes: [blank]\n",
    )
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args([
            "load",
            "-d",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--ndjson",
            "stream.yaml",
        ])
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .env_remove("TMUX")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let saw_script = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            assert!(!line.contains('\x1b'));
            let event: serde_json::Value = serde_json::from_str(&line).unwrap();
            if event["event"] == "script-output" {
                return event["text"]
                    .as_str()
                    .unwrap()
                    .contains("ready\t雪\x1b[31m");
            }
        }
        false
    })
    .await
    .unwrap();
    std::fs::write(directory.path().join("released"), "release").unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(saw_script, "{output:?}");
    assert!(output.status.success(), "{output:?}");
    guard.shutdown().await.unwrap();
}

#[test]
fn editor_arguments_and_failure_status_are_preserved_in_machine_mode() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("project.yaml"),
        "session_name: editor\nwindows: []\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "project.yaml", "--json"])
        .current_dir(directory.path())
        .env(
            "EDITOR",
            "sh -c 'test -f \"$1\"; printf edited; exit 7' editor",
        )
        .env("HOME", directory.path())
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["child_status"], 7);
    assert_eq!(result["stdout"], "edited");
}

#[test]
fn diagnostics_are_structured_without_contacting_a_default_server() {
    let output = cli(&["debug-info", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["port"], "rust");
    assert_eq!(result["workspace_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(result["tmux_available"], false);
}

#[test]
fn missing_python_bridge_has_a_precise_runtime_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["shell", "--json", "-c", "print('bridge')"])
        .env("TMUX_WORKSPACE_PYTHON", "/nonexistent/python")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["code"], "python_runtime");
}

#[tokio::test]
#[ignore = "requires TMUX_WORKSPACE_PYTHON with tmuxp 1.74.0"]
async fn python_shell_uses_the_checked_console_entrypoint() {
    let python = std::env::var_os("TMUX_WORKSPACE_PYTHON").expect("bridge interpreter");
    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("bridge"))
        .await
        .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args([
            "shell",
            "-S",
            guard.server().socket_path().to_str().unwrap(),
            "--json",
            "--code",
            "-c",
            "print(session.name)",
            "bridge",
        ])
        .env("TMUX_WORKSPACE_PYTHON", python)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["stdout"].as_str().unwrap().ends_with("\nbridge\n"));
    guard.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires TMUX_WORKSPACE_PYTHON with tmuxp 1.74.0"]
async fn python_extension_bridge_builds_and_preserves_borrowed_session_on_failure() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("extension.py"), "from tmuxp.workspace.builder.classic import ClassicWorkspaceBuilder\nfrom tmuxp.plugin import TmuxpPlugin\nclass Plugin(TmuxpPlugin):\n    def before_workspace_builder(self, session):\n        session.set_environment('PLUGIN_CALLED', 'yes')\nclass Builder(ClassicWorkspaceBuilder):\n    def build(self, *args, **kwargs):\n        super().build(*args, **kwargs)\n        self.session.set_environment('BRIDGE_CALLED', 'yes')\n").unwrap();
    let config = serde_json::json!({"session_name":"extension","workspace_builder":"extension:Builder","plugins":["extension.Plugin"],"workspace_builder_paths":[directory.path()],"windows":[{"window_name":"native","panes":["blank"]}]});
    std::fs::write(directory.path().join("extension.json"), config.to_string()).unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "-2", "--json", "extension.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let session = guard.server().session("extension").await.unwrap().unwrap();
    let environment = session.environment_all().await.unwrap();
    assert_eq!(
        environment.get("BRIDGE_CALLED"),
        Some(&libtmux::EnvironmentEntry::Set("yes".into()))
    );
    assert_eq!(
        environment.get("PLUGIN_CALLED"),
        Some(&libtmux::EnvironmentEntry::Set("yes".into()))
    );
    let pane = current_pane(&session).await;
    let mut broken = config;
    broken["before_script"] = serde_json::json!("sh -c 'exit 9'");
    std::fs::write(directory.path().join("broken.json"), broken.to_string()).unwrap();
    let output = at_pane(
        &["load", "-S", socket, "--append", "--json", "broken.json"],
        directory.path(),
        Some((&guard, &pane)),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(guard.server().has_session("extension").await.unwrap());
    guard.shutdown().await.unwrap();
}

#[test]
fn interrupted_editor_terminates_its_owned_child_group() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let marker = directory.path().join("child.pid");
    let script = directory.path().join("editor.sh");
    std::fs::write(
        &script,
        format!(
            "sleep 30 &\nprintf '%s' $! > '{}'\nwait\n",
            marker.display()
        ),
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "workspace.yaml", "--ndjson"])
        .current_dir(directory.path())
        .env("EDITOR", format!("sh {}", script.display()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let pid = std::fs::read_to_string(&marker).unwrap();
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let descendant_alive = loop {
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid])
            .output()
            .unwrap();
        assert!(state.status.success() || state.status.code() == Some(1));
        let state = String::from_utf8(state.stdout).unwrap();
        // A killed orphan may remain a zombie until its new parent reaps it.
        let alive = !state.trim().is_empty() && !state.trim().starts_with('Z');
        if !alive || std::time::Instant::now() >= deadline {
            break alive;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    if descendant_alive {
        let _ = Command::new("kill").args(["-KILL", &pid]).status();
    }
    assert_eq!(output.status.code(), Some(130), "{output:?}");
    assert!(
        !descendant_alive,
        "owned editor descendant survived interrupt"
    );
}

#[test]
fn machine_editor_uses_a_controlling_terminal_without_contaminating_json() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let script = r"
import errno, fcntl, json, os, pathlib, pty, subprocess, sys, termios
master, slave = pty.openpty()
def session():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
env = dict(os.environ, EDITOR='sh -c \'test -t 0 && test -t 1 && printf EDITOR_TTY\' editor')
with open('result.json', 'wb') as result:
    child = subprocess.Popen([sys.argv[1], 'edit', 'workspace.yaml', '--json'], stdin=slave, stdout=result, stderr=slave, env=env, preexec_fn=session)
os.close(slave)
text = b''
while True:
    try:
        chunk = os.read(master, 4096)
    except OSError as error:
        if error.errno == errno.EIO: break
        raise
    if not chunk: break
    text += chunk
os.close(master)
assert child.wait(timeout=3) == 0, text
assert b'EDITOR_TTY' in text, text
result = json.loads(pathlib.Path('result.json').read_text())
assert result['terminal'] is True, result
assert result['stdout'] == '', result
";
    let output = Command::new("python3")
        .args(["-c", script, env!("CARGO_BIN_EXE_tmux-workspace")])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn exited_editor_cannot_leave_a_descendant_holding_capture_pipes() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let marker = directory.path().join("child.pid");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "workspace.yaml", "--json"])
        .current_dir(directory.path())
        .env(
            "EDITOR",
            format!(
                "sh -c 'sleep 30 & echo $! > {}; printf ready' editor",
                marker.display()
            ),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let completed = child.try_wait().unwrap().is_some();
    if !completed {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let pid = std::fs::read_to_string(marker).unwrap();
    let _ = Command::new("kill")
        .args(["-KILL", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status();
    assert!(completed && output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["stdout"], "ready");
    assert_eq!(value["truncated"], true);
}

#[test]
fn successful_editor_can_leave_a_background_service_with_closed_streams() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: demo\nwindows: []\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
        .args(["edit", "workspace.yaml", "--json"])
        .current_dir(directory.path())
        .env(
            "EDITOR",
            "sh -c 'sleep 30 >/dev/null 2>&1 & echo $!' editor",
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let pid = value["stdout"].as_str().unwrap().trim();
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let _ = Command::new("kill")
        .args(["-KILL", pid])
        .stderr(std::process::Stdio::null())
        .status();
    assert!(
        state.status.success() && !state.stdout.starts_with(b"Z"),
        "{state:?}"
    );
}

#[tokio::test]
async fn load_places_panes_after_the_first_in_config_order() {
    // tmux inserts a detached split immediately after its source pane, so
    // splitting the same target repeatedly reverses everything after it.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: paneorder\nwindows:\n  - window_name: plain\n    panes:\n      - printf 'MARK-A\\n'; sleep 300\n      - printf 'MARK-B\\n'; sleep 300\n      - printf 'MARK-C\\n'; sleep 300\n      - printf 'MARK-D\\n'; sleep 300\n",
    )
    .unwrap();
    let socket = guard.socket_path().to_str().unwrap();
    let output = at(
        &["load", "workspace.yaml", "-S", socket, "-d"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");

    let session = guard
        .server()
        .session("paneorder")
        .await
        .unwrap()
        .expect("session was created");
    let window = session.windows().await.unwrap().remove(0);
    let panes = window.panes().await.unwrap();
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
        assert!(seen.is_ok(), "pane at index {index} should show {marker}");
    }

    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn ndjson_load_emits_completion_events_with_tmux_ids() {
    // A streaming consumer needs to know when a pane or window finishes,
    // and pane-created needs ids to correlate without a second lookup.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({
            "session_name":"ndjson-events",
            "windows":[
                {"window_name":"one","panes":["blank","blank"]},
                {"window_name":"two","panes":["blank"]}
            ]
        })
        .to_string(),
    )
    .unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "--ndjson", "workspace.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    for (name, expected) in [
        ("window-created", 2),
        ("window-completed", 2),
        ("pane-created", 3),
        ("pane-completed", 3),
    ] {
        assert_eq!(
            events.iter().filter(|event| event["event"] == name).count(),
            expected,
            "{name}: {events:#?}"
        );
    }

    for event in events
        .iter()
        .filter(|event| event["event"] == "pane-created" || event["event"] == "pane-completed")
    {
        assert!(
            event["session_id"].as_str().unwrap().starts_with('$'),
            "{event}"
        );
        assert!(
            event["window_id"].as_str().unwrap().starts_with('@'),
            "{event}"
        );
        assert!(
            event["pane_id"].as_str().unwrap().starts_with('%'),
            "{event}"
        );
        assert!(event["pane_index"].is_u64(), "{event}");
    }
    for event in events
        .iter()
        .filter(|event| event["event"] == "window-created" || event["event"] == "window-completed")
    {
        assert!(
            event["session_id"].as_str().unwrap().starts_with('$'),
            "{event}"
        );
        assert!(
            event["window_id"].as_str().unwrap().starts_with('@'),
            "{event}"
        );
        assert!(event["window_index"].is_u64(), "{event}");
    }

    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn ndjson_brackets_before_script_with_started_and_completed_events() {
    // A consumer needs to know when a script began, ended, with
    // what status, and which input it belongs to, matching go's three
    // events instead of an unbracketed, input-less script-output.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({
            "session_name":"script-events",
            "before_script":"/bin/sh -c 'printf out; printf err >&2; exit 0'",
            "windows":[{"panes":["blank"]}],
        })
        .to_string(),
    )
    .unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "--ndjson", "workspace.json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    let names = |event: &&serde_json::Value| event["event"].as_str().unwrap().starts_with("script");
    let script_events: Vec<_> = events.iter().filter(names).collect();
    assert_eq!(
        script_events
            .iter()
            .map(|event| event["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "script-started",
            "script-output",
            "script-output",
            "script-completed"
        ],
        "{events:#?}"
    );
    for event in &script_events {
        assert_eq!(event["input_index"], 0, "{event}");
    }
    let completed = script_events.last().unwrap();
    assert_eq!(completed["child_status"], 0, "{completed}");
    assert_eq!(completed["truncated"], false, "{completed}");
    let outputs: Vec<_> = script_events
        .iter()
        .filter(|event| event["event"] == "script-output")
        .collect();
    assert!(outputs.iter().any(|event| event["stream"] == "stdout"));
    assert!(outputs.iter().any(|event| event["stream"] == "stderr"));
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn panes_without_a_start_directory_use_the_invocation_directory() {
    // With no start_directory anywhere, panes should start in the
    // invocation directory, matching tmuxp.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let base = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let document_directory = base.path().join("workspaces");
    let invocation_directory = base.path().join("caller");
    std::fs::create_dir_all(&document_directory).unwrap();
    std::fs::create_dir_all(&invocation_directory).unwrap();
    let document_directory = document_directory.canonicalize().unwrap();
    let invocation_directory = invocation_directory.canonicalize().unwrap();
    std::fs::write(
        document_directory.join("workspace.yaml"),
        "session_name: nostartdir\nwindows:\n  - panes:\n      - blank\n",
    )
    .unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &[
            "load",
            "-S",
            socket,
            "-d",
            document_directory.join("workspace.yaml").to_str().unwrap(),
        ],
        &invocation_directory,
    );
    assert!(output.status.success(), "{output:?}");
    let session = guard
        .server()
        .session("nostartdir")
        .await
        .unwrap()
        .expect("session was created");
    let pane = session.panes().await.unwrap().remove(0);
    assert_eq!(
        pane.current_path()
            .map(|path| path.to_string_lossy().into_owned()),
        Some(invocation_directory.to_string_lossy().into_owned())
    );
    guard.shutdown().await.unwrap();
}

#[test]
fn teamocil_import_derives_session_name_from_the_filename() {
    // teamocil's current format has no session name; the document starts
    // at `windows:`, so the name should come from the file.
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("teamv1.yml"),
        "windows:\n  - name: sample-window\n    root: /tmp\n    layout: tiled\n    panes:\n      - cmd: echo one\n      - cmd: [echo two-a, echo two-b]\n        focus: true\n",
    )
    .unwrap();
    let output = at(
        &["import", "teamocil", "teamv1.yml", "--json"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["session_name"], "teamv1");
    assert_eq!(value["windows"][0]["window_name"], "sample-window");
}

#[tokio::test]
async fn global_options_use_tmuxs_global_session_scope() {
    // global_options must apply through the session's global table
    // (`set-option -g`); the server table refuses most option names.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: globaloptions\nglobal_options:\n  history-limit: 4242\nwindows:\n  - panes:\n      - blank\n",
    )
    .unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "workspace.yaml"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    assert!(
        guard
            .server()
            .session("globaloptions")
            .await
            .unwrap()
            .is_some(),
        "session was created"
    );
    // `show-options -t <session>` (no `-g`) answers only that session's
    // own overrides, so the global table is what confirms this landed
    // as `-g` rather than the session or server tables.
    let value = guard
        .server()
        .get_global_option("history-limit")
        .await
        .unwrap();
    assert_eq!(
        value.map(|v| v.to_string_lossy().into_owned()),
        Some("4242".to_owned())
    );
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn freeze_never_carries_the_sessions_environment() {
    // freeze must not carry the session's inherited process environment
    // (SSH_AUTH_SOCK, SSH_AGENT_PID, ...) into the document.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard.session("frozen-env").await.unwrap();
    session
        .set_environment("SSH_AUTH_SOCK", "/tmp/agent.should-not-leak")
        .await
        .unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let socket = guard.socket_path().to_str().unwrap();
    let output = at(
        &["freeze", "-S", socket, "--json", "frozen-env"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value.get("environment").is_none(), "{value}");
    guard.shutdown().await.unwrap();
}

#[test]
fn load_accepts_yes_even_though_it_never_prompts() {
    // load never prompts, but every other port and tmuxp accept it, so a
    // script written against them must not fail rs at argument parsing.
    for flag in ["-y", "--yes"] {
        let output = cli(&["load", flag, "--help"]);
        assert!(output.status.success(), "{flag}: {output:?}");
    }
}

#[tokio::test]
async fn load_accepts_both_options_and_options_after_spellings() {
    // freeze emits window options under options_after, so load must keep
    // accepting both spellings.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: optionspellings\nwindows:\n  - window_name: one\n    options:\n      main-pane-width: 42\n    panes:\n      - blank\n  - window_name: two\n    options_after:\n      main-pane-width: 43\n    panes:\n      - blank\n",
    )
    .unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "workspace.yaml"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let session = guard
        .server()
        .session("optionspellings")
        .await
        .unwrap()
        .expect("session was created");
    let windows = session.windows().await.unwrap();
    for (window, expected) in windows.iter().zip(["42", "43"]) {
        let value = window.get_option("main-pane-width").await.unwrap();
        assert_eq!(
            value.map(|v| v.to_string_lossy().into_owned()),
            Some(expected.to_owned()),
            "window {:?}",
            window.name().to_string_lossy()
        );
    }
    guard.shutdown().await.unwrap();
}

#[tokio::test]
async fn freeze_does_not_carry_the_capturing_terminals_size() {
    // default-size is the size of the terminal the capture happened on
    // (rs's own `load` sets it explicitly, matching tmuxp), not anything
    // the workspace asked for. Writing it into the frozen document would
    // pin every future reload to that size on any server.
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("workspace.yaml"),
        "session_name: sizesource\nwindows:\n  - panes:\n      - blank\n",
    )
    .unwrap();
    let socket = guard.server().socket_path().to_str().unwrap();
    let output = at(
        &["load", "-S", socket, "-d", "workspace.yaml"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");

    let output = at(
        &["freeze", "-S", socket, "--json", "sizesource"],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let frozen: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        frozen
            .get("options")
            .is_none_or(|options| options.get("default-size").is_none()),
        "{frozen}"
    );

    guard.shutdown().await.unwrap();
}

/// A document that cannot be built through leaves nothing: not the session,
/// not the windows that did build, not the bootstrap window. Running it again
/// therefore fails the same way instead of finding its own wreckage and
/// calling it done.
#[tokio::test]
async fn a_mid_build_failure_removes_the_session_and_the_rerun_fails_the_same_way() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("fail.yaml"),
        "session_name: rerun\nwindows:\n  - window_name: one\n    panes: [echo A]\n  - window_name: two\n    options:\n      not-a-real-option: 1\n    panes: [echo B]\n  - window_name: three\n    panes: [echo C]\n",
    )
    .unwrap();
    for run in ["first", "second"] {
        let output = at(
            &[
                "load",
                "-S",
                guard.socket_path().to_str().unwrap(),
                "-d",
                "--json",
                "fail.yaml",
            ],
            directory.path(),
        );
        assert_eq!(output.status.code(), Some(1), "{run}: {output:?}");
        let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        let retained = &diagnostic["retained_state"];
        assert_eq!(retained["status"], "error", "{run}: {retained}");
        assert_eq!(
            retained["errors"][0]["partial_effects"], false,
            "{run}: {retained}"
        );
        assert!(
            diagnostic["message"]
                .as_str()
                .unwrap()
                .contains("the session was removed"),
            "{run}: {diagnostic}"
        );
        assert!(
            !guard.server().has_session("rerun").await.unwrap(),
            "{run}: the failed build left its session behind"
        );
    }
    assert!(guard.server().sessions().await.unwrap().is_empty());
    guard.shutdown().await.unwrap();
}

/// Reusing a session means the workspace is already there. One that is
/// missing a window the document asks for is found, not missing, so this is
/// `session_mismatch` rather than `session_not_found`: nothing was built or
/// changed, so `status` is `error` rather than `partial`, and the message
/// names the first thing absent. The session is left exactly as it was
/// found.
#[tokio::test]
async fn reusing_a_session_without_the_document_s_windows_is_a_mismatch() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let session = guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("half"))
        .await
        .unwrap();
    let mut window = session.active_window().await.unwrap().unwrap();
    window.rename("one").await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("half.yaml"),
        "session_name: half\nwindows:\n  - window_name: one\n    panes: [echo A]\n  - window_name: two\n    panes: [echo B]\n",
    )
    .unwrap();
    let output = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "--json",
            "half.yaml",
        ],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "session_mismatch", "{diagnostic}");
    let message = diagnostic["message"].as_str().unwrap();
    assert!(message.contains("\"two\""), "{diagnostic}");
    assert!(!message.contains("\"one\""), "{diagnostic}");
    assert_eq!(
        diagnostic["retained_state"]["status"], "error",
        "{diagnostic}"
    );
    assert_eq!(session.windows().await.unwrap().len(), 1);
    guard.shutdown().await.unwrap();
}

/// Every machine code a refusal reports is one a consumer can switch on: the
/// ten conditions the envelope defines, plus the interruption record, which
/// is its own contract. A layout name that is not a layout is a defect in the
/// document, caught before any tmux command, and is reported as one.
#[tokio::test]
async fn refusals_report_only_shared_machine_codes() {
    const SHARED: [&str; 11] = [
        "workspace_not_found",
        "invalid_workspace",
        "unsupported_key",
        "session_not_found",
        "session_mismatch",
        "tmux_unavailable",
        "tmux_failed",
        "script_failed",
        "destination_exists",
        "usage",
        "interrupted",
    ];
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let socket = guard.socket_path().to_str().unwrap().to_owned();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    for (name, body) in [
        (
            "namedlayout.yaml",
            "session_name: nl\nwindows:\n  - panes: [echo A]\n  - layout: definitely-not-a-layout\n    panes: [echo B]\n",
        ),
        (
            "badoption.yaml",
            "session_name: bo\nwindows:\n  - options:\n      not-a-real-option: 1\n    panes: [echo A]\n",
        ),
        (
            "ok.yaml",
            "session_name: ok\nwindows:\n  - panes: [echo A]\n",
        ),
        ("broken.yaml", "a: [\n"),
        ("missing.yaml", "session_name: m\n"),
    ] {
        std::fs::write(directory.path().join(name), body).unwrap();
    }
    let cases: [(&str, Vec<&str>, &str); 6] = [
        (
            "layout name",
            vec!["load", "-S", &socket, "-d", "--json", "namedlayout.yaml"],
            "invalid_workspace",
        ),
        (
            "window option",
            vec!["load", "-S", &socket, "-d", "--json", "badoption.yaml"],
            "tmux_failed",
        ),
        (
            "no windows",
            vec!["load", "-S", &socket, "-d", "--json", "missing.yaml"],
            "invalid_workspace",
        ),
        (
            "legacy colors",
            vec!["load", "-S", &socket, "-d", "-8", "--json", "ok.yaml"],
            "usage",
        ),
        (
            "malformed document",
            vec!["convert", "--json", "broken.yaml"],
            "invalid_workspace",
        ),
        (
            "unparsable pattern",
            vec!["search", "--json", "--name", "a("],
            "usage",
        ),
    ];
    for (case, arguments, expected) in cases {
        let output = at(&arguments, directory.path());
        assert!(!output.status.success(), "{case}: {output:?}");
        let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        let code = diagnostic["code"].as_str().unwrap_or_default();
        assert_eq!(code, expected, "{case}: {diagnostic}");
        assert!(SHARED.contains(&code), "{case}: {code} is outside the set");
    }
    guard.shutdown().await.unwrap();
}

/// A window option a workspace lists under the session's `options:` is
/// applied where tmux keeps it, so the document's request takes effect
/// instead of stopping the load.
#[tokio::test]
async fn a_window_option_under_session_options_lands_on_the_windows() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("pbi.yaml"),
        "session_name: pbi\noptions:\n  base-index: 1\n  pane-base-index: 1\nwindows:\n  - window_name: w\n    panes: [echo A, echo B]\n",
    )
    .unwrap();
    let output = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "--json",
            "pbi.yaml",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let session = guard.server().session("pbi").await.unwrap().unwrap();
    let windows = session.windows().await.unwrap();
    assert_eq!(windows.len(), 1);
    let panes = windows[0].panes().await.unwrap();
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].index(), 1, "pane-base-index was not applied");
    assert_eq!(windows[0].index(), 1, "base-index was not applied");
    guard.shutdown().await.unwrap();
}

/// YAML reads a bare `~` as null, which names no path at all, so the pane
/// starts where the load was run — the same as leaving the key out. A
/// directory that is named but is not there still builds, and is said out
/// loud rather than silently becoming $HOME.
#[tokio::test]
async fn a_null_start_directory_is_absent_and_an_absent_one_is_reported() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let elsewhere = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let invocation = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        elsewhere.path().join("nulldir.yaml"),
        "session_name: nulldir\nstart_directory: ~\nwindows:\n  - panes: [blank]\n",
    )
    .unwrap();
    let document = elsewhere.path().join("nulldir.yaml");
    let output = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            document.to_str().unwrap(),
        ],
        invocation.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let session = guard.server().session("nulldir").await.unwrap().unwrap();
    let panes = session.windows().await.unwrap()[0].panes().await.unwrap();
    let started = panes[0].current_path().unwrap().to_string_lossy();
    assert_eq!(
        std::fs::canonicalize(started.as_ref()).unwrap(),
        std::fs::canonicalize(invocation.path()).unwrap(),
        "a null start_directory did not use the invocation directory"
    );

    std::fs::write(
        invocation.path().join("gone.yaml"),
        "session_name: gone\nstart_directory: /nonexistent/definitely/not/here\nwindows:\n  - panes: [blank]\n",
    )
    .unwrap();
    let output = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "gone.yaml",
        ],
        invocation.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("/nonexistent/definitely/not/here"),
        "{stderr}"
    );
    assert!(stderr.contains("$HOME"), "{stderr}");
    guard.shutdown().await.unwrap();
}

/// A pane's command waits for that pane's shell, whatever shell that is.
///
/// Sent before the line editor owns the terminal, the text is echoed by the
/// tty and redrawn by the editor, so the command reads twice. A shell that
/// never reaches a prompt is how the wait itself is observed: it runs, and
/// says so, under a shell that is not zsh.
#[tokio::test]
async fn a_pane_command_waits_for_any_shell_not_only_zsh() {
    use std::os::unix::fs::PermissionsExt;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let shell = directory.path().join("never-prompts");
    std::fs::write(&shell, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    guard
        .server()
        .set_global_option("default-shell", shell.to_str().unwrap())
        .await
        .unwrap();
    std::fs::write(
        directory.path().join("bash.yaml"),
        "session_name: bashed\nwindows:\n  - panes: [echo READY-MARKER]\n",
    )
    .unwrap();
    let output = command_at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "--ndjson",
            "bash.yaml",
        ],
        directory.path(),
    )
    .env("SHELL", "/bin/bash")
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let waited = String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.contains("pane_readiness_timeout"));
    assert!(
        waited,
        "the pane's command was sent without waiting for its shell: {output:?}"
    );
    guard.shutdown().await.unwrap();
}

/// tmux runs a session whose name holds a target separator; a workspace file
/// naming one cannot be loaded, because the name cannot be addressed. Capture
/// refuses it where the session is, rather than writing a file that is
/// rejected the moment it is used.
#[tokio::test]
async fn freeze_refuses_a_session_name_load_would_not_accept() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    guard
        .server()
        .new_session(libtmux::NewSessionOptions::new("my.proj"))
        .await
        .unwrap();
    assert!(guard.server().has_session("my.proj").await.unwrap());
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let destination = directory.path().join("frozen.yaml");
    let output = at(
        &[
            "freeze",
            "my.proj",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "--json",
            "--save-to",
            destination.to_str().unwrap(),
        ],
        directory.path(),
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "invalid_workspace", "{diagnostic}");
    assert!(
        diagnostic["message"].as_str().unwrap().contains("my.proj"),
        "{diagnostic}"
    );
    assert!(!destination.exists(), "the refused capture wrote a file");
    guard.shutdown().await.unwrap();
}

/// A window nothing claims focus in is left on the pane the load finished
/// making, which is where the reference implementation leaves it. A pane that
/// does claim focus still wins.
#[tokio::test]
async fn a_window_with_no_declared_focus_ends_on_its_last_pane() {
    let guard = libtmux::test::TestServer::new().await.unwrap();
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    std::fs::write(
        directory.path().join("focus.yaml"),
        "session_name: focused\nwindows:\n  - window_name: silent\n    panes: [blank, blank, blank]\n  - window_name: claimed\n    panes:\n      - blank\n      - shell_command: blank\n        focus: true\n      - blank\n",
    )
    .unwrap();
    let output = at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-d",
            "focus.yaml",
        ],
        directory.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let session = guard.server().session("focused").await.unwrap().unwrap();
    let windows = session.windows().await.unwrap();
    for (window, expected) in windows.iter().zip([2, 1]) {
        let panes = window.panes().await.unwrap();
        assert_eq!(panes.len(), 3);
        let active = window.active_pane().await.unwrap().unwrap();
        assert_eq!(
            active.id(),
            panes[expected].id(),
            "{}: wrong pane left active",
            window.name().to_string_lossy()
        );
    }
    guard.shutdown().await.unwrap();
}

/// The client the load hands over to is the same tmux the rest of the load
/// talked to, so it is started with the same endpoint flags rather than
/// tmux's defaults.
#[tokio::test]
async fn the_attach_handoff_carries_the_endpoint_flags() {
    use std::os::unix::fs::PermissionsExt as _;

    let guard = libtmux::test::TestServer::new().await.unwrap();
    let keeper = guard.session("handoff-keeper").await.unwrap();
    let pane = current_pane(&keeper).await;
    let control = libtmux::control::ControlMode::attach(guard.server(), keeper.id())
        .await
        .unwrap();
    wait_for_client(guard.server(), keeper.id().as_ref()).await;
    let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
    let trace = directory.path().join("trace");
    let wrapper = directory.path().join("tmux");
    std::fs::write(&wrapper, "#!/bin/sh\nprintf '<%s>' \"$@\" >> \"$WORKSPACE_TMUX_TRACE\"\nprintf '\\n' >> \"$WORKSPACE_TMUX_TRACE\"\nexec \"$WORKSPACE_REAL_TMUX\" \"$@\"\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = directory.path().join("tmux.conf");
    std::fs::write(&config, "").unwrap();
    std::fs::write(
        directory.path().join("workspace.json"),
        serde_json::json!({"session_name":"handed-over","windows":[{"panes":["blank"]}]})
            .to_string(),
    )
    .unwrap();
    let output = command_at(
        &[
            "load",
            "-S",
            guard.socket_path().to_str().unwrap(),
            "-2",
            "-f",
            config.to_str().unwrap(),
            "workspace.json",
        ],
        directory.path(),
    )
    .env("LIBTMUX_TEST_TMUX", &wrapper)
    .env("WORKSPACE_TMUX_TRACE", &trace)
    .env(
        "WORKSPACE_REAL_TMUX",
        std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into()),
    )
    .env("TMUX_PANE", &pane)
    .env(
        "TMUX",
        format!("{},{},0", guard.socket_path().display(), guard.daemon_pid()),
    )
    .output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let calls = std::fs::read_to_string(&trace).unwrap();
    let handoff = calls
        .lines()
        .find(|line| line.contains("<switch-client>"))
        .unwrap_or_else(|| panic!("no handoff recorded: {calls}"));
    assert!(handoff.contains("<-2>"), "{handoff}");
    assert!(handoff.contains("<-f>"), "{handoff}");
    drop(control);
    guard.shutdown().await.unwrap();
}
