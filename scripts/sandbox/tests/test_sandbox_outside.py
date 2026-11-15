"""The outside check names a socket made after its reference, and no other."""

from __future__ import annotations

import os
import socket
from pathlib import Path

import sandbox_outside


def stand_in_root(parent: Path, name: str) -> tuple[Path, Path]:
    """A directory standing in for one tmux would use, and the socket a bare client binds."""
    root = parent / name
    per_user = root / f"tmux-{os.getuid()}"
    per_user.mkdir(parents=True, mode=0o700)
    return root, per_user / "default"


def listen(path: Path) -> socket.socket:
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(path))
    return listener


def test_it_fails_closed_without_a_reference(tmp_path: Path) -> None:
    root, _ = stand_in_root(tmp_path, "caller")
    assert sandbox_outside.started_since(tmp_path / "no-reference", [str(root)]) is None


def test_it_names_only_a_socket_made_after_the_reference(tmp_path: Path) -> None:
    root, caller_socket = stand_in_root(tmp_path, "caller")
    other, other_socket = stand_in_root(tmp_path, "other")
    reference = tmp_path / "reference"
    with listen(other_socket):
        assert sandbox_outside.make_reference(reference)
        with listen(caller_socket):
            named = sandbox_outside.started_since(reference, [str(root), str(other), ""])
    assert named == [str(root)]


def test_a_stale_file_that_is_not_a_socket_is_not_named(tmp_path: Path) -> None:
    root, default = stand_in_root(tmp_path, "caller")
    reference = tmp_path / "reference"
    assert sandbox_outside.make_reference(reference)
    default.write_text("not a socket")
    assert sandbox_outside.started_since(reference, [str(root)]) == []


def test_the_reference_is_older_than_whatever_is_made_next(tmp_path: Path) -> None:
    reference = tmp_path / "reference"
    following = tmp_path / "next"
    for _ in range(50):
        assert sandbox_outside.make_reference(reference)
        following.touch()
        assert following.stat().st_mtime_ns > reference.stat().st_mtime_ns


def test_a_reference_that_cannot_be_written_is_refused(tmp_path: Path) -> None:
    assert not sandbox_outside.make_reference(tmp_path / "missing" / "reference")
