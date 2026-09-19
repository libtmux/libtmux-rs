import json, os, sys
from pathlib import Path
from libtmux import Server
from tmuxp._internal import config_reader
from tmuxp.cli.load import load_plugins
from tmuxp.cli._colors import Colors, ColorMode
from tmuxp.workspace import loader
from tmuxp.workspace.builder import prepended_sys_path, resolve_builder_class, resolve_builder_paths

# The request travels through TMUX_WORKSPACE_BRIDGE_REQUEST rather than argv:
# /proc/<pid>/cmdline is world-readable on Linux and this carries a socket
# path and session name, while /proc/<pid>/environ is not.
request = json.loads(os.environ['TMUX_WORKSPACE_BRIDGE_REQUEST'])
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
