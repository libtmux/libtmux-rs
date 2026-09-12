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
paths = resolve_builder_paths(config, path)
with prepended_sys_path(paths):
    builder = resolve_builder_class(config)(session_config=config, server=server, plugins=load_plugins(config, colors=Colors(ColorMode.NEVER)))
    borrowed = server.sessions.get(session_id=request['append']) if request.get('append') else None
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
    process::run(&argv, Path::new(&std::env::current_dir()?), report).await
}
