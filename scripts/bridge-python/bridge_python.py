#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Name the Python backends the workspace bridge tests run against.

    bridge_python.py

Run it from the workspace root in the `python-bridge` CI job, after the job has
made `$RUNNER_TEMP/bridge-venv` and installed the obsolete tmuxp into it, and
after it checked out libtmux into `.bridge-python`. It writes
`$RUNNER_TEMP/bridge-python`, an executable wrapper that runs the venv's Python
with `.bridge-python/src` on `PYTHONPATH`, and appends two lines to $GITHUB_ENV:

    LIBTMUX_TEST_OBSOLETE_PYTHON   the venv's Python, whose libtmux is too old
    LIBTMUX_TEST_QUALIFIED_PYTHON  the wrapper, whose libtmux is the checked-out one

The wrapper is a shell script because an interpreter path cannot carry an
environment variable.
"""

from __future__ import annotations

import os
import pathlib
import shlex
import sys
from collections.abc import Mapping, Sequence


def wrapper_text(source: pathlib.Path, python: pathlib.Path) -> str:
    """The wrapper: `python` with `source` first on `PYTHONPATH`.

    >>> wrapper_text(pathlib.Path("/src"), pathlib.Path("/venv/bin/python"))
    '#!/bin/sh\\nPYTHONPATH=/src exec /venv/bin/python "$@"\\n'
    """
    return (
        f'#!/bin/sh\nPYTHONPATH={shlex.quote(str(source))} exec {shlex.quote(str(python))} "$@"\n'
    )


def environment_lines(python: pathlib.Path, wrapper: pathlib.Path) -> str:
    """The two lines that tell the tests where each backend is."""
    return f"LIBTMUX_TEST_OBSOLETE_PYTHON={python}\nLIBTMUX_TEST_QUALIFIED_PYTHON={wrapper}\n"


def main(argv: Sequence[str], environ: Mapping[str, str], cwd: pathlib.Path | None = None) -> int:
    if argv:
        sys.stderr.write("usage: bridge_python.py, which takes no argument\n")
        return 2
    root = pathlib.Path(environ.get("RUNNER_TEMP", ""))
    github_env = environ.get("GITHUB_ENV")
    if not environ.get("RUNNER_TEMP") or not github_env:
        sys.stderr.write("::error::bridge-python: RUNNER_TEMP and GITHUB_ENV must be set\n")
        return 1
    python = root / "bridge-venv/bin/python"
    source = (cwd or pathlib.Path.cwd()) / ".bridge-python/src"
    wrapper = root / "bridge-python"
    wrapper.write_text(wrapper_text(source, python), encoding="utf-8")
    wrapper.chmod(0o700)
    with open(github_env, "a", encoding="utf-8") as env:
        env.write(environment_lines(python, wrapper))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], os.environ))
