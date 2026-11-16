"""The fuzz command names the nightly host and puts the writable corpus first."""

from __future__ import annotations

import pathlib
import subprocess
from typing import Any

import fuzz_run
import pytest

HOST = "x86_64-unknown-linux-gnu"
VERSION_OUTPUT = (
    f"rustc 1.99.0-nightly (abc 2026-11-01)\nbinary: rustc\nhost: {HOST}\nrelease: 1.99.0-nightly\n"
)


class Recorder:
    """Stands in for `subprocess.run` and records each command."""

    def __init__(self, version: subprocess.CompletedProcess[str], fuzz_status: int = 0) -> None:
        self.version = version
        self.fuzz_status = fuzz_status
        self.commands: list[list[str]] = []

    def __call__(self, command: list[str], **_: Any) -> subprocess.CompletedProcess[str]:
        self.commands.append(command)
        if command[0] == "rustc":
            return self.version
        return subprocess.CompletedProcess(command, self.fuzz_status)


def rustc(stdout: str, status: int = 0) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(["rustc"], status, stdout=stdout, stderr="")


def test_the_host_triple_is_the_host_line() -> None:
    assert fuzz_run.host_triple(VERSION_OUTPUT) == HOST


def test_output_without_a_host_line_is_refused() -> None:
    with pytest.raises(fuzz_run.FuzzError, match="no host triple"):
        fuzz_run.host_triple("rustc 1.99.0\nrelease: 1.99.0\n")


def test_the_command_names_the_host_and_lists_the_writable_corpus_first() -> None:
    command = fuzz_run.fuzz_command(HOST, "layout")
    assert command[:8] == [
        "cargo",
        "+nightly",
        "fuzz",
        "run",
        "--target",
        HOST,
        "layout",
        "fuzz/corpus/layout",
    ]
    assert command[8:] == ["fuzz/seeds/layout", "--", "-max_total_time=300", "-rss_limit_mb=4096"]


def test_main_makes_the_corpus_and_returns_the_fuzzer_status(tmp_path: pathlib.Path) -> None:
    recorder = Recorder(rustc(VERSION_OUTPUT), fuzz_status=77)
    assert fuzz_run.main(["layout"], run=recorder, root=tmp_path) == 77
    assert (tmp_path / "fuzz" / "corpus" / "layout").is_dir()
    assert recorder.commands[-1] == fuzz_run.fuzz_command(HOST, "layout")


def test_a_failing_rustc_stops_before_the_fuzzer(tmp_path: pathlib.Path) -> None:
    recorder = Recorder(rustc("", status=1))
    assert fuzz_run.main(["layout"], run=recorder, root=tmp_path) == 1
    assert [command[0] for command in recorder.commands] == ["rustc"]


@pytest.mark.parametrize("argv", [[], ["a", "b"], ["../escape"], ["--help"], ["Upper"]])
def test_a_bad_target_is_a_usage_error(argv: list[str], tmp_path: pathlib.Path) -> None:
    recorder = Recorder(rustc(VERSION_OUTPUT))
    assert fuzz_run.main(argv, run=recorder, root=tmp_path) == 2
    assert recorder.commands == []
