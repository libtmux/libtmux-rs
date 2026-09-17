use std::ffi::OsStr;
use std::path::Path;

use serde_json::{Value, json};

use super::{Result, output::Reporter, process};

const BUILD: &str = r"
import json, os, sys
from pathlib import Path
from libtmux import Server
from tmuxp._internal import config_reader
from tmuxp.cli.load import load_plugins
from tmuxp.cli._colors import Colors, ColorMode
from tmuxp.workspace import loader
from tmuxp.workspace.builder import prepended_sys_path, resolve_builder_class, resolve_builder_paths

request = json.loads(sys.argv[1])
path = Path(request['path'])
config = loader.trickle(loader.expand(config_reader.ConfigReader._from_file(path), cwd=os.path.dirname(path)))
config['session_name'] = request['session_name']
server = Server(socket_path=request['socket'], config_file=request.get('config_file'), colors=request.get('colors'))
def append_target():
    if request.get('append') is None:
        return None
    borrowed = server.sessions.get(session_id=request['append'], default=None)
    if borrowed is None:
        raise RuntimeError('append_context: borrowed session no longer exists')
    identity = server.cmd('display-message', '-p', '-t', borrowed.session_id, '#{pid}:#{start_time}')
    if identity.returncode != 0 or identity.stdout != [request.get('append_identity')]:
        raise RuntimeError('append_context: borrowed daemon identity changed')
    return borrowed

append_target()
paths = resolve_builder_paths(config, path)
with prepended_sys_path(paths):
    builder = resolve_builder_class(config)(session_config=config, server=server, plugins=load_plugins(config, colors=Colors(ColorMode.NEVER)))
    borrowed = append_target()
    if borrowed is not None:
        # Classic before_script failure kills its session. Append borrows it.
        borrowed.kill = lambda *args, **kwargs: None
    if borrowed is not None or not builder.session_exists(config['session_name']):
        builder.build(borrowed, append=borrowed is not None)
        for plugin in builder.plugins:
            plugin.before_script(builder.session)
";

pub(super) async fn build(
    python: &OsStr,
    request: Value,
    report: &mut Reporter,
) -> Result<process::ChildOutput> {
    report.event("warning", json!({"code":"python_extension_bridge","message":"Python extension hooks execute through tmuxp 1.74.0; child output is captured separately from native build events"}))?;
    let argv = [
        python.to_owned(),
        "-c".into(),
        BUILD.into(),
        request.to_string().into(),
    ];
    process::run(&argv, Path::new(&std::env::current_dir()?), report, None).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires TMUX_WORKSPACE_PYTHON with tmuxp 1.74.0"]
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
                .args(["-c", BUILD, &request.to_string()])
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
