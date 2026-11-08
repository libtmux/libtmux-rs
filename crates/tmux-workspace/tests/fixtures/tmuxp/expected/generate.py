#!/usr/bin/env -S uv run --no-project --python 3.13 --with tmuxp==1.74.0
"""Write the structure tmuxp 1.74.0 builds from each example in ``..``.

For every ``../*.yaml`` this runs tmuxp's own loading and normalization
(``ConfigReader._from_file``, ``loader.expand`` and ``loader.trickle``, the
three steps ``tmuxp load`` takes before it builds anything), keeps what a
builder has to honour, and writes ``<name>.json`` beside this script. It never
starts tmux.

    uv run --no-project --python 3.13 --with tmuxp==1.74.0 generate.py
    uv run --no-project --python 3.13 --with tmuxp==1.74.0 generate.py --check

``--check`` writes nothing and exits 1 when a file differs from what tmuxp
produces now, or when an expected file has no example or the reverse.

The output does not depend on the machine. The environment is emptied and
refilled with the placeholders below before tmuxp runs, so ``~`` and ``$VAR``
expand to a placeholder rather than to this host's values, and the directory
tmuxp resolves ``./`` against is ``/@WORKSPACE_DIR@``. See ``README.md``.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sys

PINNED = "1.74.0"
FEATURES_FILE = "syntax-features.json"
SCHEMA_VERSION = 1

# Every variable the catalog reads, set to its own name between at-signs.
# Anything else stays literal, because tmuxp leaves a variable it cannot
# resolve alone. The ones that hold paths get a leading slash: tmuxp treats a
# start_directory that begins with `~` or `/` as absolute and joins any other
# onto its parent, so an unrooted placeholder would change the result.
PATH_VARIABLES = ("HOME", "MY_ENV_VAR", "PWD")
PLAIN_VARIABLES = ("MAIN_PANE_HEIGHT", "SERVER_PORT", "USER")
WORKSPACE_DIR = "/@WORKSPACE_DIR@"

HERE = pathlib.Path(__file__).resolve().parent
CATALOG = HERE.parent


def load(path: pathlib.Path) -> dict:
    """Return what ``tmuxp load`` hands its builder for this file."""
    from tmuxp._internal import config_reader
    from tmuxp.workspace import loader

    raw = config_reader.ConfigReader._from_file(path) or {}
    expanded = loader.expand(raw, cwd=WORKSPACE_DIR)
    return loader.trickle(expanded)


def commands(pane: dict, window: dict) -> list[dict]:
    """Return a pane's commands with the flags the builder gives each.

    ``ClassicWorkspaceBuilder.iter_create_panes`` starts from the pane's own
    ``enter``, ``sleep_before`` and ``sleep_after`` and lets each command
    override them for itself and every command after it. The sleeps are
    runtime, so they sit beside the command rather than in it.
    """
    enter = pane.get("enter", True)
    sleep_before = pane.get("sleep_before")
    sleep_after = pane.get("sleep_after")
    out = []
    for command in pane["shell_command"]:
        enter = command.get("enter", enter)
        sleep_before = command.get("sleep_before", sleep_before)
        sleep_after = command.get("sleep_after", sleep_after)
        out.append(
            {
                "cmd": command["cmd"],
                "enter": enter,
                "runtime": {
                    "sleep_before": sleep_before,
                    "sleep_after": sleep_after,
                },
            }
        )
    return out


def suppress_history(pane: dict, window: dict) -> bool:
    """Return the builder's choice: pane, then window (session trickled), else true."""
    if "suppress_history" in pane:
        return pane["suppress_history"]
    if "suppress_history" in window:
        return window["suppress_history"]
    return True


def structure(name: str, config: dict) -> dict:
    windows = []
    for window in config["windows"]:
        panes = []
        for pane in window["panes"]:
            panes.append(
                {
                    "shell": pane.get("shell", window.get("window_shell")),
                    "start_directory": pane.get("start_directory", window.get("start_directory")),
                    "environment": pane.get("environment", window.get("environment")),
                    "focus": bool(pane.get("focus", False)),
                    "commands": commands(pane, window),
                    "runtime": {
                        "suppress_history": suppress_history(pane, window),
                    },
                }
            )
        windows.append(
            {
                "window_name": window.get("window_name"),
                "window_index": window.get("window_index"),
                "layout": window.get("layout"),
                "options": window.get("options"),
                "options_after": window.get("options_after"),
                "start_directory": window.get("start_directory"),
                "environment": window.get("environment"),
                "shell": window.get("window_shell"),
                "focus": bool(window.get("focus", False)),
                "panes": panes,
            }
        )
    return {
        "schema_version": SCHEMA_VERSION,
        "source": name,
        "tmuxp": PINNED,
        "session": {
            "session_name": config.get("session_name"),
            "start_directory": config.get("start_directory"),
            "environment": config.get("environment"),
            "options": config.get("options"),
            "global_options": config.get("global_options"),
        },
        "windows": windows,
        "runtime": {
            "before_script": config.get("before_script"),
            "plugins": config.get("plugins"),
            "suppress_history": config.get("suppress_history"),
        },
    }


def _walk(raw: dict):
    """Yield (level, mapping) for the session, each window and each pane mapping."""
    yield "session", raw
    for window in raw.get("windows") or []:
        yield "window", window
        for pane in window.get("panes") or []:
            if isinstance(pane, dict):
                yield "pane", pane


def _has(raw: dict, key: str, *levels: str) -> bool:
    return any(key in node and (not levels or level in levels) for level, node in _walk(raw))


def _anywhere(value, key: str) -> bool:
    if isinstance(value, dict):
        return key in value or any(_anywhere(v, key) for v in value.values())
    if isinstance(value, list):
        return any(_anywhere(v, key) for v in value)
    return False


def _pane_values(raw: dict):
    for window in raw.get("windows") or []:
        yield from window.get("panes") or []


def _commands(raw: dict):
    for pane in _pane_values(raw):
        if isinstance(pane, dict):
            value = pane.get("shell_command")
            yield from value if isinstance(value, list) else [value]


def _starts(raw: dict, level: str, prefix: str) -> bool:
    return any(
        isinstance(node.get("start_directory"), str) and node["start_directory"].startswith(prefix)
        for lvl, node in _walk(raw)
        if lvl == level
    )


def _window_relative(raw: dict) -> bool:
    session = raw.get("start_directory")
    return bool(session) and any(
        isinstance(w.get("start_directory"), str) and w["start_directory"][:1] not in ("", "~", "/", ".")
        for w in raw.get("windows") or []
    )


def _dot(raw: dict) -> bool:
    return any(
        isinstance(node.get("start_directory"), str) and node["start_directory"].startswith(".")
        for _, node in _walk(raw)
    )


def _blank(value) -> bool:
    return value is None or value in ("blank", "pane")


# (id, title, tmuxp syntax, predicate on the file as written). A file exercises a
# feature when the predicate holds for what it says before expansion.
FEATURES = (
    ("session.shell_command_before", "shell_command_before on the session", "shell_command_before at the top of the file", lambda r: "shell_command_before" in r),
    ("window.shell_command_before", "shell_command_before on a window", "shell_command_before in a window", lambda r: _has(r, "shell_command_before", "window")),
    ("pane.shell_command_before", "shell_command_before on a pane", "shell_command_before in a pane", lambda r: _has(r, "shell_command_before", "pane")),
    ("session.before_script", "before_script", "before_script at the top of the file", lambda r: "before_script" in r),
    ("sleep_before", "sleep_before", "sleep_before on a pane or a command, including those in shell_command_before", lambda r: _anywhere(r, "sleep_before")),
    ("sleep_after", "sleep_after", "sleep_after on a pane or a command, including those in shell_command_before", lambda r: _anywhere(r, "sleep_after")),
    ("session.plugins", "plugins", "plugins at the top of the file", lambda r: "plugins" in r),
    ("window.window_shell", "window_shell", "window_shell in a window", lambda r: _has(r, "window_shell")),
    ("pane.shell", "pane shell", "shell in a pane", lambda r: _has(r, "shell", "pane")),
    ("suppress_history", "suppress_history", "suppress_history at any level", lambda r: _has(r, "suppress_history")),
    ("window.focus", "window focus", "focus: true on a window", lambda r: _has(r, "focus", "window")),
    ("pane.focus", "pane focus", "focus: true on a pane", lambda r: _has(r, "focus", "pane")),
    ("session.options", "session options", "options at the top of the file", lambda r: "options" in r),
    ("session.global_options", "global options", "global_options at the top of the file", lambda r: "global_options" in r),
    ("window.options", "window options", "options in a window", lambda r: _has(r, "options", "window")),
    ("window.options_after", "window options_after", "options_after in a window", lambda r: _has(r, "options_after")),
    ("pane.string", "a pane written as a string", "- command", lambda r: any(isinstance(p, str) and not _blank(p) for p in _pane_values(r))),
    ("pane.list", "a pane written as a list", "- [command, command]", lambda r: any(isinstance(p, list) for p in _pane_values(r))),
    ("pane.shell_command_string", "shell_command as a string", "shell_command: command", lambda r: any(isinstance(p, dict) and isinstance(p.get("shell_command"), str) for p in _pane_values(r))),
    ("pane.command_mapping", "a command written as a mapping", "- cmd: command", lambda r: any(isinstance(c, dict) for c in _commands(r))),
    ("pane.blank", "a blank pane", "a null pane, `blank` or `pane`", lambda r: any(_blank(p) or (isinstance(p, dict) and "shell_command" in p and _blank(p["shell_command"])) for p in _pane_values(r))),
    ("pane.empty_command", "an empty command", "- \"\" in a pane", lambda r: any(c == "" for c in _commands(r)) or any(p == "" for p in _pane_values(r))),
    ("command.enter", "enter: false", "enter on a pane or a command", lambda r: _anywhere(r, "enter")),
    ("expand.environment_variable", "${VAR} and $VAR", "a variable reference in a value", lambda r: bool(__import__("re").search(r"\$(\{[A-Za-z_]|[A-Za-z_])", json.dumps(r)))),
    ("expand.tilde", "~ in start_directory", "start_directory: ~", lambda r: any(_starts(r, level, "~") for level in ("session", "window", "pane"))),
    ("session.start_directory", "session start_directory", "start_directory at the top of the file", lambda r: "start_directory" in r),
    ("window.start_directory", "window start_directory", "start_directory in a window", lambda r: _has(r, "start_directory", "window")),
    ("pane.start_directory", "pane start_directory", "start_directory in a pane", lambda r: _has(r, "start_directory", "pane")),
    ("start_directory.relative_to_session", "a relative window start_directory", "a window path joined onto the session's", _window_relative),
    ("start_directory.dot_relative", "a ./ start_directory", "start_directory: ./", _dot),
    ("session.environment", "session environment", "environment at the top of the file", lambda r: "environment" in r),
    ("window.environment", "window environment", "environment in a window", lambda r: _has(r, "environment", "window")),
    ("pane.environment", "pane environment", "environment in a pane", lambda r: _has(r, "environment", "pane")),
    ("window.window_index", "window_index", "window_index in a window", lambda r: _has(r, "window_index")),
) + tuple(
    (f"window.layout.{name}", f"layout {name}", f"layout: {name}", (lambda n: lambda r: any(w.get("layout") == n for w in r.get("windows") or []))(name))
    for name in ("even-vertical", "main-horizontal", "main-vertical", "tiled")
)


def features(raw_by_name: dict[str, dict]) -> dict:
    out = []
    for feature_id, title, syntax, predicate in FEATURES:
        files = sorted(name for name, raw in raw_by_name.items() if predicate(raw))
        out.append({"id": feature_id, "title": title, "syntax": syntax, "files": files})
    return {"schema_version": SCHEMA_VERSION, "tmuxp": PINNED, "features": out}


def render(document: dict) -> str:
    return json.dumps(document, indent=2, ensure_ascii=False) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    import tmuxp

    if tmuxp.__version__ != PINNED:
        sys.exit(f"tmuxp {PINNED} is required, found {tmuxp.__version__}")
    # tmuxp resolves a leading `.` with Path.resolve, which follows symlinks.
    # The catalog's only such path sits under /var, so a host where /var is a
    # link (macOS) would write a different expectation.
    if os.path.realpath("/var") != "/var":
        sys.exit("/var is a symlink here; tmuxp would resolve through it")

    os.environ.clear()
    for variable in PATH_VARIABLES:
        os.environ[variable] = f"/@{variable}@"
    for variable in PLAIN_VARIABLES:
        os.environ[variable] = f"@{variable}@"

    examples = sorted(CATALOG.glob("*.yaml"))
    expected = {f"{path.stem}.json": render(structure(path.name, load(path))) for path in examples}
    from tmuxp._internal import config_reader

    raw = {path.name: config_reader.ConfigReader._from_file(path) or {} for path in examples}
    expected[FEATURES_FILE] = render(features(raw))

    stale = sorted(
        path.name
        for path in HERE.glob("*.json")
        if path.name not in expected and not path.name.endswith(".schema.json")
    )
    differing = [
        name
        for name, text in expected.items()
        if not (HERE / name).exists() or (HERE / name).read_text(encoding="utf-8") != text
    ]
    if args.check:
        for name in differing:
            print(f"differs: {name}")
        for name in stale:
            print(f"no example: {name}")
        print(f"{len(examples)} examples, {len(differing)} differ, {len(stale)} without an example")
        return 1 if differing or stale else 0
    for name, text in expected.items():
        (HERE / name).write_text(text, encoding="utf-8")
    for name in stale:
        (HERE / name).unlink()
    print(f"wrote {len(expected)} files")
    return 0


if __name__ == "__main__":
    sys.exit(main())
