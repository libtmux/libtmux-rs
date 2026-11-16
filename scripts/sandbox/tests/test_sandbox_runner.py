"""The runner's decisions, without a tmux server. The Rust test `sandbox_runner` runs it with one."""

from __future__ import annotations

import os
import signal
import socket
import stat
import sys
from pathlib import Path

import pytest
import sandbox_runner

PYTHON = sys.executable
RUNNING_AS_ROOT = os.getuid() == 0


def run(tmp_path: Path, code: str, **extra: str) -> int:
    """Run `python -c code` under the runner with no tmux on PATH; its status."""
    environ = {"TMPDIR": str(tmp_path), "PATH": "", **extra}
    return sandbox_runner.main([PYTHON, "-c", code], environ)


def sandboxes(tmp_path: Path) -> list[Path]:
    return sorted(tmp_path.glob("ltrs.*"))


def test_a_clean_run_passes_the_status_through_and_removes_its_directory(tmp_path: Path) -> None:
    assert run(tmp_path, "raise SystemExit(0)") == 0
    assert run(tmp_path, "raise SystemExit(3)") == 3
    assert sandboxes(tmp_path) == []


def test_the_binary_sees_the_sandbox_and_no_server(tmp_path: Path) -> None:
    code = (
        "import os, pathlib;"
        "names = ['TMUX', 'TMUX_PANE', 'LIBTMUX_SOCKET', 'LIBTMUX_SOCKET_PATH',"
        " 'LIBTMUX_SANDBOX_TMUX', 'LIBTMUX_SANDBOX_FALLBACK_ROOT'];"
        "assert not [name for name in names if name in os.environ], os.environ;"
        "directory = pathlib.Path(os.environ['TMUX_TMPDIR']);"
        "assert directory.is_dir() and directory.name.startswith('ltrs.');"
        "assert os.environ['LIBTMUX_TEST_SANDBOX'] == str(directory);"
        "assert oct(directory.stat().st_mode & 0o777) == '0o700'"
    )
    ambient = {
        "TMUX": "decoy",
        "TMUX_PANE": "%1",
        "LIBTMUX_SOCKET": "decoy",
        "LIBTMUX_SOCKET_PATH": "decoy",
    }
    assert run(tmp_path, code, **ambient) == 0


def test_a_named_tmux_is_handed_on_and_announced(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = "import os; raise SystemExit(0 if os.environ['TMUX'] == 'named' else 1)"
    status = run(tmp_path, code, LIBTMUX_SANDBOX_TMUX="named", TMUX="ambient")
    assert status == 0
    assert "LIBTMUX_SANDBOX_TMUX is set: true" in capsys.readouterr().err


@pytest.mark.parametrize(
    ("tmpdir", "message"),
    [
        ("relative", "TMPDIR is not an absolute path"),
        ("/nonexistent/directory", "TMPDIR is not a directory"),
    ],
)
def test_a_bad_tmpdir_is_refused_before_the_binary_runs(
    tmp_path: Path, capsys: pytest.CaptureFixture[str], tmpdir: str, message: str
) -> None:
    marker = tmp_path / "ran"
    code = f"open({str(marker)!r}, 'w')"
    status = sandbox_runner.main([PYTHON, "-c", code], {"TMPDIR": tmpdir})
    assert status == sandbox_runner.REFUSED
    assert message in capsys.readouterr().err
    assert not marker.exists()


def test_a_tmpdir_too_long_for_a_socket_is_refused(tmp_path: Path) -> None:
    long = tmp_path
    while len(os.fsencode(str(long))) < 100:
        long = long / ("x" * 20)
        long.mkdir(exist_ok=True)
    with pytest.raises(sandbox_runner.Refused, match="too long"):
        sandbox_runner.choose_parent({"TMPDIR": str(long)})


def test_no_binary_is_refused() -> None:
    assert sandbox_runner.main([], {}) == sandbox_runner.REFUSED


def test_a_socket_left_with_no_tmux_to_ask_keeps_the_directory(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = (
        "import os, socket;"
        "s = socket.socket(socket.AF_UNIX);"
        "s.bind(os.environ['TMUX_TMPDIR'] + '/left.sock')"
    )
    status = run(tmp_path, code)
    assert status == sandbox_runner.LEFT_A_SERVER
    err = capsys.readouterr().err
    assert "no tmux to ask" in err
    assert "directory is kept" in err
    assert len(sandboxes(tmp_path)) == 1


@pytest.mark.skipif(RUNNING_AS_ROOT, reason="root searches and removes what it likes")
def test_a_directory_it_cannot_search_is_kept(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    code = "import os;d = os.environ['TMUX_TMPDIR'] + '/locked'; os.mkdir(d); os.chmod(d, 0)"
    # With a tmux to ask the search is what fails; here there is none, so the
    # directory is kept for the other reason. Either way it is kept and exits 70.
    status = run(tmp_path, code)
    for directory in sandboxes(tmp_path):
        (directory / "locked").chmod(0o700)
    assert status == sandbox_runner.LEFT_A_SERVER
    assert "ltrs." not in capsys.readouterr().err


def test_an_interrupt_the_binary_survives_exits_130(tmp_path: Path) -> None:
    code = "import os, signal; os.kill(os.getppid(), signal.SIGINT)"
    assert run(tmp_path, code) == sandbox_runner.INTERRUPTED
    assert sandboxes(tmp_path) == []


def test_a_failed_binary_keeps_its_status_when_interrupted(tmp_path: Path) -> None:
    code = "import os, signal; os.kill(os.getppid(), signal.SIGTERM); raise SystemExit(4)"
    assert run(tmp_path, code) == 4


def test_the_handlers_are_released_afterwards(tmp_path: Path) -> None:
    before = signal.getsignal(signal.SIGINT)
    run(tmp_path, "raise SystemExit(0)")
    assert signal.getsignal(signal.SIGINT) in (signal.SIG_DFL, before)


def test_a_missing_binary_is_not_found(tmp_path: Path) -> None:
    status = sandbox_runner.main(["/nonexistent/binary"], {"TMPDIR": str(tmp_path), "PATH": ""})
    assert status == 127


def test_a_binary_killed_by_a_signal_reports_128_plus_the_number(tmp_path: Path) -> None:
    code = "import os, signal; os.kill(os.getpid(), signal.SIGKILL)"
    assert run(tmp_path, code) == 128 + signal.SIGKILL


def test_find_sockets_does_not_follow_links(tmp_path: Path) -> None:
    (tmp_path / "a" / "b").mkdir(parents=True)
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(tmp_path / "a" / "b" / "deep.sock"))
    (tmp_path / "link.sock").symlink_to(tmp_path / "a" / "b" / "deep.sock")
    (tmp_path / "link-dir").symlink_to(tmp_path / "a")
    sockets, complete = sandbox_runner.find_sockets(tmp_path)
    listener.close()
    assert sockets == [tmp_path / "a" / "b" / "deep.sock"]
    assert complete


def test_the_messages_list_sockets_after_a_colon() -> None:
    assert sandbox_runner.join(["default"]) == " default"
    assert sandbox_runner.join(["default", "a/b/deep.sock"]) == " default, a/b/deep.sock"


def test_a_sandbox_is_owner_only_and_named_for_the_runner(tmp_path: Path) -> None:
    sandbox = sandbox_runner.make_sandbox(tmp_path)
    assert sandbox.parent == tmp_path
    assert len(sandbox.name) == len("ltrs.XXXXXX")
    assert stat.S_IMODE(sandbox.stat().st_mode) == 0o700


def test_a_signal_ignored_at_start_stays_ignored() -> None:
    previous = signal.signal(signal.SIGHUP, signal.SIG_IGN)
    try:
        with sandbox_runner.Interrupts():
            assert signal.getsignal(signal.SIGHUP) is signal.SIG_IGN
    finally:
        signal.signal(signal.SIGHUP, previous)


@pytest.fixture
def wedged_tmux(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> str:
    """A `tmux` that never answers, with the runner's deadline cut to a tenth of a second."""
    program = tmp_path / "tmux"
    program.write_text(f"#!{PYTHON}\nimport time\ntime.sleep(30)\n")
    program.chmod(0o700)
    monkeypatch.setattr(sandbox_runner, "TMUX_SECONDS", 0.1)
    monkeypatch.setattr(sandbox_runner, "STOP_TRIES", 2)
    monkeypatch.setattr(sandbox_runner, "STOP_PAUSE", 0.0)
    return str(program)


def test_a_server_that_never_answers_still_counts_as_listening(
    tmp_path: Path, wedged_tmux: str
) -> None:
    assert sandbox_runner.answers(wedged_tmux, tmp_path / "s", {})


def test_stopping_a_wedged_server_gives_up_instead_of_hanging(
    tmp_path: Path, wedged_tmux: str
) -> None:
    assert not sandbox_runner.stop(wedged_tmux, tmp_path / "s", {})
