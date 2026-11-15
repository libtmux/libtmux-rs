"""The wrapper fails a command that ran no test, and only that one."""

from __future__ import annotations

import sys

import pytest
import require_tests

PYTHON = sys.executable


def command(output: str, status: int = 0) -> list[str]:
    return [PYTHON, "-c", f"import sys; print({output!r}); sys.exit({status})"]


def test_a_run_that_ran_tests_passes(capsys: pytest.CaptureFixture[str]) -> None:
    assert require_tests.main(command("running 2 tests")) == 0
    assert "running 2 tests" in capsys.readouterr().out


def test_a_filter_that_matched_nothing_fails(capsys: pytest.CaptureFixture[str]) -> None:
    assert require_tests.main(command("running 0 tests")) == 1
    assert "no test ran for:" in capsys.readouterr().err


def test_a_failing_command_keeps_its_own_status() -> None:
    assert require_tests.main(command("running 2 tests", status=3)) == 3


def test_a_missing_command_is_not_a_pass() -> None:
    assert require_tests.main(["/nonexistent/command"]) == 127


def test_no_command_is_a_usage_error() -> None:
    assert require_tests.main([]) == 2


def test_standard_error_counts_as_output(capsys: pytest.CaptureFixture[str]) -> None:
    script = "import sys; print('running 1 test', file=sys.stderr)"
    assert require_tests.main([PYTHON, "-c", script]) == 0
    assert "running 1 test" in capsys.readouterr().out


def test_the_uv_environment_is_removed() -> None:
    environ = {
        "PATH": "/venv/bin:/usr/bin",
        "VIRTUAL_ENV": "/venv",
        "UV": "/bin/uv",
        "UV_RUN_RECURSION_DEPTH": "1",
        "KEEP": "1",
    }
    require_tests.restore_environment(environ)
    assert environ == {"PATH": "/usr/bin", "KEEP": "1"}


def test_a_callers_own_virtual_environment_is_left_alone() -> None:
    environ = {"PATH": "/venv/bin:/usr/bin", "VIRTUAL_ENV": "/venv"}
    require_tests.restore_environment(environ)
    assert environ == {"PATH": "/venv/bin:/usr/bin", "VIRTUAL_ENV": "/venv"}
