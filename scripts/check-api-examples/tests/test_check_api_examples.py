"""Negative controls for the source-owned complete-program manifest."""

import json
from pathlib import Path

import check_api_examples as CHECK
import pytest

PACKAGES = ("libtmux-core", "libtmux-blocking", "libtmux-tokio", "libtmux")
CHECKOUT = Path(__file__).resolve().parents[3]


def write(root: Path, file: str, code: str) -> None:
    destination = root / file
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(code)


def example(package: str, name: str, targets: list[str]) -> dict:
    return {
        "id": f"rust-{name}",
        "title": name.title(),
        "description": f"A complete {name} example.",
        "sourceFile": f"crates/{package}/examples/api_{name}.rs",
        "cargoExample": f"api_{name}",
        "targets": targets,
        "expectedOutput": name,
    }


def make_manifest(root: Path, package: str, examples: list[dict]) -> dict:
    manifest = {
        "schemaVersion": 1,
        "language": "rust",
        "nativeInventory": f"crates/{package}/docs/public-api.txt",
        "setupFiles": [
            {"sourceFile": f"crates/{package}/examples/api/consumer.toml", "name": "Cargo.toml"},
            {"sourceFile": "rust-toolchain.toml", "name": "rust-toolchain.toml"},
        ],
        "programFileName": "src/main.rs",
        "runCommand": "cargo run --quiet",
        "examples": examples,
    }
    write(root, manifest["setupFiles"][0]["sourceFile"], '[package]\nname = "example"\n')
    write(root, "rust-toolchain.toml", '[toolchain]\nchannel = "stable"\n')
    for item in examples:
        write(root, item["sourceFile"], "#[tokio::main]\nasync fn main() {}\n")
    write(root, f"crates/{package}/examples/api/manifest.json", json.dumps(manifest) + "\n")
    return manifest


@pytest.fixture
def root(tmp_path):
    """A legacy checkout with complete source and its native inventory."""
    write(
        tmp_path,
        CHECK.INVENTORY,
        "struct libtmux::Server\nfunction libtmux::Server::sessions: fn()\n",
    )
    make_manifest(
        tmp_path,
        "libtmux",
        [
            example("libtmux", "construction", ["libtmux::Server"]),
            example("libtmux", "listings", ["libtmux::Server::sessions"]),
        ],
    )
    return tmp_path


@pytest.fixture
def manifest(root):
    return json.loads((root / CHECK.MANIFEST).read_text())


@pytest.fixture
def split_root(tmp_path):
    for package in PACKAGES:
        write(tmp_path, f"crates/{package}/Cargo.toml", f'[package]\nname = "{package}"\n')
    declarations = {
        "libtmux-core": "function libtmux_core::CreatedSession::pane: fn()\n",
        "libtmux-blocking": "function libtmux_blocking::Server::new_session: fn()\n",
        "libtmux-tokio": (
            "struct libtmux_tokio::Server\nfunction libtmux_tokio::Server::capture: fn()\n"
        ),
        "libtmux": "module libtmux::blocking\n",
    }
    for package, code in declarations.items():
        write(tmp_path, f"crates/{package}/docs/public-api.txt", code)
    make_manifest(
        tmp_path,
        "libtmux-tokio",
        [example("libtmux-tokio", "construction", ["libtmux_tokio::Server"])],
    )
    mixed = example(
        "libtmux",
        "mixed",
        [
            "libtmux_blocking::Server::new_session",
            "libtmux_core::CreatedSession::pane",
            "libtmux_tokio::Server::capture",
        ],
    )
    facade = make_manifest(tmp_path, "libtmux", [mixed])
    facade["runCommand"] = json.loads((CHECKOUT / CHECK.MANIFEST).read_text())["runCommand"]
    write(tmp_path, str(CHECK.MANIFEST), json.dumps(facade) + "\n")
    write(tmp_path, "crates/libtmux/README.md", (CHECKOUT / "crates/libtmux/README.md").read_text())
    write(tmp_path, mixed["sourceFile"], "fn main() {}\n")
    return tmp_path


def test_records_whole_files_and_native_targets(root, manifest):
    result = CHECK.validate(root, manifest)
    assert len(result["examples"]) == 2
    assert sum(len(item["targets"]) for item in result["examples"]) == 2
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


def test_workspace_reads_both_manifests_and_the_defining_crate_inventories(split_root):
    result = CHECK.validate_workspace(split_root)
    assert [item["id"] for item in result["examples"]] == ["rust-construction", "rust-mixed"]
    assert len(result["nativeInventories"]) == 4
    assert result["examples"][1]["files"][-1]["code"] == "fn main() {}\n"


def test_old_revisions_keep_the_single_manifest_contract(root, manifest):
    assert CHECK.validate_workspace(root) == CHECK.validate(root, manifest)


def test_checked_in_mixed_recipe_and_programs_pass():
    result = CHECK.validate_workspace(CHECKOUT)
    mixed = next(item for item in result["examples"] if item["id"] == "rust-mixed")
    assert (
        mixed["files"][-1]["code"]
        == (CHECKOUT / "crates/libtmux/examples/api_mixed.rs").read_text()
    )


@pytest.mark.parametrize("damage", ["default", "cleanup", "endpoint", "cargo", "extra"])
def test_checked_in_mixed_recipe_rejects_a_changed_run(damage):
    manifest = json.loads((CHECKOUT / CHECK.MANIFEST).read_text())
    command = manifest["runCommand"]
    if damage == "default":
        command = "cargo run --quiet"
    elif damage == "cleanup":
        command = command.replace("  trap cleanup EXIT\n", "")
    elif damage == "endpoint":
        command = command.replace("/tmp/libtmux-rs-dev/mixed.XXXXXX", "/tmp/shared.XXXXXX")
    elif damage == "cargo":
        command = command.replace(
            '  cargo run --quiet -- "$socket"',
            '  cargo run --quiet -p libtmux --example api_mixed --features tokio -- "$socket"',
        )
    else:
        command += "\ntrue"
    assert command != manifest["runCommand"]
    manifest["runCommand"] = command
    inventories = {package: CHECK.native_inventory(CHECKOUT, package) for package in PACKAGES}
    with pytest.raises(ValueError, match="unexpected Rust example setup"):
        CHECK.validate(CHECKOUT, manifest, split=True, inventories=inventories)


@pytest.mark.parametrize("damage", ["missing", "duplicate", "invocation"])
def test_checked_in_readme_requires_one_recognised_mixed_recipe(tmp_path, damage):
    readme = (CHECKOUT / "crates/libtmux/README.md").read_text()
    if damage == "missing":
        readme = readme.replace("```sh\n", "```text\n")
    elif damage == "duplicate":
        readme += readme
    else:
        readme = readme.replace("--features tokio --", "--features test-support --")
    write(tmp_path, "crates/libtmux/README.md", readme)
    with pytest.raises(ValueError, match="one mixed-reader shell recipe"):
        CHECK.expected_run_command(tmp_path, package="libtmux", split=True)


def test_tokio_recipe_keeps_its_default_run(split_root):
    file = split_root / "crates/libtmux-tokio/examples/api/manifest.json"
    manifest = json.loads(file.read_text())
    manifest["runCommand"] = json.loads((CHECKOUT / CHECK.MANIFEST).read_text())["runCommand"]
    file.write_text(json.dumps(manifest) + "\n")
    with pytest.raises(ValueError, match="unexpected Rust example setup"):
        CHECK.validate_workspace(split_root)


def test_split_layout_never_falls_back_to_the_legacy_manifest(split_root):
    (split_root / "crates/libtmux-tokio/examples/api/manifest.json").unlink()
    with pytest.raises(ValueError, match="missing API example manifest"):
        CHECK.validate_workspace(split_root)


def test_partial_split_layout_is_refused(split_root):
    (split_root / "crates/libtmux-blocking/Cargo.toml").unlink()
    with pytest.raises(ValueError, match="incomplete Rust driver layout"):
        CHECK.validate_workspace(split_root)


def test_missing_defining_crate_inventory_is_refused(split_root):
    (split_root / "crates/libtmux-core/docs/public-api.txt").unlink()
    with pytest.raises(ValueError, match="missing native public inventory"):
        CHECK.validate_workspace(split_root)


def test_native_target_from_another_crate_requires_its_own_inventory(split_root):
    write(split_root, "crates/libtmux-core/docs/public-api.txt", "struct libtmux_core::Other\n")
    with pytest.raises(ValueError, match="unknown native target: libtmux_core::CreatedSession"):
        CHECK.validate_workspace(split_root)


def test_duplicate_targets_across_manifests_are_refused(split_root):
    file = split_root / "crates/libtmux/examples/api/manifest.json"
    manifest = json.loads(file.read_text())
    manifest["examples"][0]["targets"].append("libtmux_tokio::Server")
    file.write_text(json.dumps(manifest) + "\n")
    with pytest.raises(ValueError, match="duplicate native target"):
        CHECK.validate_workspace(split_root)
