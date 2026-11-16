#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Cargo target runner (see .cargo/config.toml).

    sandbox_runner.py BINARY [ARGUMENT ...]

Runs one test, example or bench binary with no inherited tmux server and a
private default socket directory, then stops any server the binary left there.

Exit status: the binary's own; 78 when the run is refused before the binary
starts; 70 when the binary passed but left a server, or the directory could
not be checked, or a server started outside the sandbox; 130 when interrupted
and the binary passed. Messages name variables and sockets, never values.
"""

from __future__ import annotations

import contextlib
import os
import re
import secrets
import shutil
import signal
import stat
import string
import subprocess
import sys
import time
from collections.abc import Mapping, MutableMapping, Sequence
from pathlib import Path
from types import FrameType
from typing import Self

import sandbox_outside

REFUSED = 78
LEFT_A_SERVER = 70
INTERRUPTED = 130

# tmux appends tmux-<uid>/default, and sun_path holds 104 bytes on macOS and
# 108 on Linux.
SOCKET_LIMIT = 100
PREFIX = "ltrs."
SUFFIX_LENGTH = 6
# `kill-server` answers before the daemon exits, so a stopped server gets this
# many tries, 0.1 s apart, to stop answering.
STOP_TRIES = 30
STOP_PAUSE = 0.1
# A tmux client that has not answered by then is talking to a server that is
# wedged; waiting longer would hang the run that wraps every test binary.
TMUX_SECONDS = 10
CARGO_FALLBACK = "/.cargo/tmux-sandbox"
SERVER_VARIABLES = ("TMUX", "TMUX_PANE", "LIBTMUX_SOCKET", "LIBTMUX_SOCKET_PATH")
HELD = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)


class Refused(Exception):
    """The run cannot start; the message says why."""


def say(message: str) -> None:
    print(f"sandbox-runner: {message}", file=sys.stderr)


# The `bin` directory of an environment `uv run` made for a script.
UV_ENVIRONMENT_BIN = re.compile(r"/environments-v[0-9]+/[^/]+/bin$")


def restore_environment(environ: MutableMapping[str, str]) -> None:
    """Remove what `uv run` adds, so the binary sees the caller's environment.

    uv puts the script's own environment first in `PATH` and exports
    `VIRTUAL_ENV`, `UV` and `UV_RUN_RECURSION_DEPTH`. A caller that was itself
    started by `uv run` has added one of these too, so every environment of
    that kind is taken out of `PATH`.

    >>> environ = {
    ...     "PATH": "/c/uv/environments-v2/a-1/bin:/c/uv/environments-v2/b-2/bin:/usr/bin",
    ...     "VIRTUAL_ENV": "/c/uv/environments-v2/a-1",
    ...     "UV": "/bin/uv",
    ...     "UV_RUN_RECURSION_DEPTH": "2",
    ... }
    >>> restore_environment(environ)
    >>> environ
    {'PATH': '/usr/bin'}
    """
    if "UV_RUN_RECURSION_DEPTH" not in environ:
        return
    for name in ("VIRTUAL_ENV", "UV", "UV_RUN_RECURSION_DEPTH"):
        environ.pop(name, None)
    entries = environ.get("PATH", "").split(os.pathsep)
    environ["PATH"] = os.pathsep.join(
        entry for entry in entries if not UV_ENVIRONMENT_BIN.search(entry)
    )


def choose_parent(environ: Mapping[str, str]) -> Path:
    """The directory the sandbox is made in: absolute, and short enough for a socket.

    >>> choose_parent({"TMPDIR": "relative"})
    Traceback (most recent call last):
        ...
    sandbox_runner.Refused: TMPDIR is not an absolute path
    """
    parent = environ.get("TMPDIR") or "/tmp"
    parent = parent.removesuffix("/")
    if not parent.startswith("/"):
        raise Refused("TMPDIR is not an absolute path")
    if not os.path.isdir(parent):
        raise Refused("TMPDIR is not a directory")
    try:
        # The path tmux binds is the resolved one: /var is /private/var on macOS.
        resolved = os.path.realpath(parent, strict=True)
    except OSError:
        raise Refused("TMPDIR cannot be resolved") from None
    longest = f"{resolved}/{PREFIX}{'X' * SUFFIX_LENGTH}/tmux-{os.getuid()}/default"
    if len(os.fsencode(longest)) > SOCKET_LIMIT:
        raise Refused(
            f"TMPDIR is too long for a tmux socket path; the limit is {SOCKET_LIMIT} bytes"
        )
    return Path(parent)


def make_sandbox(parent: Path) -> Path:
    """Make an owner-only directory `ltrs.XXXXXX` in `parent`."""
    alphabet = string.ascii_letters + string.digits
    for _ in range(100):
        sandbox = parent / (
            PREFIX + "".join(secrets.choice(alphabet) for _ in range(SUFFIX_LENGTH))
        )
        try:
            sandbox.mkdir(mode=0o700)
        except FileExistsError:
            continue
        except OSError:
            break
        return sandbox
    raise Refused("cannot create a private TMUX_TMPDIR under TMPDIR")


def child_environment(environ: Mapping[str, str], sandbox: Path, named_tmux: str) -> dict[str, str]:
    """What the binary inherits: no server, the sandbox, and a named `TMUX` if given."""
    kept = {
        name: value
        for name, value in environ.items()
        if name not in (*SERVER_VARIABLES, "LIBTMUX_SANDBOX_TMUX", "LIBTMUX_SANDBOX_FALLBACK_ROOT")
    }
    kept["TMUX_TMPDIR"] = str(sandbox)
    kept["LIBTMUX_TEST_SANDBOX"] = str(sandbox)
    if named_tmux:
        kept["TMUX"] = named_tmux
    return kept


def find_sockets(root: Path) -> tuple[list[Path], bool]:
    """Every socket under `root`, links not followed, and whether all of it was searched."""
    sockets: list[Path] = []
    complete = True
    pending = [root]
    while pending:
        directory = pending.pop()
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            pending.append(Path(entry.path))
                        elif stat.S_ISSOCK(entry.stat(follow_symlinks=False).st_mode):
                            sockets.append(Path(entry.path))
                    except OSError:
                        complete = False
        except OSError:
            complete = False
    return sorted(sockets), complete


def answers(tmux: str, socket: Path, environ: Mapping[str, str]) -> bool:
    """Whether a server listens on `socket`; a protocol mismatch still means one does."""
    try:
        result = subprocess.run(
            [tmux, "-N", "-S", str(socket), "list-sessions"],
            env=dict(environ),
            capture_output=True,
            stdin=subprocess.DEVNULL,
            text=True,
            errors="replace",
            check=False,
            timeout=TMUX_SECONDS,
        )
    except subprocess.TimeoutExpired:
        return True
    except OSError:
        return False
    return result.returncode == 0 or "protocol version mismatch" in result.stdout + result.stderr


def stop(tmux: str, socket: Path, environ: Mapping[str, str]) -> bool:
    """Ask the server on `socket` to stop and wait for it; whether it did."""
    with contextlib.suppress(subprocess.TimeoutExpired):
        subprocess.run(
            [tmux, "-S", str(socket), "kill-server"],
            env=dict(environ),
            capture_output=True,
            stdin=subprocess.DEVNULL,
            check=False,
            timeout=TMUX_SECONDS,
        )
    for _ in range(STOP_TRIES):
        if not answers(tmux, socket, environ):
            return True
        time.sleep(STOP_PAUSE)
    return not answers(tmux, socket, environ)


def pick_tmux(environ: Mapping[str, str]) -> str | None:
    """`$LIBTMUX_TEST_TMUX`, else `tmux`, whichever is there to run; `None` if neither."""
    path = environ.get("PATH")
    wanted = environ.get("LIBTMUX_TEST_TMUX") or "tmux"
    if shutil.which(wanted, path=path) is None:
        wanted = "tmux"
    return wanted if shutil.which(wanted, path=path) is not None else None


class Interrupts:
    """Holds `INT`, `TERM` and `HUP` until released, so the sweep still runs.

    A signal that was ignored when the runner started stays ignored, as it does
    for the binary: a shell cannot trap what it inherits as ignored either.
    """

    def __init__(self) -> None:
        self.caught = False
        self._held: list[signal.Signals] = []

    def _note(self, number: int, frame: FrameType | None) -> None:
        self.caught = True

    def __enter__(self) -> Self:
        for number in HELD:
            if signal.getsignal(number) is not signal.SIG_IGN:
                signal.signal(number, self._note)
                self._held.append(number)
        return self

    def __exit__(self, *exc: object) -> None:
        for number in self._held:
            signal.signal(number, signal.SIG_DFL)


def run_binary(command: Sequence[str], environ: Mapping[str, str]) -> int:
    """Run the binary and return its status as a shell would report it."""
    try:
        child = subprocess.Popen(command, env=dict(environ), close_fds=False)
    except FileNotFoundError:
        say(f"{command[0]}: No such file or directory")
        return 127
    except PermissionError:
        say(f"{command[0]}: Permission denied")
        return 126
    code = child.wait()
    return code if code >= 0 else 128 - code


def join(names: Sequence[str]) -> str:
    """` a, b`, the way the messages list sockets after a colon."""
    return "".join(f" {name}," for name in names).removesuffix(",")


def main(argv: Sequence[str], environ: MutableMapping[str, str] | None = None) -> int:
    """Run `argv` in the sandbox and return its status, or REFUSED when the sandbox says no."""
    environ = dict(os.environ) if environ is None else environ
    try:
        return run(argv, environ)
    except Refused as refusal:
        say(str(refusal))
        return REFUSED


def run(argv: Sequence[str], environ: MutableMapping[str, str]) -> int:
    """Run `argv` with no inherited tmux server and a private tmux directory, then sweep it.

    Returns the binary's status, or LEFT_A_SERVER when it passed but left a server behind.
    """
    if not argv:
        raise Refused("no binary to run")
    restore_environment(environ)

    # Where tmux would have put a bare client's server had the sandbox not: under
    # the TMUX_TMPDIR received, which under Cargo is its `[env]` fallback, and
    # under /tmp, or the directory named for tests. Only the first is skipped.
    caller_tmpdir = environ.get("TMUX_TMPDIR", "")
    if caller_tmpdir.endswith(CARGO_FALLBACK):
        caller_tmpdir = ""
    fallback_named = bool(environ.get("LIBTMUX_SANDBOX_FALLBACK_ROOT"))
    fallback_root = environ.get("LIBTMUX_SANDBOX_FALLBACK_ROOT") or "/tmp"
    # LIBTMUX_SANDBOX_TMUX is the one way back to a server, and it names it. The
    # TMUX received is never the source, and TMUX_PANE stays unset.
    named_tmux = environ.get("LIBTMUX_SANDBOX_TMUX", "")

    parent = choose_parent(environ)
    sandbox = make_sandbox(parent)
    binary_environment = child_environment(environ, sandbox, named_tmux)

    if named_tmux:
        say("LIBTMUX_SANDBOX_TMUX is set: true; TMUX comes from it and TMUX_PANE is unset")
    if fallback_named:
        say(
            "LIBTMUX_SANDBOX_FALLBACK_ROOT is set: true; "
            "it names the fallback directory to watch in place of /tmp"
        )

    # Anything newer than this, outside the sandbox, was made after this point.
    reference = sandbox / ".reference"
    if not sandbox_outside.make_reference(reference):
        shutil.rmtree(sandbox, ignore_errors=True)
        raise Refused("cannot write under the private TMUX_TMPDIR")

    # Held until the binary exits, so an interrupt still reaches the sweep below.
    with Interrupts() as interrupts:
        status = run_binary(argv, binary_environment)

    outside = sandbox_outside.started_since(reference, (fallback_root, caller_tmpdir))
    tmux = pick_tmux(environ)
    # The sweep names each socket with -S, so a server of the caller's cannot be
    # chosen by an inherited TMUX.
    ask = {
        name: value
        for name, value in binary_environment.items()
        if name not in ("TMUX", "TMUX_PANE")
    }

    left: list[str] = []
    stuck: list[str] = []
    unscanned = blind = False
    if tmux is not None:
        sockets, complete = find_sockets(sandbox)
        unscanned = not complete
        for socket in sockets:
            if not answers(tmux, socket, ask):
                continue
            name = socket.relative_to(sandbox).as_posix()
            left.append(name)
            if not stop(tmux, socket, ask):
                stuck.append(name)
    elif find_sockets(sandbox)[0]:
        # A socket nothing can be asked about may have a server behind it.
        blind = True

    # Nothing owns a directory that still holds a server, so it stays, and so
    # does one that could not be searched or asked about.
    leftover = False
    if not stuck and not unscanned and not blind:
        if sandbox.parent == parent and re.fullmatch(
            rf"{re.escape(PREFIX)}[A-Za-z0-9]{{6}}", sandbox.name
        ):
            shutil.rmtree(sandbox, ignore_errors=True)
        leftover = sandbox.exists()

    # Read, never touched: nothing says the server is this run's to stop.
    if outside is None:
        say("could not check for servers outside the sandbox")
        status = status or LEFT_A_SERVER
    elif outside:
        where = join(
            [
                "the tmux fallback directory"
                if root == fallback_root
                else "the caller's TMUX_TMPDIR"
                for root in outside
            ]
        )
        say(f"a tmux server started outside the sandbox, in{where}; not touched")
        status = status or LEFT_A_SERVER

    if unscanned:
        say("could not search the sandbox for sockets; the directory is kept")
        status = status or LEFT_A_SERVER
    elif blind:
        say("no tmux to ask about the sandbox sockets; the directory is kept")
        status = status or LEFT_A_SERVER
    elif stuck:
        say(f"could not stop the tmux server on socket(s):{join(stuck)}; the directory is kept")
        status = status or LEFT_A_SERVER
    elif leftover:
        say("could not remove the sandbox directory; it is kept")
        status = status or LEFT_A_SERVER
    elif left:
        say(f"the binary left a tmux server on socket(s):{join(left)}; stopped")
        status = status or LEFT_A_SERVER

    if interrupts.caught and status == 0:
        status = INTERRUPTED
    return status


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
