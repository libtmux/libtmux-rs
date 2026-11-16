"""Negative controls for the source-owned complete-program manifest."""

import json
import shutil
from pathlib import Path

import check_api_examples as CHECK
import pytest

ROOT = Path(__file__).resolve().parents[3]


@pytest.fixture
def root(tmp_path):
    """A checkout holding only the files the manifest names."""
    manifest = json.loads((ROOT / CHECK.MANIFEST).read_text())
    files = [
        CHECK.INVENTORY,
        *(item["sourceFile"] for item in manifest["setupFiles"]),
        *(item["sourceFile"] for item in manifest["examples"]),
    ]
    for file in files:
        destination = tmp_path / file
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / file, destination)
    return tmp_path


@pytest.fixture
def manifest():
    return json.loads((ROOT / CHECK.MANIFEST).read_text())


def test_records_whole_files_and_native_targets(root, manifest):
    result = CHECK.validate(root, manifest)
    assert len(result["examples"]) == 7
    assert sum(len(item["targets"]) for item in result["examples"]) == 35
    for example in result["examples"]:
        assert [item["name"] for item in example["files"]] == [
            "Cargo.toml",
            "rust-toolchain.toml",
            "src/main.rs",
        ]
        for item in example["files"]:
            assert item["code"] == (root / item["sourceFile"]).read_text()


def test_rejects_unknown_native_target(root, manifest):
    manifest["examples"][0]["targets"][0] = "libtmux::Server::invented"
    with pytest.raises(ValueError, match="unknown native target"):
        CHECK.validate(root, manifest)


def test_rejects_duplicate_native_target(root, manifest):
    target = manifest["examples"][0]["targets"][0]
    manifest["examples"][1]["targets"].append(target)
    with pytest.raises(ValueError, match="duplicate native target"):
        CHECK.validate(root, manifest)


def test_rejects_unmapped_program(root, manifest):
    manifest["examples"].pop()
    with pytest.raises(ValueError, match="program files disagree"):
        CHECK.validate(root, manifest)


def test_rejects_missing_setup(root, manifest):
    manifest["setupFiles"].pop()
    with pytest.raises(ValueError, match="both Cargo and toolchain"):
        CHECK.validate(root, manifest)


def test_rejects_source_outside_checkout(root, manifest):
    manifest["setupFiles"][0]["sourceFile"] = "../Cargo.toml"
    with pytest.raises(ValueError, match="source path leaves checkout"):
        CHECK.validate(root, manifest)


def test_rejects_truncated_file(root, manifest):
    file = root / manifest["examples"][0]["sourceFile"]
    file.write_bytes(file.read_bytes().rstrip(b"\n"))
    with pytest.raises(ValueError, match="LF-terminated"):
        CHECK.validate(root, manifest)


def test_rejects_hidden_helper(root, manifest):
    file = root / manifest["examples"][0]["sourceFile"]
    file.write_text(file.read_text() + "mod helper;\n")
    with pytest.raises(ValueError, match="hidden source helper"):
        CHECK.validate(root, manifest)


def test_rejects_missing_entrypoint(root, manifest):
    file = root / manifest["examples"][0]["sourceFile"]
    file.write_text(file.read_text().replace("async fn main()", "async fn helper()"))
    with pytest.raises(ValueError, match="displayed entrypoint"):
        CHECK.validate(root, manifest)
