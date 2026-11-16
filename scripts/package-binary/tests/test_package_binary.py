"""The archive holds the layout `cargo binstall` fetches, and a missing piece fails the run."""

from __future__ import annotations

import pathlib
import tarfile

import package_binary
import pytest

TARGET = "aarch64-unknown-linux-musl"


def workspace(root: pathlib.Path, package: str = "demo") -> pathlib.Path:
    """A workspace with a built command and a crate that has its documents."""
    release = root / "target" / TARGET / "release"
    release.mkdir(parents=True)
    command = release / package
    command.write_text("#!/bin/sh\n", encoding="utf-8")
    command.chmod(0o755)
    crate = root / "crates" / package
    crate.mkdir(parents=True)
    for document in package_binary.DOCUMENTS:
        (crate / document).write_text(f"{document}\n", encoding="utf-8")
    return root


def test_the_archive_holds_the_command_and_the_documents(tmp_path: pathlib.Path) -> None:
    workspace(tmp_path)
    name = package_binary.build_archive("demo", TARGET, "0.1.2", tmp_path)
    assert name == f"demo-0.1.2-{TARGET}"
    with tarfile.open(tmp_path / f"{name}.tar.gz") as archive:
        members = {member.name: member for member in archive.getmembers()}
    assert set(members) == {
        name,
        f"{name}/demo",
        f"{name}/README.md",
        f"{name}/LICENSE-APACHE",
        f"{name}/LICENSE-MIT",
    }
    assert members[f"{name}/demo"].mode & 0o111, "the command lost its executable bit"


def test_a_linked_licence_is_archived_as_the_file_it_names(tmp_path: pathlib.Path) -> None:
    workspace(tmp_path)
    crate = tmp_path / "crates" / "demo"
    (tmp_path / "LICENSE").write_text("text\n", encoding="utf-8")
    (crate / "LICENSE-MIT").unlink()
    (crate / "LICENSE-MIT").symlink_to(tmp_path / "LICENSE")
    name = package_binary.build_archive("demo", TARGET, "0.1.2", tmp_path)
    with tarfile.open(tmp_path / f"{name}.tar.gz") as archive:
        assert archive.getmember(f"{name}/LICENSE-MIT").isfile()


@pytest.mark.parametrize(
    "missing",
    ["target/aarch64-unknown-linux-musl/release/demo", "crates/demo/LICENSE-APACHE"],
)
def test_a_missing_piece_fails(tmp_path: pathlib.Path, missing: str) -> None:
    workspace(tmp_path)
    (tmp_path / missing).unlink()
    with pytest.raises(package_binary.PackageError, match="cannot lay out"):
        package_binary.build_archive("demo", TARGET, "0.1.2", tmp_path)


def test_an_existing_directory_fails(tmp_path: pathlib.Path) -> None:
    workspace(tmp_path)
    (tmp_path / f"demo-0.1.2-{TARGET}").mkdir()
    with pytest.raises(package_binary.PackageError, match="cannot lay out"):
        package_binary.build_archive("demo", TARGET, "0.1.2", tmp_path)


def test_the_version_comes_from_the_member_with_that_name() -> None:
    metadata = (
        '{"packages": [{"name": "a", "version": "1.0.0"}, {"name": "b", "version": "2.0.0"}]}'
    )
    assert package_binary.crate_version(metadata, "b") == "2.0.0"
    with pytest.raises(package_binary.PackageError, match="no workspace member is called c"):
        package_binary.crate_version(metadata, "c")


def test_main_writes_the_name_for_the_next_step(tmp_path: pathlib.Path) -> None:
    workspace(tmp_path)
    output = tmp_path / "output"
    status = package_binary.main(
        ["demo", TARGET],
        {"GITHUB_OUTPUT": str(output)},
        version_of=lambda package: "0.3.0",
        root=tmp_path,
    )
    assert status == 0
    assert output.read_text(encoding="utf-8") == f"name=demo-0.3.0-{TARGET}\n"


def test_an_explicit_version_wins_over_the_manifest(tmp_path: pathlib.Path) -> None:
    workspace(tmp_path)

    def unused(package: str) -> str:
        raise AssertionError("the manifest was read although a version was given")

    assert package_binary.main(["demo", TARGET, "9.9.9"], {}, version_of=unused, root=tmp_path) == 0
    assert (tmp_path / f"demo-9.9.9-{TARGET}.tar.gz").exists()


def test_main_fails_and_names_what_is_missing(
    tmp_path: pathlib.Path, capsys: pytest.CaptureFixture[str]
) -> None:
    status = package_binary.main(
        ["demo", TARGET], {}, version_of=lambda package: "0.3.0", root=tmp_path
    )
    assert status == 1
    assert "cannot lay out" in capsys.readouterr().err


def test_main_rejects_a_wrong_argument_count() -> None:
    assert package_binary.main(["demo"], {}) == 2
