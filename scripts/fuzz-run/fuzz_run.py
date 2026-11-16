#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Fuzz one cargo-fuzz target for the time a CI job allows.

    fuzz_run.py TARGET

Run it from the workspace root, with the nightly toolchain installed.
cargo-fuzz builds for the triple it was itself compiled for, and the installer
ships a musl binary. musl links libc statically, which a sanitizer cannot
instrument, so the nightly host triple is named here rather than left to how
cargo-fuzz arrived. libFuzzer writes new inputs to the first corpus directory
only, so `fuzz/corpus/TARGET` comes first and the checked-in seeds second.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys
from collections.abc import Callable, Sequence

TARGET = re.compile(r"[a-z][a-z0-9_]*")
MAX_TOTAL_TIME = 300
RSS_LIMIT_MB = 4096


class FuzzError(Exception):
    """The target cannot be fuzzed."""


def host_triple(version_output: str) -> str:
    """The `host:` line of `rustc -vV`.

    >>> host_triple("rustc 1.99.0-nightly\\nhost: x86_64-unknown-linux-gnu\\nrelease: 1.99.0")
    'x86_64-unknown-linux-gnu'
    """
    for line in version_output.splitlines():
        if line.startswith("host: "):
            return line.removeprefix("host: ").strip()
    raise FuzzError("rustc -vV names no host triple")


def fuzz_command(host: str, target: str) -> list[str]:
    """The `cargo fuzz run` command for one target."""
    return [
        "cargo",
        "+nightly",
        "fuzz",
        "run",
        "--target",
        host,
        target,
        f"fuzz/corpus/{target}",
        f"fuzz/seeds/{target}",
        "--",
        f"-max_total_time={MAX_TOTAL_TIME}",
        f"-rss_limit_mb={RSS_LIMIT_MB}",
    ]


def main(
    argv: Sequence[str],
    run: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    root: pathlib.Path | None = None,
) -> int:
    if len(argv) != 1 or not TARGET.fullmatch(argv[0]):
        sys.stderr.write("usage: fuzz_run.py TARGET, a fuzz target name such as layout\n")
        return 2
    target = argv[0]
    try:
        version = run(["rustc", "+nightly", "-vV"], capture_output=True, text=True, check=False)
        if version.returncode != 0:
            raise FuzzError(f"rustc +nightly -vV failed: {version.stderr.strip()}")
        host = host_triple(version.stdout)
    except FuzzError as error:
        sys.stderr.write(f"::error::fuzz-run: {error}\n")
        return 1
    (root or pathlib.Path.cwd()).joinpath("fuzz", "corpus", target).mkdir(
        parents=True, exist_ok=True
    )
    return run(fuzz_command(host, target), check=False).returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
