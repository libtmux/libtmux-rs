//! Which socket `Server` hands tmux when the caller names none.
//!
//! Each case runs again in a child process that starts from an empty
//! environment and holds only what the case sets, so nothing the developer
//! exported can steer it and no case can reach their tmux. Cases that only
//! read the arguments run a recording stand-in for tmux; the cases that run a
//! real tmux give it a private `TMUX_TMPDIR`.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use libtmux::{Command, Server};

const CHILD: &str = "LIBTMUX_RS_SOCKET_SELECTION_CHILD";
const EXPLICIT: &str = "LIBTMUX_RS_SOCKET_SELECTION_EXPLICIT";
const SOCKET_NAME: &str = "LIBTMUX_SOCKET";
const SOCKET_PATH: &str = "LIBTMUX_SOCKET_PATH";
#[cfg(feature = "control-mode")]
const REAL_TMUX: &str = "LIBTMUX_RS_SOCKET_SELECTION_REAL_TMUX";

/// How the child builds its server before the environment gets a say.
#[derive(Clone, Copy)]
enum Explicit {
    Nothing,
    Name(&'static str),
    Path(&'static str),
    FromEnv(&'static str),
}

impl Explicit {
    fn encode(self) -> String {
        match self {
            Self::Nothing => "nothing".into(),
            Self::Name(name) => format!("name:{name}"),
            Self::Path(path) => format!("path:{path}"),
            Self::FromEnv(value) => format!("from-env:{value}"),
        }
    }
}

/// One tmux invocation the stand-in recorded: its arguments, and whether
/// each of three variables was set. Values are never recorded.
struct Call {
    arguments: Vec<String>,
    tmux_set: bool,
    tmux_pane_set: bool,
}

impl Call {
    fn has(&self, flag: &str) -> bool {
        self.arguments.iter().any(|argument| argument == flag)
    }

    fn after(&self, flag: &str) -> Option<&str> {
        let at = self
            .arguments
            .iter()
            .position(|argument| argument == flag)?;
        self.arguments.get(at + 1).map(String::as_str)
    }
}

fn fixture_root() -> PathBuf {
    let root = PathBuf::from("/tmp/libtmux-rs-test");
    std::fs::create_dir_all(&root).expect("fixture root exists");
    root
}

/// The fake `tmux`, first on the child's `PATH`: it records its arguments and
/// three booleans, answers both version probes as 3.7b, and succeeds.
fn write_recorder(directory: &Path) {
    let bin = directory.join("bin");
    std::fs::create_dir(&bin).expect("recorder directory");
    let executable = bin.join("tmux");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\n\
             if [ \"${{1-}}\" = -V ]; then printf 'tmux 3.7b\\n'; exit 0; fi\n\
             {{\n\
               for argument in \"$@\"; do printf '<%s>' \"$argument\"; done\n\
               printf '|%s|%s\\n' \"${{TMUX:+set}}\" \"${{TMUX_PANE:+set}}\"\n\
             }} >> '{}'\n\
             for argument in \"$@\"; do\n\
               case \"$argument\" in\n\
               libtmux-version-*)\n\
                 printf 'unknown command: %s3.7b\\n' \"${{argument%%#*}}\" >&2\n\
                 exit 1 ;;\n\
               esac\n\
             done\n\
             exit 0\n",
            directory.join("record").display(),
        ),
    )
    .expect("recorder is written");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("recorder is executable");
}

fn calls(directory: &Path) -> Vec<Call> {
    std::fs::read_to_string(directory.join("record"))
        .expect("the stand-in recorded a call")
        .lines()
        .map(|line| {
            let mut fields = line.rsplitn(3, '|');
            let tmux_pane = fields.next().expect("record line shape");
            let tmux = fields.next().expect("record line shape");
            let arguments = fields.next().expect("record line shape");
            Call {
                arguments: arguments
                    .split('>')
                    .filter_map(|part| part.strip_prefix('<'))
                    .map(str::to_owned)
                    .collect(),
                tmux_set: tmux == "set",
                tmux_pane_set: tmux_pane == "set",
            }
        })
        .collect()
}

/// Build the server the case describes and run one command through it.
fn child_run(directory: &Path) {
    let explicit = std::env::var(EXPLICIT).expect("the case names how to build");
    write_recorder(directory);
    let server = match explicit.split_once(':').unwrap_or((explicit.as_str(), "")) {
        ("name", name) => Server::builder().socket_name(name).build(),
        ("path", path) => Server::builder().socket_path(path).build(),
        ("from-env", value) => Server::from_env_value(Some(value)),
        _ => Server::builder().build(),
    }
    .expect("the server builds");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    runtime.block_on(async {
        server
            .cmd(Command::new("display-message").arg("-p").arg("x"))
            .await
            .expect("the stand-in answers");
    });
    if let Some(expected) = std::env::var_os("LIBTMUX_RS_SOCKET_SELECTION_IDENTITY") {
        assert_eq!(server.socket_path(), Path::new(&expected));
    }
}

/// Run `case` again with only `environment` set, and return what the stand-in
/// recorded.
fn record(case: &str, explicit: Explicit, environment: &[(&str, &str)]) -> Vec<Call> {
    let directory = tempfile::tempdir_in(fixture_root()).expect("case directory");
    let root = directory.path().join("root");
    std::fs::create_dir(&root).expect("private socket root");
    let output = ProcessCommand::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", case, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env(CHILD, directory.path())
        .env(EXPLICIT, explicit.encode())
        .env("PATH", directory.path().join("bin"))
        .env("TMUX_TMPDIR", &root)
        .envs(environment.iter().copied())
        .output()
        .expect("child starts");
    assert!(
        output.status.success(),
        "{case} failed in its child: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    calls(directory.path())
}

fn child_directory() -> Option<PathBuf> {
    std::env::var_os(CHILD).map(PathBuf::from)
}

#[test]
fn no_socket_chosen_passes_tmux_neither_l_nor_s() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let calls = record(
        "no_socket_chosen_passes_tmux_neither_l_nor_s",
        Explicit::Nothing,
        &[],
    );

    assert!(!calls.is_empty());
    for call in &calls {
        assert!(!call.has("-L"), "{:?}", call.arguments);
        assert!(!call.has("-S"), "{:?}", call.arguments);
    }
}

#[test]
fn inside_tmux_with_no_socket_chosen_tmux_gets_its_own_server_to_pick() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let calls = record(
        "inside_tmux_with_no_socket_chosen_tmux_gets_its_own_server_to_pick",
        Explicit::Nothing,
        &[
            ("TMUX", "/tmp/libtmux-rs-test/inside.sock,10,4"),
            ("TMUX_PANE", "%4"),
            (
                "LIBTMUX_RS_SOCKET_SELECTION_IDENTITY",
                "/tmp/libtmux-rs-test/inside.sock",
            ),
        ],
    );

    assert!(!calls.is_empty());
    for call in &calls {
        assert!(!call.has("-L") && !call.has("-S"), "{:?}", call.arguments);
        assert!(call.tmux_set && call.tmux_pane_set);
    }
}

#[test]
fn the_socket_name_variable_passes_dash_l_and_not_the_inherited_server() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let calls = record(
        "the_socket_name_variable_passes_dash_l_and_not_the_inherited_server",
        Explicit::Nothing,
        &[
            (SOCKET_NAME, "chosen-by-env"),
            ("TMUX", "/tmp/libtmux-rs-test/inside.sock,10,4"),
            ("TMUX_PANE", "%4"),
        ],
    );

    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(call.after("-L"), Some("chosen-by-env"));
        assert!(!call.has("-S"));
        assert!(!call.tmux_set && !call.tmux_pane_set);
    }
}

#[test]
fn the_socket_path_variable_passes_dash_s_and_not_the_inherited_server() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let calls = record(
        "the_socket_path_variable_passes_dash_s_and_not_the_inherited_server",
        Explicit::Nothing,
        &[
            (SOCKET_PATH, "/tmp/libtmux-rs-test/chosen-by-env.sock"),
            ("TMUX", "/tmp/libtmux-rs-test/inside.sock,10,4"),
        ],
    );

    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(
            call.after("-S"),
            Some("/tmp/libtmux-rs-test/chosen-by-env.sock")
        );
        assert!(!call.has("-L"));
        assert!(!call.tmux_set);
    }
}

#[test]
fn the_socket_path_variable_wins_when_both_are_set() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let calls = record(
        "the_socket_path_variable_wins_when_both_are_set",
        Explicit::Nothing,
        &[
            (SOCKET_PATH, "/tmp/libtmux-rs-test/chosen-by-env.sock"),
            (SOCKET_NAME, "name-loses"),
        ],
    );

    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(
            call.after("-S"),
            Some("/tmp/libtmux-rs-test/chosen-by-env.sock")
        );
        assert!(!call.has("-L"), "{:?}", call.arguments);
    }
}

#[test]
fn a_blank_variable_does_not_hide_the_other() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let calls = record(
        "a_blank_variable_does_not_hide_the_other",
        Explicit::Nothing,
        &[(SOCKET_PATH, ""), (SOCKET_NAME, "chosen-by-env")],
    );

    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(call.after("-L"), Some("chosen-by-env"));
        assert!(!call.has("-S"));
    }
}

#[test]
fn a_socket_named_to_the_builder_leaves_both_variables_unread() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let environment = [
        (SOCKET_PATH, "/tmp/libtmux-rs-test/chosen-by-env.sock"),
        (SOCKET_NAME, "chosen-by-env"),
    ];
    let named = record(
        "a_socket_named_to_the_builder_leaves_both_variables_unread",
        Explicit::Name("explicit"),
        &environment,
    );
    let pathed = record(
        "a_socket_named_to_the_builder_leaves_both_variables_unread",
        Explicit::Path("/tmp/libtmux-rs-test/explicit.sock"),
        &environment,
    );

    for call in &named {
        assert_eq!(call.after("-L"), Some("explicit"));
        assert!(!call.has("-S"));
    }
    for call in &pathed {
        assert_eq!(call.after("-S"), Some("/tmp/libtmux-rs-test/explicit.sock"));
        assert!(!call.has("-L"));
    }
}

#[test]
fn from_env_is_how_a_caller_asks_for_the_tmux_socket() {
    if let Some(directory) = child_directory() {
        return child_run(&directory);
    }
    let calls = record(
        "from_env_is_how_a_caller_asks_for_the_tmux_socket",
        Explicit::FromEnv("/tmp/libtmux-rs-test/asked.sock,10,4"),
        &[(SOCKET_NAME, "ignored-by-from-env")],
    );

    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(call.after("-S"), Some("/tmp/libtmux-rs-test/asked.sock"));
        assert!(!call.has("-L"));
    }
}

/// The directory a real-tmux case works in, and the bare client's way in.
struct Private {
    directory: tempfile::TempDir,
}

impl Private {
    fn new() -> Self {
        Self {
            directory: tempfile::tempdir_in(fixture_root()).expect("case directory"),
        }
    }

    fn root(&self) -> PathBuf {
        self.directory.path().join("root")
    }

    fn tmux() -> OsString {
        std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into())
    }

    /// A tmux client that reads nothing from the developer's environment.
    fn bare(&self, arguments: &[&str]) -> std::process::Output {
        let mut command = ProcessCommand::new(Self::tmux());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMUX_TMPDIR", self.root())
            .args(["-f", "/dev/null"])
            .args(arguments);
        command.output().expect("bare tmux runs")
    }
}

impl Drop for Private {
    fn drop(&mut self) {
        // Every client below starts from an empty environment, so only the
        // root says which server it means. A case that never made it must
        // not leave a client to guess.
        if !self.root().is_dir() {
            return;
        }
        let _ = self.bare(&["kill-server"]);
        let _ = ProcessCommand::new(Self::tmux())
            .env_clear()
            .args(["-f", "/dev/null", "-S"])
            .arg(self.directory.path().join("named.sock"))
            .arg("kill-server")
            .output();
        let _ = ProcessCommand::new(Self::tmux())
            .env_clear()
            .env("TMUX_TMPDIR", self.root())
            .args(["-f", "/dev/null", "-L", "chosen-by-env", "kill-server"])
            .output();
    }
}

/// Create a session through a server built with the case's variables, from a
/// child whose only tmux context is `TMUX_TMPDIR`.
fn child_creates_session() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    runtime.block_on(async {
        let server = Server::builder()
            .tmux_executable(Private::tmux())
            .config_file("/dev/null")
            .build()
            .expect("the server builds");
        let created = server
            .cmd(Command::new("new-session").arg("-d").arg("-s").arg("probe"))
            .await
            .expect("tmux runs");
        assert!(created.success(), "{created:?}");
    });
}

fn real_case(case: &str, environment: &[(&str, &OsString)]) -> Private {
    let private = Private::new();
    std::fs::create_dir(private.root()).expect("private socket root");
    let mut command = ProcessCommand::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", case, "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env(CHILD, private.directory.path())
        .env("TMUX_TMPDIR", private.root());
    for (name, value) in environment {
        command.env(name, value);
    }
    if let Some(tmux) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        command.env("LIBTMUX_TEST_TMUX", tmux);
    }
    let output = command.output().expect("child starts");
    assert!(
        output.status.success(),
        "{case} failed in its child: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    private
}

fn sessions(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn with_nothing_chosen_a_bare_tmux_finds_the_server_the_library_started() {
    if child_directory().is_some() {
        return child_creates_session();
    }
    let private = real_case(
        "with_nothing_chosen_a_bare_tmux_finds_the_server_the_library_started",
        &[],
    );

    let listed = private.bare(&["list-sessions", "-F", "#{session_name}"]);
    assert_eq!(sessions(&listed), "probe", "{listed:?}");
}

#[test]
fn the_socket_name_variable_starts_the_server_a_bare_dash_l_finds() {
    if child_directory().is_some() {
        return child_creates_session();
    }
    let private = real_case(
        "the_socket_name_variable_starts_the_server_a_bare_dash_l_finds",
        &[(SOCKET_NAME, &OsString::from("chosen-by-env"))],
    );

    let named = private.bare(&[
        "-L",
        "chosen-by-env",
        "list-sessions",
        "-F",
        "#{session_name}",
    ]);
    let default = private.bare(&["list-sessions", "-F", "#{session_name}"]);
    assert_eq!(sessions(&named), "probe", "{named:?}");
    assert!(
        !default.status.success(),
        "the default socket must stay empty: {default:?}"
    );
}

#[test]
fn the_socket_path_variable_starts_the_server_a_bare_dash_s_finds() {
    if child_directory().is_some() {
        return child_creates_session();
    }
    let private = Private::new();
    std::fs::create_dir(private.root()).expect("private socket root");
    let socket = private.directory.path().join("named.sock");
    let output = ProcessCommand::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "the_socket_path_variable_starts_the_server_a_bare_dash_s_finds",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env(CHILD, private.directory.path())
        .env("TMUX_TMPDIR", private.root())
        .env(SOCKET_PATH, &socket)
        .output()
        .expect("child starts");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let found = ProcessCommand::new(Private::tmux())
        .env_clear()
        .args(["-f", "/dev/null", "-S"])
        .arg(&socket)
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .expect("bare tmux runs");
    let default = private.bare(&["list-sessions", "-F", "#{session_name}"]);
    assert_eq!(sessions(&found), "probe", "{found:?}");
    assert!(
        !default.status.success(),
        "the default socket must stay empty: {default:?}"
    );
}

/// The tmux the cases run, as an absolute path a script can `exec`.
#[cfg(feature = "control-mode")]
fn real_tmux() -> PathBuf {
    let tmux = Private::tmux();
    let path = PathBuf::from(&tmux);
    if path.is_absolute() {
        return path;
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(&tmux))
        .find(|candidate| candidate.is_file())
        .expect("tmux is on PATH")
}

/// A `tmux` first on the child's `PATH` that records its arguments, then runs
/// the real one.
#[cfg(feature = "control-mode")]
fn write_forwarder(directory: &Path, real: &Path) {
    let bin = directory.join("bin");
    std::fs::create_dir(&bin).expect("forwarder directory");
    let executable = bin.join("tmux");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\n\
             {{\n\
               for argument in \"$@\"; do printf '<%s>' \"$argument\"; done\n\
               printf '|%s|%s\\n' \"${{TMUX:+set}}\" \"${{TMUX_PANE:+set}}\"\n\
             }} >> '{}'\n\
             exec '{}' \"$@\"\n",
            directory.join("record").display(),
            real.display(),
        ),
    )
    .expect("forwarder is written");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("forwarder is executable");
}

/// Attach a control client through a server whose socket nobody chose.
#[cfg(feature = "control-mode")]
fn child_attaches_control(directory: &Path) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    runtime.block_on(async {
        write_forwarder(
            directory,
            Path::new(&std::env::var_os(REAL_TMUX).expect("the case names tmux")),
        );
        let server = Server::builder()
            .config_file("/dev/null")
            .build()
            .expect("the server builds");
        let session = server.new_session("probe").await.expect("tmux runs");
        let version = server
            .capabilities()
            .await
            .expect("version")
            .tmux_version()
            .clone();
        if !version.has_behavior(&libtmux::since::CONTROL_ACQUISITION) {
            std::fs::write(directory.join("skipped"), "").expect("skip marker");
            return;
        }
        let (_, events) = libtmux::control::ControlMode::attach(&server, session.id())
            .await
            .expect("the control client attaches")
            .split();
        events.shutdown().await.expect("the control client stops");
    });
}

#[cfg(feature = "control-mode")]
#[test]
fn a_control_client_is_pinned_when_no_socket_was_chosen() {
    if let Some(directory) = child_directory() {
        return child_attaches_control(&directory);
    }
    let private = Private::new();
    std::fs::create_dir(private.root()).expect("private socket root");
    let output = ProcessCommand::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "a_control_client_is_pinned_when_no_socket_was_chosen",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", private.directory.path().join("bin"))
        .env(CHILD, private.directory.path())
        .env(REAL_TMUX, real_tmux())
        .env("TMUX_TMPDIR", private.root())
        .output()
        .expect("child starts");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if private.directory.path().join("skipped").exists() {
        eprintln!("control acquisition needs a newer tmux; case skipped");
        return;
    }

    // Acquisition probes the daemon twice with `run-shell -C`: once as left,
    // and once through the pin, which is what shows the pin reaches the same
    // daemon. The client that follows attaches through the pin as well.
    let all = calls(private.directory.path());
    let pinned = |call: &Call| {
        let pin = call.after("-S").map(Path::new);
        !call.has("-L")
            && pin.is_some_and(|pin| {
                pin.file_name().is_some_and(|name| name == "s")
                    && pin
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| name.to_string_lossy().starts_with(".lt-"))
            })
    };
    let attach_at = all
        .iter()
        .position(|call| call.has("attach-session") || call.has("attach"))
        .expect("no control client was started");
    let probes: Vec<&Call> = all[..attach_at]
        .iter()
        .filter(|call| call.has("run-shell"))
        .collect();
    let [.., as_left, through_pin] = probes.as_slice() else {
        panic!("acquisition made fewer than two probes");
    };
    assert!(
        !as_left.has("-S"),
        "the first probe reads the daemon tmux picks: {:?}",
        as_left.arguments
    );
    assert!(
        pinned(through_pin),
        "the second probe was not given the pinned socket: {:?}",
        through_pin.arguments
    );
    let attaches: Vec<&Call> = all
        .iter()
        .filter(|call| call.has("attach-session") || call.has("attach"))
        .collect();
    assert!(!attaches.is_empty(), "no control client was started");
    for call in attaches {
        assert!(
            pinned(call),
            "the control client was not given the pinned socket: {:?}",
            call.arguments
        );
    }
}
