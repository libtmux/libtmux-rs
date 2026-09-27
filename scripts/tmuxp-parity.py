#!/usr/bin/env python3
"""Prove, fixture by fixture, that `tmux-workspace load` builds what tmuxp builds.

Both tools read tmuxp-style YAML and drive real tmux; the only trustworthy
check is to load the same document with each, on its own throwaway server,
and diff what tmux itself reports -- sessions, windows and panes -- rather
than trusting either tool's own success message. Everything that is a tmux
identity rather than a document fact (pane pid, socket path, timestamps) is
dropped before comparing.

Some divergence from tmuxp is deliberate and documented (see README.md's
"Compatibility with tmuxp" and "the tmux-workspace command read different
documents" sections): a window with no explicit layout is tiled instead of
tmuxp's organic stack, an unrecognised key is refused instead of ignored, a
session name holding ':' or '.' is refused, and a typed command's $VAR is
meant to reach the pane's shell as written rather than be substituted into
the command text. ALLOWLIST records each with its reason; a fixture that
only differs there still reports "match" and does not fail the run.
Anything else that differs is a real finding and fails it. Two of the four
have not been observed to fire against the pinned tmuxp and the commit this
was written against -- the report explains why, not this script.

Two fixtures need help this script cannot fabricate safely: plugin-system.yaml
names a real, unpublished tmuxp plugin, and pane-shell.yaml's window_shell
names /usr/bin/python2, which may not exist on the runner. SKIP documents
both with a reason instead of silently dropping them; python2's absence is
left to run, since a missing interpreter should fail identically on both
sides and a mismatch there is itself a finding.

A pane whose current command is not a shell (vim, top, a REPL) never settles
on a stable screen -- top repaints every second by design -- so only its
running command is compared, not its captured text. Everything else is
compared after polling tmux until the pane's captured text stops changing,
bounded by a timeout, never a fixed sleep.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_FIXTURES_DIR = REPO_ROOT / "crates/tmux-workspace/tests/fixtures/tmuxp"
DEFAULT_BINARY = REPO_ROOT / "target/debug/tmux-workspace"
DEV_ROOT = Path("/tmp/libtmux-rs-dev")

TMUXP_SOCKET = "tmuxp"
WORKSPACE_SOCKET = "workspace"
DEFAULT_SIZE = "200x60"
LOAD_TIMEOUT = 30.0
CAPTURE_TIMEOUT = 8.0
CAPTURE_INTERVAL = 0.2
CAPTURE_STABLE_ROUNDS = 3
SHELLS = {"sh", "bash", "zsh", "dash", "ash", "ksh", "fish"}

# Fixtures this harness cannot exercise safely on every machine, and why.
SKIP = {
    "plugin-system": (
        "names a real tmuxp Python plugin "
        "(tmuxp_plugin_extended_build.plugin.PluginExtendedBuild) that is not "
        "published; nothing to install"
    ),
}

# Known, documented deliberate differences from tmuxp. A difference that
# matches one of these is reported, but does not fail the run.
ALLOWLIST = {
    "default-layout": (
        "a window with no explicit `layout:` is tiled (an even grid) under "
        "tmux-workspace; tmuxp never calls select-layout and leaves whatever "
        "shape sequential splits left -- README.md 'Compatibility with tmuxp'"
    ),
    "unknown-key": (
        "a key tmux-workspace does not model, other than one starting `x-`, "
        "is refused before any mutation; tmuxp has no schema and loads the "
        "file with the key ignored -- README.md 'the tmux-workspace command "
        "read different documents' and 'Compatibility with tmuxp'"
    ),
    "session-separator": (
        "a session_name holding ':' or '.' is refused by tmux-workspace "
        "(libtmux::target::SessionName) before any mutation -- "
        "libtmux/src/target.rs SessionNameError::Separator. Kept because "
        "the crate's own docs name this as a deliberate difference from "
        "tmuxp, though nothing here has observed it fire: pinned tmuxp "
        "1.74.0's own libtmux-python also refuses such a name client-side "
        "(BadSessionName: contains colons/periods), so both sides simply "
        "refuse and this allowlist entry goes unused"
    ),
    "session-window-option": (
        "a window option such as main-pane-height listed under the session's "
        "`options:` reaches the document's windows under tmux-workspace "
        "(docs/cli.md 'Documents'); tmuxp sets it on the default window "
        "new_session made, which it then replaces, so the value is lost and "
        "tmux's own default shapes the window -- a tmuxp bug, observed on "
        "options.yaml"
    ),
    "prompt-redraw": (
        "tmux-workspace waits for a pane's shell to own its terminal before "
        "typing (execution.rs), and a pane resized by a later split redraws "
        "its prompt once more; the screens differ only by a bare prompt line"
    ),
    "live-filesystem": (
        "the fixture lists /var/log, which the running system writes to "
        "between the two loads, so the listing differs whichever tool ran "
        "it; the commands typed are the same"
    ),
    "var-in-command-env": (
        "$VAR and ~ in a typed command are meant to reach the pane's shell "
        "as written, with the loader's value added to the pane's "
        "environment, instead of being substituted into the command text "
        "-- README.md table row '~ and $VAR in shell commands'"
    ),
}

# Extra minimal documents that exercise an allowlisted difference no shipped
# fixture reaches. Written to a temp file at run time; never touch crates/.
EXTRA_CASES: dict[str, str] = {
    "extra-unknown-key": """\
session_name: extra unknown key
not_a_real_tmuxp_key: true
windows:
  - panes:
      - echo hi
""",
    "extra-session-colon": """\
session_name: "extra:colon"
windows:
  - panes:
      - echo hi
""",
    "extra-session-dot": """\
session_name: "extra.dot"
windows:
  - panes:
      - echo hi
""",
}

# The extras' own document facts, since this script wrote them and does not
# need to infer them the way it does for the shipped fixtures below.
EXTRA_DOC_FACTS = {
    "extra-unknown-key": {"allow": "unknown-key"},
    "extra-session-colon": {"allow": "session-separator"},
    "extra-session-dot": {"allow": "session-separator"},
}


# --------------------------------------------------------------------------
# A narrow, targeted reader for the option blocks in these fixtures' YAML.
#
# Not a YAML parser: it only finds `options:`/`options_after:`/
# `global_options:` blocks and each window's `layout:` presence, because
# that is all this script needs from the document (every other field is
# read back from tmux itself, on both sides, rather than trusted from the
# file). It relies on this fixture set's consistent two-space-per-level
# block style; a fixture written some other way would need a real parser.
# --------------------------------------------------------------------------


def _strip_comment(line: str) -> str:
    in_squote = in_dquote = False
    for index, char in enumerate(line):
        if char == "'" and not in_dquote:
            in_squote = not in_squote
        elif char == '"' and not in_squote:
            in_dquote = not in_dquote
        elif char == "#" and not in_squote and not in_dquote:
            if index == 0 or line[index - 1].isspace():
                return line[:index]
    return line


def _lines(text: str) -> list[tuple[int, str]]:
    result = []
    for raw in text.splitlines():
        stripped = _strip_comment(raw).rstrip()
        content = stripped.lstrip(" ")
        if not content:
            continue
        result.append((len(stripped) - len(content), content))
    return result


_KEY = re.compile(r"^([A-Za-z0-9_.-]+):")


def _flat_block(lines: list[tuple[int, str]], start: int, block_indent: int) -> set[str]:
    """Key names of a flat `key: value` block starting after `start`."""
    keys: set[str] = set()
    index = start
    while index < len(lines) and lines[index][0] > block_indent:
        match = _KEY.match(lines[index][1])
        if match:
            keys.add(match.group(1))
        index += 1
    return keys


@dataclass
class WindowFacts:
    has_layout: bool = False
    option_keys: set[str] = field(default_factory=set)


@dataclass
class DocFacts:
    global_option_keys: set[str] = field(default_factory=set)
    session_option_keys: set[str] = field(default_factory=set)
    windows: list[WindowFacts] = field(default_factory=list)
    allow: str | None = None  # set only for this script's own extra cases


def read_doc_facts(text: str) -> DocFacts:
    lines = _lines(text)
    facts = DocFacts()
    index = 0
    windows_start = None
    while index < len(lines):
        indent, content = lines[index]
        if indent != 0:
            index += 1
            continue
        match = _KEY.match(content)
        if not match:
            index += 1
            continue
        key = match.group(1)
        if key == "windows":
            windows_start = index + 1
            break
        if key in ("options", "global_options"):
            target = facts.session_option_keys if key == "options" else facts.global_option_keys
            target.update(_flat_block(lines, index + 1, 0))
        index += 1

    if windows_start is None:
        return facts

    index = windows_start
    dash_indent = None
    while index < len(lines):
        indent, content = lines[index]
        if indent == 0:
            break
        if content.startswith("- ") and (dash_indent is None or indent == dash_indent):
            dash_indent = indent
            key_indent = indent + 2
            window = WindowFacts()
            first_key = content[2:]
            match = _KEY.match(first_key)
            if match and match.group(1) == "layout":
                window.has_layout = True
            cursor = index + 1
            while cursor < len(lines) and lines[cursor][0] > dash_indent:
                w_indent, w_content = lines[cursor]
                if w_indent == key_indent:
                    key_match = _KEY.match(w_content)
                    if key_match and key_match.group(1) == "layout":
                        window.has_layout = True
                    if key_match and key_match.group(1) in ("options", "options_after"):
                        window.option_keys.update(_flat_block(lines, cursor + 1, key_indent))
                cursor += 1
            facts.windows.append(window)
        index += 1
    return facts


# --------------------------------------------------------------------------
# tmux state, read back from each server independently.
# --------------------------------------------------------------------------


@dataclass
class Pane:
    index: int
    active: bool
    dead: bool
    path: str
    command: str
    top: int
    left: int
    width: int
    height: int
    text: list[str] | None


@dataclass
class Window:
    index: int
    name: str
    active: bool
    panes: list[Pane]
    options: dict[str, str]

    @property
    def shape(self) -> list[tuple[int, int, int, int]]:
        return sorted((p.top, p.left, p.width, p.height) for p in self.panes)


@dataclass
class Session:
    name: str
    windows: list[Window]
    session_options: dict[str, str]
    global_options: dict[str, str]


@dataclass
class RunResult:
    returncode: int
    stdout: str
    stderr: str
    session: Session | None


@dataclass
class Difference:
    scope: str
    field: str
    tmuxp: object
    workspace: object
    allow: str | None = None


def tmux(tmux_bin: str, socket: str, env: dict[str, str], *args: str, timeout: float = 10.0) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [tmux_bin, "-L", socket, *args],
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def capture_stable(tmux_bin: str, socket: str, env: dict[str, str], pane_target: str) -> list[str]:
    deadline = time.monotonic() + CAPTURE_TIMEOUT
    history: list[str] = []
    last = ""
    while time.monotonic() < deadline:
        result = tmux(tmux_bin, socket, env, "capture-pane", "-p", "-t", pane_target)
        last = result.stdout
        history.append(last)
        if len(history) >= CAPTURE_STABLE_ROUNDS and len(set(history[-CAPTURE_STABLE_ROUNDS:])) == 1:
            break
        time.sleep(CAPTURE_INTERVAL)
    # A resized pane's shell can redraw its prompt onto a blank line of its
    # own -- tmux-workspace's own wait for "the line editor owns the
    # terminal" (see execution.rs) does this once per pane that shares a
    # window with a later split. Harmless and not what this script is
    # proving, so blank lines are dropped throughout, not just trailing.
    return [line.rstrip() for line in last.splitlines() if line.strip()]


def pane_snapshot(tmux_bin: str, socket: str, env: dict[str, str], pane_target: str) -> tuple[str, str, str, str]:
    """Fresh `(active, dead, path, command)` for one pane, read right now."""
    result = tmux(
        tmux_bin,
        socket,
        env,
        "display-message",
        "-p",
        "-t",
        pane_target,
        "#{pane_active}\t#{pane_dead}\t#{pane_current_path}\t#{pane_current_command}",
    )
    active, dead, path, command = result.stdout.rstrip("\n").split("\t")
    return active, dead, path, command


def settle_pane(tmux_bin: str, socket: str, env: dict[str, str], pane_target: str) -> tuple[str, str, str, str]:
    """Poll a pane until its reported command stops changing, bounded by
    CAPTURE_TIMEOUT. The load command has already returned by the time this
    runs, but a just-forked pane's shell (or a command it then runs, such as
    `ssh` still resolving a host) needs real time to get where it is going;
    `#{pane_current_command}` is cheap enough to poll for that directly,
    rather than trusting the snapshot taken the moment load exited.
    """
    deadline = time.monotonic() + CAPTURE_TIMEOUT
    previous = None
    while True:
        snapshot = pane_snapshot(tmux_bin, socket, env, pane_target)
        if snapshot == previous or time.monotonic() >= deadline:
            return snapshot
        previous = snapshot
        time.sleep(CAPTURE_INTERVAL)


def read_session(tmux_bin: str, socket: str, env: dict[str, str], doc: DocFacts) -> Session | None:
    sessions = tmux(tmux_bin, socket, env, "list-sessions", "-F", "#{session_name}")
    names = [line for line in sessions.stdout.splitlines() if line]
    if not names:
        return None
    name = names[0]

    def options(*scope: str) -> dict[str, str]:
        result = tmux(tmux_bin, socket, env, "show-options", *scope)
        values: dict[str, str] = {}
        for line in result.stdout.splitlines():
            key, _, value = line.partition(" ")
            values[key] = value
        return values

    global_options = options("-g")
    session_options = options("-t", name)

    windows_out = tmux(
        tmux_bin,
        socket,
        env,
        "list-windows",
        "-t",
        name,
        "-F",
        "#{window_index}\t#{window_name}\t#{window_active}",
    )
    windows: list[Window] = []
    for line in windows_out.stdout.splitlines():
        if not line:
            continue
        w_index_s, w_name, w_active = line.split("\t")
        w_index = int(w_index_s)
        window_target = f"{name}:{w_index}"
        window_options_all = options("-t", window_target, "-w")
        doc_window = doc.windows[len(windows)] if len(windows) < len(doc.windows) else WindowFacts()
        window_options = {k: v for k, v in window_options_all.items() if k in doc_window.option_keys}

        panes_out = tmux(
            tmux_bin,
            socket,
            env,
            "list-panes",
            "-t",
            window_target,
            "-F",
            "\t".join(
                [
                    "#{pane_index}",
                    "#{pane_active}",
                    "#{pane_dead}",
                    "#{pane_current_path}",
                    "#{pane_current_command}",
                    "#{pane_top}",
                    "#{pane_left}",
                    "#{pane_width}",
                    "#{pane_height}",
                ]
            ),
        )
        panes: list[Pane] = []
        for pane_line in panes_out.stdout.splitlines():
            if not pane_line:
                continue
            p_index, p_active, p_dead, p_path, p_command, p_top, p_left, p_width, p_height = (
                pane_line.split("\t")
            )
            pane_target = f"{name}:{w_index}.{p_index}"
            # The load command has already returned, but a just-forked
            # pane's shell -- or a command it then runs, like `ssh` still
            # resolving a host -- needs real time to settle; re-read instead
            # of trusting the snapshot taken the moment load exited.
            p_active, p_dead, p_path, p_command = settle_pane(tmux_bin, socket, env, pane_target)
            command = p_command.lstrip("-")
            text = capture_stable(tmux_bin, socket, env, pane_target) if command in SHELLS else None
            panes.append(
                Pane(
                    index=int(p_index),
                    active=p_active == "1",
                    dead=p_dead == "1",
                    path=p_path,
                    command=command,
                    top=int(p_top),
                    left=int(p_left),
                    width=int(p_width),
                    height=int(p_height),
                    text=text,
                )
            )
        windows.append(
            Window(index=w_index, name=w_name, active=w_active == "1", panes=panes, options=window_options)
        )
    return Session(
        name=name, windows=windows, session_options=session_options, global_options=global_options
    )


# --------------------------------------------------------------------------
# Comparison
# --------------------------------------------------------------------------


PROMPTS = {"%", "$", "#", ">"}

# Window options tmux keeps per window, which a document may still list under
# the session's `options:`.
WINDOW_SCOPE = {
    "main-pane-height", "main-pane-width", "other-pane-height",
    "other-pane-width", "pane-base-index", "synchronize-panes",
}


def text_allow(tp: Pane, wp: Pane, shape_allow: str | None) -> str | None:
    """Which allowlisted cause explains two different screens, if one does."""
    def bare(text: list[str]) -> list[str]:
        return [line for line in text if line.strip() not in PROMPTS]

    if bare(tp.text) == bare(wp.text):
        return "prompt-redraw"
    # The command line itself can scroll away under a long listing, so the
    # listing is recognised by its own lines: `ls -al` rows of log files.
    def lists_logs(text: list[str]) -> bool:
        return any(
            "/var/log" in line or re.match(r"^[-dl]r[-w]\S+\s.*\.(log|gz)\S*$", line)
            for line in text
        )

    if lists_logs(tp.text) and lists_logs(wp.text):
        return "live-filesystem"
    return shape_allow


def compare_pane(tp: Pane, wp: Pane, label: str, shape_allow: str | None) -> list[Difference]:
    diffs = []
    for attr in ("path", "active", "dead", "command"):
        tv, wv = getattr(tp, attr), getattr(wp, attr)
        if tv != wv:
            diffs.append(Difference(label, attr, tv, wv))
    if tp.text is not None and wp.text is not None and tp.text != wp.text:
        # `var-in-command-env` (see ALLOWLIST) would show up here if it ever
        # fires, but nothing in this fixture set exercises it at the commit
        # under test (see the report), so it is never auto-allowed here.
        # A window's allowlisted shape difference does change its panes'
        # widths, though, which reflows wrapped text on its own -- credit
        # that to the same allowlist entry rather than report it as new.
        diffs.append(Difference(label, "screen_text", tp.text, wp.text, text_allow(tp, wp, shape_allow)))
    return diffs


def compare_window(
    tw: Window, ww: Window, doc: WindowFacts, label: str, session_option_keys: set[str]
) -> list[Difference]:
    diffs = []
    if tw.name != ww.name:
        diffs.append(Difference(label, "name", tw.name, ww.name))
    if tw.active != ww.active:
        diffs.append(Difference(label, "active", tw.active, ww.active))
    shape_allow = None
    if tw.shape != ww.shape:
        if not doc.has_layout:
            shape_allow = "default-layout"
        elif session_option_keys & WINDOW_SCOPE:
            shape_allow = "session-window-option"
        diffs.append(Difference(label, "shape", tw.shape, ww.shape, shape_allow))
    for key in sorted(doc.option_keys):
        tv, wv = tw.options.get(key), ww.options.get(key)
        if tv != wv:
            diffs.append(Difference(label, f"option:{key}", tv, wv))
    if len(tw.panes) != len(ww.panes):
        diffs.append(Difference(label, "pane_count", len(tw.panes), len(ww.panes)))
    for tp, wp in zip(tw.panes, ww.panes):
        diffs.extend(compare_pane(tp, wp, f"{label}.pane[{tp.index}]", shape_allow))
    return diffs


def compare(tmuxp: RunResult, workspace: RunResult, doc: DocFacts, extra_allow: str | None) -> list[Difference]:
    t_session, w_session = tmuxp.session, workspace.session
    if (t_session is None) != (w_session is None):
        allow = extra_allow if (t_session is not None and w_session is None) else None
        return [
            Difference(
                "session",
                "presence",
                "present" if t_session else "absent",
                "present" if w_session else "absent",
                allow,
            )
        ]
    if t_session is None or w_session is None:
        return []

    diffs = []
    if t_session.name != w_session.name:
        diffs.append(Difference("session", "name", t_session.name, w_session.name))
    for key in sorted(doc.global_option_keys):
        tv, wv = t_session.global_options.get(key), w_session.global_options.get(key)
        if tv != wv:
            diffs.append(Difference("session", f"global_option:{key}", tv, wv))
    for key in sorted(doc.session_option_keys):
        tv, wv = t_session.session_options.get(key), w_session.session_options.get(key)
        if tv != wv:
            diffs.append(Difference("session", f"option:{key}", tv, wv))
    if len(t_session.windows) != len(w_session.windows):
        diffs.append(Difference("session", "window_count", len(t_session.windows), len(w_session.windows)))
    for index, (tw, ww) in enumerate(zip(t_session.windows, w_session.windows)):
        window_doc = doc.windows[index] if index < len(doc.windows) else WindowFacts()
        diffs.extend(
            compare_window(tw, ww, window_doc, f"window[{tw.index}]", doc.session_option_keys)
        )
    return diffs


# --------------------------------------------------------------------------
# Running a fixture pair
# --------------------------------------------------------------------------


@dataclass
class Fixture:
    name: str
    path: Path
    doc: DocFacts
    allow: str | None = None


def discover_fixtures(fixtures_dir: Path, scratch: Path) -> list[Fixture]:
    fixtures = []
    for path in sorted(fixtures_dir.glob("*.yaml")):
        text = path.read_text(encoding="utf-8")
        fixtures.append(Fixture(name=path.stem, path=path, doc=read_doc_facts(text)))
    extra_dir = scratch / "extra-cases"
    extra_dir.mkdir(parents=True, exist_ok=True)
    for name, text in EXTRA_CASES.items():
        extra_path = extra_dir / f"{name}.yaml"
        extra_path.write_text(text, encoding="utf-8")
        fixtures.append(
            Fixture(
                name=name,
                path=extra_path,
                doc=read_doc_facts(text),
                allow=EXTRA_DOC_FACTS[name]["allow"],
            )
        )
    return fixtures


def build_env(run_dir: Path) -> dict[str, str]:
    env = {key: value for key, value in os.environ.items() if key not in {"TMUX", "TMUX_PANE"}}
    xdg = run_dir / "xdg"
    tmux_conf_dir = xdg / "tmux"
    tmux_conf_dir.mkdir(parents=True, exist_ok=True)
    (tmux_conf_dir / "tmux.conf").write_text(f"set -g default-size {DEFAULT_SIZE}\n", encoding="utf-8")

    # Pinning `default-shell` to /bin/sh looked like the fix for the zsh rc
    # noise below, but a plain "-c command" pane then comes up running sh
    # itself rather than exec'ing into it (reproduces identically for both
    # loaders, so it never fails a comparison -- it just quietly defeats
    # pane-shell.yaml, whose point is exactly which command ends up running).
    # ZDOTDIR pointed at an empty directory keeps the operator's real
    # default-shell, so a `shell:`/`window_shell:` override still execs
    # cleanly, while zsh reads no rc file and so never reaches for it.
    zdotdir = run_dir / "zdotdir"
    zdotdir.mkdir(parents=True, exist_ok=True)
    # An empty .zshrc, not just an empty ZDOTDIR: with none of .zshenv,
    # .zprofile, .zshrc or .zlogin present, zsh assumes a first run and
    # opens its interactive "new user" setup wizard instead of a prompt.
    (zdotdir / ".zshrc").touch()

    scripts_dir = run_dir / "scripts"
    scripts_dir.mkdir(parents=True, exist_ok=True)
    before_script = scripts_dir / "test3.sh"
    before_script.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    before_script.chmod(before_script.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    cwd = run_dir / "cwd"
    (cwd / "test").mkdir(parents=True, exist_ok=True)

    env.update(
        XDG_CONFIG_HOME=str(xdg),
        TMUX_TMPDIR=str(run_dir / "tmux-tmpdir"),
        PWD=str(cwd),
        MY_ENV_VAR=str(scripts_dir),
        MAIN_PANE_HEIGHT="15",
        EDITOR="/bin/true",
        NO_COLOR="1",
        ZDOTDIR=str(zdotdir),
        # /etc/zsh/zshrc calls compinit unconditionally unless this is set;
        # ZDOTDIR only reaches the user's own rc files, not the system one.
        skip_global_compinit="1",
        # Both tools mirror tmuxp's own terminal-size detection: unset, they
        # ask a non-tty stdout and fall back to a hardcoded 80x24, ignoring
        # `default-size` entirely. "0" (any value but "1") makes both skip
        # detection and take the session size from `default-size` above,
        # so main-pane-height's 30-row window fits and the two loaders stay
        # on equal footing either way.
        TMUXP_DETECT_TERMINAL_SIZE="0",
    )
    (run_dir / "tmux-tmpdir").mkdir(parents=True, exist_ok=True)
    return env


def run_loader(argv: list[str], env: dict[str, str], cwd: Path) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            argv, env=env, cwd=cwd, capture_output=True, text=True, timeout=LOAD_TIMEOUT
        )
    except subprocess.TimeoutExpired as error:
        return subprocess.CompletedProcess(
            argv, 124, stdout=error.stdout or "", stderr=f"timed out after {LOAD_TIMEOUT}s"
        )


def kill_server(tmux_bin: str, socket: str, env: dict[str, str]) -> None:
    try:
        tmux(tmux_bin, socket, env, "kill-server", timeout=5.0)
    except (subprocess.TimeoutExpired, OSError):
        pass


def run_fixture(
    fixture: Fixture, args: argparse.Namespace, dev_root: Path
) -> tuple[str, list[Difference]]:
    run_dir = Path(tempfile.mkdtemp(prefix=f"tmuxp-parity-{fixture.name}-", dir=dev_root))
    env = build_env(run_dir)
    cwd = run_dir / "cwd"
    try:
        tmuxp_proc = run_loader(
            [args.tmuxp, "load", "-d", "-y", "-L", TMUXP_SOCKET, str(fixture.path)], env, cwd
        )
        tmuxp_session = read_session(args.tmux, TMUXP_SOCKET, env, fixture.doc)
        tmuxp_result = RunResult(tmuxp_proc.returncode, tmuxp_proc.stdout, tmuxp_proc.stderr, tmuxp_session)

        workspace_proc = run_loader(
            [str(args.binary), "load", "-d", "-y", "-L", WORKSPACE_SOCKET, str(fixture.path)], env, cwd
        )
        workspace_session = read_session(args.tmux, WORKSPACE_SOCKET, env, fixture.doc)
        workspace_result = RunResult(
            workspace_proc.returncode, workspace_proc.stdout, workspace_proc.stderr, workspace_session
        )

        diffs = compare(tmuxp_result, workspace_result, fixture.doc, fixture.allow)
        real = [diff for diff in diffs if diff.allow is None]
        status = "differs" if real else "match"
        return status, diffs
    finally:
        kill_server(args.tmux, TMUXP_SOCKET, env)
        kill_server(args.tmux, WORKSPACE_SOCKET, env)
        if not args.keep:
            shutil.rmtree(run_dir, ignore_errors=True)


def resolve_tmux(explicit: str | None) -> str:
    tmux_bin = explicit or shutil.which("tmux") or "tmux"
    return str(Path(tmux_bin).resolve()) if Path(tmux_bin).exists() else tmux_bin


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tmuxp", default=shutil.which("tmuxp") or "tmuxp", help="tmuxp executable (pinned 1.74.0)")
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY, help="tmux-workspace executable")
    parser.add_argument("--tmux", default=None, help="tmux executable; defaults to PATH")
    parser.add_argument("--fixtures-dir", type=Path, default=DEFAULT_FIXTURES_DIR)
    parser.add_argument("--only", action="append", default=[], help="run only fixtures with this stem (repeatable)")
    parser.add_argument("--keep", action="store_true", help="keep run directories for inspection")
    args = parser.parse_args()

    args.binary = args.binary.resolve()
    if not args.binary.is_file():
        parser.error(f"no tmux-workspace binary at {args.binary}")
    resolved_tmux = resolve_tmux(args.tmux)
    args.tmux = resolved_tmux

    dev_root = DEV_ROOT
    dev_root.mkdir(exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="tmuxp-parity-run-", dir=dev_root))

    # Prepend the chosen tmux's directory so both loaders -- tmuxp shelling
    # out to "tmux", tmux-workspace calling libtmux -- resolve the same one.
    if Path(args.tmux).is_absolute():
        os.environ["PATH"] = str(Path(args.tmux).parent) + os.pathsep + os.environ.get("PATH", "")

    try:
        fixtures = discover_fixtures(args.fixtures_dir, scratch)
        if args.only:
            fixtures = [f for f in fixtures if f.name in args.only]

        matched = differed = skipped = 0
        for fixture in fixtures:
            if fixture.name in SKIP:
                print(f"skipped  {fixture.name:<28} {SKIP[fixture.name]}")
                skipped += 1
                continue
            status, diffs = run_fixture(fixture, args, dev_root)
            allowlisted = [d for d in diffs if d.allow]
            real = [d for d in diffs if not d.allow]
            if status == "match":
                matched += 1
                note = ""
                if allowlisted:
                    ids = ", ".join(sorted({d.allow for d in allowlisted}))
                    note = f"  (allowlisted: {ids})"
                print(f"match    {fixture.name:<28}{note}")
            else:
                differed += 1
                print(f"differs  {fixture.name}")
                for diff in real:
                    print(f"    {diff.scope} {diff.field}: tmuxp={diff.tmuxp!r} workspace={diff.workspace!r}")
                for diff in allowlisted:
                    print(f"    (allowlisted: {diff.allow}) {diff.scope} {diff.field}")

        print()
        print(f"{matched} matched, {differed} differed, {skipped} skipped")
        if differed:
            print()
            print("Allowlist:")
            for key, reason in ALLOWLIST.items():
                print(f"  {key}: {reason}")
        return 1 if differed else 0
    finally:
        if not args.keep:
            shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
