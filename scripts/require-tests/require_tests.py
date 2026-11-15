#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Run a test command and fail when it ran no test.

    require_tests.py COMMAND [ARGUMENT ...]

A `cargo test` name filter that matches nothing exits 0 with "running 0
tests", so a renamed test would leave its lane green without running anything.
Pass the whole command: `require_tests.py cargo test --package libtmux ...`.
The command's output reaches this script's output as it is written. Its exit
status is returned when it is not zero, and 1 when it passes without a test.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from collections.abc import MutableMapping, Sequence

RAN_A_TEST = re.compile(r"^running [1-9][0-9]* tests?$", re.MULTILINE)


def ran_a_test(log: str) -> bool:
    """Whether the output of a test run says that at least one test ran.

    >>> ran_a_test("running 0 tests\\n")
    False
    >>> ran_a_test("running 3 tests\\n")
    True
    >>> ran_a_test("running 1 test\\n")
    True
    """
    return RAN_A_TEST.search(log) is not None


def restore_environment(environ: MutableMapping[str, str]) -> None:
    """Remove what `uv run` adds, so the command sees the caller's environment.

    uv puts the script's own environment first in `PATH` and exports
    `VIRTUAL_ENV`, `UV` and `UV_RUN_RECURSION_DEPTH`. A test that inspects its
    environment should not see them.
    """
    if "UV_RUN_RECURSION_DEPTH" not in environ:
        return
    virtual_env = environ.pop("VIRTUAL_ENV", None)
    environ.pop("UV", None)
    environ.pop("UV_RUN_RECURSION_DEPTH", None)
    if virtual_env is None:
        return
    entries = environ.get("PATH", "").split(os.pathsep)
    if entries and entries[0] == os.path.join(virtual_env, "bin"):
        environ["PATH"] = os.pathsep.join(entries[1:])


def run(command: Sequence[str], environ: MutableMapping[str, str]) -> int:
    """Run `command`, echo its output, and judge whether a test ran."""
    log: list[str] = []
    with subprocess.Popen(
        command,
        env=dict(environ),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
    ) as child:
        assert child.stdout is not None
        for line in child.stdout:
            sys.stdout.write(line)
            sys.stdout.flush()
            log.append(line)
    if child.returncode != 0:
        return child.returncode
    if not ran_a_test("".join(log)):
        print(f"no test ran for: {' '.join(command)}", file=sys.stderr)
        return 1
    return 0


def main(argv: Sequence[str]) -> int:
    if not argv:
        print("usage: require_tests.py COMMAND [ARGUMENT ...]", file=sys.stderr)
        return 2
    environ = dict(os.environ)
    restore_environment(environ)
    try:
        return run(argv, environ)
    except FileNotFoundError as error:
        print(f"require_tests.py: {error}", file=sys.stderr)
        return 127


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
