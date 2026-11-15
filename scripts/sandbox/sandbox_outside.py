"""Look at the default sockets a bare tmux client would reach outside the sandbox.

Imported by `sandbox_runner.py` and `check_doctest_sandbox.py`. It never
connects to a socket, signals a server or removes a file.
"""

from __future__ import annotations

import os
import time
from collections.abc import Iterable
from pathlib import Path

# How long `make_reference` waits for the clock to pass the reference file: 100
# tries, 10 ms apart.
REFERENCE_TRIES = 100
REFERENCE_PAUSE = 0.01


def make_reference(reference: Path) -> bool:
    """Make `reference`, and wait until the clock is past it.

    A socket made after this returns has a modification time later than the
    reference. A file system that stamps coarsely makes this wait for the next
    tick, which is why it polls.

    >>> import tempfile
    >>> with tempfile.TemporaryDirectory() as directory:
    ...     make_reference(Path(directory) / "reference")
    True
    """
    tick = reference.with_name(reference.name + ".tick")
    try:
        reference.touch()
        reference_time = reference.stat().st_mtime_ns
        for _ in range(REFERENCE_TRIES):
            tick.touch()
            if tick.stat().st_mtime_ns > reference_time:
                return True
            time.sleep(REFERENCE_PAUSE)
    except OSError:
        return False
    return False


def started_since(reference: Path, roots: Iterable[str]) -> list[str] | None:
    """The roots whose default socket is newer than `reference`, whoever made it.

    tmux puts a bare client's server at `<root>/tmux-<uid>/default`. Returns
    `None` when there is no reference to compare with, so the caller can fail
    closed rather than report nothing.

    >>> started_since(Path("/nonexistent/reference"), ["/tmp"]) is None
    True
    """
    try:
        reference_time = reference.stat().st_mtime_ns
    except OSError:
        return None
    uid = os.getuid()
    named: list[str] = []
    for root in roots:
        if not root:
            continue
        base = root.removesuffix("/")
        socket = Path(f"{base}/tmux-{uid}/default")
        try:
            made = socket.stat()
        except OSError:
            continue
        if socket.is_socket() and made.st_mtime_ns > reference_time:
            named.append(root)
    return named
