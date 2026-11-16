"""A tag must name a workspace crate at its manifest version, and the attach step retries safely."""

from __future__ import annotations

import json
import pathlib
import subprocess
from typing import Any

import pytest
import release_crate

METADATA = json.dumps(
    {
        "packages": [
            {"name": "libtmux", "version": "0.1.0"},
            {"name": "tmux-mcp", "version": "0.2.0-alpha.1"},
        ]
    }
)


class Recorder:
    """Stands in for `subprocess.run`: answers cargo with METADATA and records the rest."""

    def __init__(
        self, view_status: int = 0, metadata_status: int = 0, upload_status: int = 0
    ) -> None:
        self.view_status = view_status
        self.metadata_status = metadata_status
        self.upload_status = upload_status
        self.commands: list[list[str]] = []

    def __call__(self, command: list[str], **_: Any) -> subprocess.CompletedProcess[str]:
        self.commands.append(command)
        if command[0] == "cargo":
            return subprocess.CompletedProcess(
                command, self.metadata_status, METADATA, "no manifest"
            )
        if command[:3] == ["gh", "release", "view"]:
            return subprocess.CompletedProcess(command, self.view_status, "", "")
        return subprocess.CompletedProcess(command, self.upload_status, "", "")


def test_a_tag_splits_at_its_last_marker() -> None:
    assert release_crate.parse_tag("tmux-mcp@v0.2.0-alpha.1") == release_crate.Target(
        "tmux-mcp", "0.2.0-alpha.1"
    )
    assert release_crate.parse_tag("odd@vname@v1.0.0") == release_crate.Target("odd@vname", "1.0.0")


@pytest.mark.parametrize("tag", ["libtmux", "libtmux-v0.1.0", "v0.1.0"])
def test_a_tag_without_the_marker_is_refused(tag: str) -> None:
    with pytest.raises(release_crate.ReleaseError, match="is not <crate>@v<version>"):
        release_crate.parse_tag(tag)


def test_the_manifest_must_carry_the_crate_at_the_tagged_version() -> None:
    release_crate.check_manifest(release_crate.Target("libtmux", "0.1.0"), METADATA)
    with pytest.raises(release_crate.ReleaseError, match="no workspace member is called nothing"):
        release_crate.check_manifest(release_crate.Target("nothing", "0.1.0"), METADATA)
    with pytest.raises(
        release_crate.ReleaseError,
        match=r"libtmux is 0\.1\.0 in the manifest, but the tag says 0\.2\.0",
    ):
        release_crate.check_manifest(release_crate.Target("libtmux", "0.2.0"), METADATA)


def test_target_writes_the_outputs(tmp_path: pathlib.Path) -> None:
    output = tmp_path / "output"
    status = release_crate.main(
        ["target", "libtmux@v0.1.0"], {"GITHUB_OUTPUT": str(output)}, Recorder()
    )
    assert status == 0
    assert output.read_text(encoding="utf-8") == (
        "package=libtmux\nversion=0.1.0\ncrate_file=target/package/libtmux-0.1.0.crate\n"
    )


def test_target_writes_nothing_for_a_refused_tag(
    tmp_path: pathlib.Path, capsys: pytest.CaptureFixture[str]
) -> None:
    output = tmp_path / "output"
    status = release_crate.main(
        ["target", "libtmux@v9.9.9"], {"GITHUB_OUTPUT": str(output)}, Recorder()
    )
    assert status == 1
    assert not output.exists()
    assert "the tag says 9.9.9" in capsys.readouterr().err


def test_a_cargo_failure_is_not_a_pass(capsys: pytest.CaptureFixture[str]) -> None:
    assert release_crate.main(["target", "libtmux@v0.1.0"], {}, Recorder(metadata_status=101)) == 1
    assert "cargo metadata failed" in capsys.readouterr().err


def test_an_existing_release_gets_the_crate_uploaded() -> None:
    recorder = Recorder(view_status=0)
    args = ["attach", "libtmux@v0.1.0", "libtmux", "0.1.0", "target/package/libtmux-0.1.0.crate"]
    assert release_crate.main(args, {"GITHUB_REPOSITORY": "o/r"}, recorder) == 0
    assert recorder.commands[-1] == [
        "gh",
        "release",
        "upload",
        "libtmux@v0.1.0",
        "target/package/libtmux-0.1.0.crate",
        "--repo",
        "o/r",
        "--clobber",
    ]


def test_a_missing_release_is_created_with_the_crate() -> None:
    recorder = Recorder(view_status=1)
    args = ["attach", "libtmux@v0.1.0", "libtmux", "0.1.0", "target/package/libtmux-0.1.0.crate"]
    assert release_crate.main(args, {"GITHUB_REPOSITORY": "o/r"}, recorder) == 0
    command = recorder.commands[-1]
    assert command[:4] == ["gh", "release", "create", "libtmux@v0.1.0"]
    assert "--prerelease" not in command
    assert command[command.index("--title") + 1] == "libtmux 0.1.0"
    assert command[command.index("--notes") + 1] == (
        "`libtmux` 0.1.0. See crates/libtmux/CHANGELOG.md for what changed."
    )
    assert command[-2:] == ["--verify-tag", "target/package/libtmux-0.1.0.crate"]


def test_a_prerelease_version_makes_a_prerelease() -> None:
    target = release_crate.Target("tmux-mcp", "0.2.0-alpha.1")
    command = release_crate.release_commands(
        "tmux-mcp@v0.2.0-alpha.1", target, "c.crate", "o/r", exists=False
    )
    assert "--prerelease" in command
    assert command[command.index("--notes") + 1].endswith(" This is a prerelease.")


def test_the_upload_status_is_the_commands() -> None:
    args = ["attach", "libtmux@v0.1.0", "libtmux", "0.1.0", "c.crate"]
    assert release_crate.main(args, {"GITHUB_REPOSITORY": "o/r"}, Recorder(upload_status=3)) == 3


def test_attach_needs_the_repository(capsys: pytest.CaptureFixture[str]) -> None:
    args = ["attach", "libtmux@v0.1.0", "libtmux", "0.1.0", "c.crate"]
    assert release_crate.main(args, {}, Recorder()) == 1
    assert "GITHUB_REPOSITORY is not set" in capsys.readouterr().err


@pytest.mark.parametrize("argv", [[], ["target"], ["attach", "a"], ["publish", "x"]])
def test_other_arguments_are_a_usage_error(argv: list[str]) -> None:
    assert release_crate.main(argv, {}, Recorder()) == 2
