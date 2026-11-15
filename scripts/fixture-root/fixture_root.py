#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Report whatever the test suite left in the fixture root.

    fixture_root.py [ROOT]          # default: /tmp/libtmux-rs-test

tmux never unlinks its socket when the server exits, so whatever named one owns
removing it. `TestServer` does; a test that hand-rolls a socket path does not,
and the file it leaves is invisible until the root fills up.

Ownership, not tidiness, is what this decides. A directory belonging to a run
in progress is not a leak even though it is new, and one belonging to a run
that is gone is a leak even though it is old, so it asks who owns a thing
rather than what it is called or how old it is.

The `owner` file holds the PID that made the fixture, and a living owner means
skip. A socket that a server still answers on is in use. Reaping by name, by
age, or by "is a server answering here" alone inverts that: aliveness is the
reason to leave a fixture alone, and using it as the reason to remove one
destroys the fixtures of whatever is running now. That failure does not look
like a leak. It looks like someone else's test failing somewhere unrelated,
with a message pointing at their code, which is what happened on 2026-08-24,
when a second agent reaped four servers here and two `tmux-mcp` waiting tests
failed reporting a closed pane.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

DEFAULT_ROOT = Path("/tmp/libtmux-rs-test")


def pid_alive(pid: int) -> bool:
    """Whether a process with this id exists, even one this user cannot signal.

    >>> pid_alive(os.getpid())
    True
    """
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def server_answers(socket: Path) -> bool:
    """Whether a tmux server listens on `socket`."""
    try:
        result = subprocess.run(
            ["tmux", "-S", str(socket), "list-sessions"],
            capture_output=True,
            stdin=subprocess.DEVNULL,
            check=False,
        )
    except OSError:
        return False
    return result.returncode == 0


def abandoned(root: Path) -> list[Path]:
    """The entries of `root` nobody is using.

    >>> abandoned(Path("/nonexistent/fixture-root"))
    []
    """
    if not root.is_dir():
        return []
    left = []
    for entry in sorted(root.iterdir()):
        owner = entry / "owner"
        if owner.is_file():
            try:
                if pid_alive(int(owner.read_text().strip())):
                    continue
            except ValueError:
                pass
        if entry.is_socket() and server_answers(entry):
            continue
        left.append(entry)
    return left


def main(arguments: list[str]) -> int:
    root = Path(arguments[0]) if arguments else DEFAULT_ROOT
    if not root.is_dir():
        print("fixture root absent")
        return 0
    left = abandoned(root)
    if not left:
        print("fixture root clean")
        return 0
    print("the fixture root still holds:", file=sys.stderr)
    for entry in left:
        print(f"  {entry.relative_to(root)}", file=sys.stderr)
    print(
        "\ntmux does not unlink its socket when the server exits, so a test that\n"
        "names its own socket path has to remove it. libtmux::test::TestServer\n"
        "owns that lifecycle; prefer it over Server::builder().socket_path().",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
