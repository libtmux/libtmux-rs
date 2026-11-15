"""The doctest sandbox check fails when a broken run passes, and only then."""

from __future__ import annotations

import sys
from pathlib import Path

import check_doctest_sandbox as check
import pytest

PYTHON = sys.executable


def fails_with(output: str) -> list[str]:
    script = f"import sys; print({output!r}); sys.exit(1)"
    return [PYTHON, "-c", script]


GOOD = "boom: TMUX is set\ntest result: FAILED. 0 passed; 1 failed"


def test_a_run_that_fails_the_right_way_passes() -> None:
    check.expect_failure(fails_with(GOOD), {}, "TMUX is set", "TMUX naming a server")


def test_a_run_that_passes_is_a_failure() -> None:
    with pytest.raises(check.CheckFailed, match="passed with a break"):
        check.expect_failure([PYTHON, "-c", "print(1)"], {}, "TMUX is set", "a break")


def test_a_run_that_fails_without_naming_what_is_wrong_is_a_failure() -> None:
    with pytest.raises(check.CheckFailed, match="did not fail on a break"):
        check.expect_failure(fails_with(GOOD), {}, "something else", "a break")


def test_a_run_that_fails_for_another_reason_is_a_failure() -> None:
    with pytest.raises(check.CheckFailed, match="did not fail on a break"):
        check.expect_failure(fails_with("TMUX is set"), {}, "TMUX is set", "a break")


def test_a_run_that_prints_the_decoy_is_a_failure() -> None:
    with pytest.raises(check.CheckFailed, match="printed the value"):
        check.expect_failure(
            fails_with(GOOD + "\n/nonexistent/decoy.sock"), {}, "TMUX is set", "a break"
        )


def test_the_tmux_that_lands_elsewhere_names_its_own_socket(tmp_path: Path) -> None:
    wrapper = check.write_tmux_elsewhere(tmp_path)
    text = wrapper.read_text(encoding="utf-8")
    assert f'-S "{tmp_path}/server.sock"' in text
    assert wrapper.stat().st_mode & 0o111
