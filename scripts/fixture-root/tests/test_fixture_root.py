"""A fixture a living run owns is left alone, and one nobody owns is reported."""

from __future__ import annotations

import os
import shutil
import socket
import subprocess
import sys
from pathlib import Path

import fixture_root
import pytest


def dead_pid() -> int:
    child = subprocess.Popen([sys.executable, "-c", "pass"])
    child.wait()
    return child.pid


def test_a_directory_owned_by_a_living_run_is_not_a_leak(tmp_path: Path) -> None:
    (tmp_path / "now").mkdir()
    (tmp_path / "now" / "owner").write_text(f"{os.getpid()}\n")
    assert fixture_root.abandoned(tmp_path) == []


def test_a_directory_owned_by_a_run_that_is_gone_is_a_leak(tmp_path: Path) -> None:
    (tmp_path / "old").mkdir()
    (tmp_path / "old" / "owner").write_text(f"{dead_pid()}\n")
    assert fixture_root.abandoned(tmp_path) == [tmp_path / "old"]


def test_an_owner_file_that_names_no_process_is_a_leak(tmp_path: Path) -> None:
    (tmp_path / "odd").mkdir()
    (tmp_path / "odd" / "owner").write_text("not a pid\n")
    assert fixture_root.abandoned(tmp_path) == [tmp_path / "odd"]


def test_a_file_with_no_owner_and_no_server_is_a_leak(tmp_path: Path) -> None:
    (tmp_path / "stray").write_text("")
    assert fixture_root.abandoned(tmp_path) == [tmp_path / "stray"]


def test_a_socket_nothing_answers_on_is_a_leak(tmp_path: Path) -> None:
    path = tmp_path / "stale.sock"
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(path))
    listener.close()
    assert path.is_socket()
    assert fixture_root.abandoned(tmp_path) == [path]


@pytest.mark.skipif(shutil.which("tmux") is None, reason="needs tmux")
def test_a_socket_a_server_answers_on_is_in_use(tmp_path: Path) -> None:
    path = tmp_path / "live.sock"
    env = {**os.environ, "TMUX": "", "SHELL": "/bin/sh"}
    subprocess.run(
        ["tmux", "-f", "/dev/null", "-S", str(path), "new-session", "-d"],
        check=True,
        env=env,
        stdin=subprocess.DEVNULL,
    )
    try:
        assert fixture_root.abandoned(tmp_path) == []
    finally:
        subprocess.run(["tmux", "-S", str(path), "kill-server"], check=False, capture_output=True)


def test_an_absent_root_leaks_nothing_and_says_so(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert fixture_root.main([str(tmp_path / "missing")]) == 0
    assert capsys.readouterr().out == "fixture root absent\n"


def test_a_clean_root_passes(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    assert fixture_root.main([str(tmp_path)]) == 0
    assert capsys.readouterr().out == "fixture root clean\n"


def test_a_leak_fails_and_names_what_is_left(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    (tmp_path / "stray").write_text("")
    assert fixture_root.main([str(tmp_path)]) == 1
    err = capsys.readouterr().err
    assert "the fixture root still holds:\n  stray\n" in err
    assert "TestServer" in err
