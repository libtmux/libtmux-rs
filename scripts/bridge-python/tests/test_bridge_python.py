"""The wrapper puts the checked-out libtmux ahead of the venv's, and the tests learn both paths."""

from __future__ import annotations

import pathlib
import subprocess

import bridge_python
import pytest


def test_the_wrapper_sets_the_path_and_runs_the_venv_python() -> None:
    text = bridge_python.wrapper_text(
        pathlib.Path("/work/.bridge-python/src"), pathlib.Path("/tmp/venv/bin/python")
    )
    assert text == '#!/bin/sh\nPYTHONPATH=/work/.bridge-python/src exec /tmp/venv/bin/python "$@"\n'


def test_a_path_with_a_space_is_quoted() -> None:
    text = bridge_python.wrapper_text(pathlib.Path("/a b/src"), pathlib.Path("/c d/python"))
    assert "PYTHONPATH='/a b/src' exec '/c d/python'" in text


def test_main_writes_an_executable_wrapper_and_the_environment(tmp_path: pathlib.Path) -> None:
    runner = tmp_path / "runner"
    runner.mkdir()
    github_env = tmp_path / "env"
    github_env.write_text("EXISTING=1\n", encoding="utf-8")
    status = bridge_python.main(
        [], {"RUNNER_TEMP": str(runner), "GITHUB_ENV": str(github_env)}, cwd=tmp_path
    )
    assert status == 0
    wrapper = runner / "bridge-python"
    assert wrapper.stat().st_mode & 0o777 == 0o700
    assert github_env.read_text(encoding="utf-8") == (
        "EXISTING=1\n"
        f"LIBTMUX_TEST_OBSOLETE_PYTHON={runner}/bridge-venv/bin/python\n"
        f"LIBTMUX_TEST_QUALIFIED_PYTHON={wrapper}\n"
    )


def test_the_wrapper_runs_its_python_with_the_source_on_the_path(tmp_path: pathlib.Path) -> None:
    source = tmp_path / ".bridge-python" / "src"
    source.mkdir(parents=True)
    runner = tmp_path / "runner"
    (runner / "bridge-venv" / "bin").mkdir(parents=True)
    (runner / "bridge-venv" / "bin" / "python").symlink_to(
        pathlib.Path(__import__("sys").executable)
    )
    github_env = tmp_path / "env"
    assert (
        bridge_python.main(
            [], {"RUNNER_TEMP": str(runner), "GITHUB_ENV": str(github_env)}, cwd=tmp_path
        )
        == 0
    )
    result = subprocess.run(
        [str(runner / "bridge-python"), "-c", "import sys; print(sys.path[1])"],
        capture_output=True,
        text=True,
        check=True,
    )
    assert result.stdout.strip() == str(source)


@pytest.mark.parametrize("environ", [{}, {"RUNNER_TEMP": "/tmp"}, {"GITHUB_ENV": "/tmp/env"}])
def test_missing_variables_fail(
    environ: dict[str, str], capsys: pytest.CaptureFixture[str]
) -> None:
    assert bridge_python.main([], environ) == 1
    assert "RUNNER_TEMP and GITHUB_ENV must be set" in capsys.readouterr().err


def test_an_argument_is_a_usage_error() -> None:
    assert bridge_python.main(["x"], {}) == 2
