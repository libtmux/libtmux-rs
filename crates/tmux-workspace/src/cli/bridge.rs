use std::ffi::OsStr;
use std::path::Path;

use serde_json::{Value, json};

use super::{Result, output::Reporter, process};

/// The plugin bridge, as a real, syntax-highlighted file rather than a Rust
/// string literal. `bridge_source_parses_as_python` in this module's tests
/// syntax-checks it with a bare `python3`, which needs no tmuxp install.
const BUILD: &str = include_str!("bridge.py");

pub(super) async fn build(
    python: &OsStr,
    request: Value,
    report: &mut Reporter,
) -> Result<process::ChildOutput> {
    report.event("warning", json!({"code":"python_extension_bridge","message":"Python extension hooks execute through tmuxp 1.74.x; child output is captured separately from native build events"}))?;
    let argv = [python.to_owned(), "-c".into(), BUILD.into()];
    // The request carries a socket path and session name; an argv value
    // lands in /proc/<pid>/cmdline, which is world-readable on Linux, while
    // an environment variable lands in /proc/<pid>/environ, which is not.
    let request = request.to_string();
    process::run(
        &argv,
        Path::new(&std::env::current_dir()?),
        report,
        None,
        &[("TMUX_WORKSPACE_BRIDGE_REQUEST", &request)],
    )
    .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Catches a typo in the embedded bridge before it reaches `python -c`,
    /// where the whole program's syntax was previously checked only at the
    /// moment tmuxp actually ran it. `ast.parse` needs a bare interpreter,
    /// not tmuxp, so this runs unconditionally rather than behind
    /// `TMUX_WORKSPACE_PYTHON`.
    #[test]
    fn bridge_source_parses_as_python() {
        let output = std::process::Command::new("python3")
            .args(["-c", "import ast, sys; ast.parse(sys.stdin.read())"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child
                    .stdin
                    .take()
                    .expect("stdin was piped")
                    .write_all(BUILD.as_bytes())?;
                child.wait_with_output()
            })
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    #[ignore = "requires TMUX_WORKSPACE_PYTHON with tmuxp 1.74.x"]
    async fn append_bridge_rechecks_borrowed_identity_before_imports_and_build() {
        let original = libtmux::test::TestServer::new().await.unwrap();
        let replacement = libtmux::test::TestServer::new().await.unwrap();
        let first = original.session("original").await.unwrap();
        let second = replacement.session("replacement").await.unwrap();
        assert_eq!(first.id(), second.id());
        let identity = original
            .server()
            .cmd(
                libtmux::Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(first.id().to_string())
                    .arg("#{pid}:#{start_time}"),
            )
            .await
            .unwrap()
            .stdout_lossy()
            .trim()
            .to_owned();
        let directory = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
        let alias = directory.path().join("socket,alias");
        let config = directory.path().join("workspace.json");
        let imported = directory.path().join("imported");
        let built = directory.path().join("built");
        std::fs::write(
            directory.path().join("extension.py"),
            r"
import os
from pathlib import Path
from tmuxp.workspace.builder.classic import ClassicWorkspaceBuilder
Path('imported').touch()
class Builder(ClassicWorkspaceBuilder):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        if os.environ.get('REPLACEMENT_SOCKET'):
            Path('socket,alias').unlink()
            Path('socket,alias').symlink_to(os.environ['REPLACEMENT_SOCKET'])
    def build(self, *args, **kwargs):
        Path('built').touch()
        super().build(*args, **kwargs)
",
        )
        .unwrap();
        std::fs::write(&config, json!({"session_name":"unwanted","workspace_builder":"extension:Builder","workspace_builder_paths":[directory.path()],"windows":[{"window_name":"added","panes":["blank"]}]}).to_string()).unwrap();
        let python = std::env::var_os("TMUX_WORKSPACE_PYTHON").unwrap();
        let mut failures = Vec::new();
        for case in ["missing", "mismatch", "constructor"] {
            let _ = std::fs::remove_file(&alias);
            std::os::unix::fs::symlink(original.socket_path(), &alias).unwrap();
            let _ = std::fs::remove_file(&imported);
            let _ = std::fs::remove_file(&built);
            let request = json!({"path":config,"session_name":"unwanted","socket":alias,
                "append":if case == "missing" {"$999".to_owned()} else {first.id().to_string()},
                "append_identity":if case == "mismatch" {"0:0"} else {&identity}});
            let mut command = std::process::Command::new(&python);
            command
                .args(["-c", BUILD])
                .env("TMUX_WORKSPACE_BRIDGE_REQUEST", request.to_string())
                .current_dir(directory.path())
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .env_remove("REPLACEMENT_SOCKET");
            if case == "constructor" {
                command.env("REPLACEMENT_SOCKET", replacement.socket_path());
            }
            let output = command.output().unwrap();
            if output.status.success()
                || built.exists()
                || imported.exists() != (case == "constructor")
                || !String::from_utf8_lossy(&output.stderr).contains("append_context")
            {
                failures.push(format!(
                    "{case}: {output:?}; imported={} built={}",
                    imported.exists(),
                    built.exists()
                ));
            }
        }
        for (guard, session) in [(&original, &first), (&replacement, &second)] {
            if session.windows().await.unwrap().len() != 1
                || guard.server().sessions().await.unwrap().len() != 1
            {
                failures.push(format!(
                    "{} topology changed",
                    session.name().to_string_lossy()
                ));
            }
        }
        original.shutdown().await.unwrap();
        replacement.shutdown().await.unwrap();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
