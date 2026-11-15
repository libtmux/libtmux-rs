#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Run the doctest canary through the runner, through `[env]` alone, and with the
sandbox broken one way at a time.

    check_doctest_sandbox.py

Each broken run must fail, name what is wrong and print no value. The ambient
environment names a server throughout. Run from anywhere: the script changes to
the repository root.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from collections.abc import Mapping, Sequence
from pathlib import Path

import sandbox_outside
from sandbox_runner import restore_environment

ROOT = Path(__file__).resolve().parents[2]
REQUIRE_TESTS = ROOT / "scripts" / "require-tests" / "require_tests.py"
CANARY = ["cargo", "test", "--locked", "--package", "libtmux", "--doc", "SandboxCanary"]
DECOY = "/nonexistent/decoy.sock,1,0"
AMBIENT = {
    "TMUX": DECOY,
    "TMUX_PANE": "%9",
    "LIBTMUX_SOCKET": "decoy-name",
    "LIBTMUX_SOCKET_PATH": "/nonexistent/decoy-path.sock",
}
FAILED = "test result: FAILED. 0 passed; 1 failed"


class CheckFailed(Exception):
    """The sandbox check found something wrong; the message says what."""


def expect_failure(
    command: Sequence[str], environ: Mapping[str, str], expected: str, label: str
) -> None:
    """Run `command`; it must fail, say `expected`, and print no decoy value.

    >>> script = "print('boom: test result: FAILED. 0 passed; 1 failed'); raise SystemExit(1)"
    >>> expect_failure([sys.executable, "-c", script], {}, "boom", "a break")
    """
    result = subprocess.run(
        command,
        env=dict(environ),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        errors="replace",
        check=False,
    )
    log = result.stdout + result.stderr
    if result.returncode == 0:
        raise CheckFailed(f"the doctest canary passed with {label}")
    if expected not in log or FAILED not in log:
        raise CheckFailed(f"the doctest canary did not fail on {label}:\n{log}")
    if "decoy" in log:
        raise CheckFailed(f"the doctest canary printed the value it was given for {label}")


def host_triple() -> str:
    """The host triple, whose runner outranks the `cfg(unix)` one in the config file."""
    output = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    for line in output.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ")
    raise CheckFailed("rustc did not report a host triple")


def write_tmux_elsewhere(elsewhere: Path) -> Path:
    """A tmux whose servers listen on a socket of its own, so a bare client does
    not land on the default socket."""
    wrapper = elsewhere / "tmux"
    wrapper.write_text(
        f'#!/bin/sh\nexec tmux -S "{elsewhere}/server.sock" "$@"\n', encoding="utf-8"
    )
    wrapper.chmod(0o755)
    return wrapper


def stop_servers(elsewhere: Path, environ: Mapping[str, str]) -> None:
    for socket in (
        elsewhere / "server.sock",
        elsewhere / "tmpdir" / f"tmux-{os.getuid()}" / "default",
    ):
        subprocess.run(
            ["tmux", "-S", str(socket), "kill-server"],
            env=dict(environ),
            capture_output=True,
            check=False,
        )


def check(elsewhere: Path, environ: dict[str, str]) -> None:
    triple = host_triple()
    # Overriding the `cfg(unix)` entry itself does not always replace it.
    swap = ["--config", f"target.\"{triple}\".runner=['env']"]

    # A TMUX_TMPDIR that exists but is not the sandbox. One that does not exist
    # would send a bare client to /tmp/tmux-<uid>, the developer's own sockets.
    (elsewhere / "tmpdir").mkdir()
    if not sandbox_outside.make_reference(elsewhere / "reference"):
        raise CheckFailed("cannot write the reference file for the outside check")
    tmux = write_tmux_elsewhere(elsewhere)

    ambient = {**environ, **AMBIENT}
    for expects, extra in (("runner", []), ("env", swap)):
        run = subprocess.run(
            [sys.executable, str(REQUIRE_TESTS), *CANARY, *extra],
            env={**ambient, "LIBTMUX_DOCTEST_EXPECTS": expects},
            check=False,
        )
        if run.returncode != 0:
            raise CheckFailed(f"the doctest canary failed expecting {expects}")

    for variable in AMBIENT:
        expect_failure(
            [*CANARY, *swap, "--config", f'env.{variable}.value="{DECOY}"'],
            environ,
            f"{variable} is set",
            f"{variable} naming a server",
        )
    expect_failure(
        [
            *CANARY,
            *swap,
            "--config",
            f'env.TMUX_TMPDIR.value="{elsewhere / "tmpdir"}"',
            "--config",
            "env.TMUX_TMPDIR.relative=false",
        ],
        environ,
        "TMUX_TMPDIR is not LIBTMUX_TEST_SANDBOX",
        "a TMUX_TMPDIR that is not the sandbox",
    )
    expect_failure(
        [*CANARY, *swap],
        {**environ, "LIBTMUX_TEST_TMUX": str(tmux)},
        "missed the sandbox's default socket",
        "a tmux that lands elsewhere",
    )
    expect_failure(
        [*CANARY, *swap],
        {**environ, "LIBTMUX_DOCTEST_EXPECTS": "runner"},
        "the runner did not reach this doctest",
        "a doctest the runner did not reach",
    )
    expect_failure(
        CANARY,
        {**environ, "LIBTMUX_DOCTEST_EXPECTS": "env"},
        "and not the runner should have reached this doctest",
        "a doctest the runner did reach, where [env] was expected",
    )


def main() -> int:
    os.chdir(ROOT)
    environ = dict(os.environ)
    restore_environment(environ)
    elsewhere = Path(tempfile.mkdtemp())
    status = 0
    try:
        check(elsewhere, environ)
        print("the doctest canary passes in the sandbox and fails when it is broken")
    except CheckFailed as failure:
        print(failure, file=sys.stderr)
        status = 1
    finally:
        stop_servers(elsewhere, environ)
        # Read, never touched: only the servers this script started are stopped.
        outside = sandbox_outside.started_since(
            elsewhere / "reference", ["/tmp", environ.get("TMUX_TMPDIR", "")]
        )
        if outside is None:
            print("could not check for servers outside the sandbox", file=sys.stderr)
            status = 70
        elif outside:
            print(
                "a tmux server started outside the sandbox during this check; not touched",
                file=sys.stderr,
            )
            status = 70
        shutil.rmtree(elsewhere, ignore_errors=True)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
