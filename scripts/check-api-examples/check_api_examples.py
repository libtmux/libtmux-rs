#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Validate complete API programs against the recorded native public surface.

    check_api_examples.py [--output FILE]

Reads the Tokio and mixed-runtime API example manifests and checks each
complete program against its targets' defining crates. Older revisions keep
their single libtmux manifest. `--output` writes complete source and hashes.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import tomllib
from pathlib import Path, PurePosixPath

MANIFEST = Path("crates/libtmux/examples/api/manifest.json")
INVENTORY = "crates/libtmux/docs/public-api.txt"
PACKAGES = ("libtmux-core", "libtmux-blocking", "libtmux-tokio", "libtmux")


def source_file(root: Path, path: str) -> dict[str, str]:
    """Read a complete UTF-8 source file inside the selected checkout."""
    relative = PurePosixPath(path)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"source path leaves checkout: {path}")
    source = (root / path).resolve()
    if not source.is_relative_to(root.resolve()):
        raise ValueError(f"source path leaves checkout: {path}")
    data = source.read_bytes()
    if not data or not data.endswith(b"\n") or b"\r" in data:
        raise ValueError(f"source file must have complete LF-terminated lines: {path}")
    return {
        "sourceFile": path,
        "code": data.decode("utf-8"),
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def native_inventory(root: Path, package: str) -> tuple[dict[str, str], set[str]]:
    """Read one package's recorded declarations without borrowing another's names."""
    path = f"crates/{package}/docs/public-api.txt"
    if not (root / path).is_file():
        raise ValueError(f"missing native public inventory: {path}")
    inventory = source_file(root, path)
    native = set(re.findall(r"^\w+ (\w+(?:::\w+)*)(?=[<(: ]|$)", inventory["code"], re.MULTILINE))
    if not native:
        raise ValueError(f"native public inventory has no declarations: {path}")
    return inventory, native


def expected_run_command(root: Path, *, package: str, split: bool) -> str:
    """Keep the mixed reader's standalone run aligned with its checkout recipe."""
    if not split or package != "libtmux":
        return "cargo run --quiet"
    readme = source_file(root, "crates/libtmux/README.md")["code"]
    invocation = '  cargo run --quiet -p libtmux --example api_mixed --features tokio -- "$socket"'
    recipes = [
        block
        for block in re.findall(r"^```sh\n(.*?)\n```$", readme, re.DOTALL | re.MULTILINE)
        if invocation in block.split("\n")
    ]
    if len(recipes) != 1 or recipes[0].split("\n").count(invocation) != 1:
        raise ValueError("expected one mixed-reader shell recipe in the facade README")
    return "\n".join(
        '  cargo run --quiet -- "$socket"' if line == invocation else line
        for line in recipes[0].split("\n")
    )


def validate(
    root: Path,
    manifest: dict,
    *,
    package: str = "libtmux",
    split: bool = False,
    inventories: dict[str, tuple[dict[str, str], set[str]]] | None = None,
) -> dict:
    """Check explicit native targets and return the exact displayed file hashes."""
    keys = {
        "schemaVersion",
        "language",
        "nativeInventory",
        "setupFiles",
        "programFileName",
        "runCommand",
        "examples",
    }
    if set(manifest) != keys or manifest["schemaVersion"] != 1:
        raise ValueError("unsupported API example manifest schema")
    if (
        manifest["language"] != "rust"
        or manifest["nativeInventory"] != f"crates/{package}/docs/public-api.txt"
        or manifest["programFileName"] != "src/main.rs"
        or manifest["runCommand"] != expected_run_command(root, package=package, split=split)
    ):
        raise ValueError("unexpected Rust example setup")
    if inventories is None:
        inventories = {package: native_inventory(root, package)}
    inventory, _ = inventories[package]
    setup = []
    names = set()
    for item in manifest["setupFiles"]:
        if set(item) != {"sourceFile", "name"} or item["name"] in names:
            raise ValueError("invalid or duplicate setup file")
        if item["name"] not in {"Cargo.toml", "rust-toolchain.toml"}:
            raise ValueError(f"unexpected setup filename: {item['name']}")
        names.add(item["name"])
        setup.append({**source_file(root, item["sourceFile"]), "name": item["name"]})
    if names != {"Cargo.toml", "rust-toolchain.toml"}:
        raise ValueError("both Cargo and toolchain setup files are required")
    examples = []
    identifiers = set()
    targets = set()
    files = set()
    for item in manifest["examples"]:
        if set(item) != {
            "id",
            "title",
            "description",
            "sourceFile",
            "cargoExample",
            "targets",
            "expectedOutput",
        }:
            raise ValueError("unsupported API example fields")
        if not re.fullmatch(r"rust-[a-z]+", item["id"]) or item["id"] in identifiers:
            raise ValueError(f"invalid or duplicate example ID: {item['id']}")
        identifiers.add(item["id"])
        if not all(
            isinstance(item[key], str) and item[key].strip()
            for key in ["title", "description", "expectedOutput"]
        ):
            raise ValueError(f"example prose or expected output is empty: {item['id']}")
        name = item["id"].removeprefix("rust-")
        if (
            item["cargoExample"] != f"api_{name}"
            or item["sourceFile"] != f"crates/{package}/examples/api_{name}.rs"
            or item["sourceFile"] in files
        ):
            raise ValueError(f"unexpected or duplicate program file: {item['id']}")
        files.add(item["sourceFile"])
        if not isinstance(item["targets"], list) or not item["targets"]:
            raise ValueError(f"example has no native targets: {item['id']}")
        for target in item["targets"]:
            defining = target.split("::", 1)[0].replace("_", "-")
            found = inventories.get(defining)
            if found is None or target not in found[1]:
                raise ValueError(f"unknown native target: {target}")
            if target in targets:
                raise ValueError(f"duplicate native target: {target}")
            targets.add(target)
        program = source_file(root, item["sourceFile"])
        code = program["code"]
        main = re.search(r"\b(async\s+)?fn\s+main\s*\(\s*\)", code)
        if (
            main is None
            or (main[1] is not None and "#[tokio::main" not in code)
            or (not split and main[1] is None)
        ):
            raise ValueError(f"program has no displayed entrypoint: {item['id']}")
        if re.search(
            r"\binclude(?:_str|_bytes)?!|^\s*mod\s+\w+\s*;|use\s+(?:crate|super)::",
            code,
            re.MULTILINE,
        ):
            raise ValueError(f"program depends on a hidden source helper: {item['id']}")
        examples.append({**item, "files": [*setup, {**program, "name": "src/main.rs"}]})
    shipped = {
        path.relative_to(root).as_posix()
        for path in (root / f"crates/{package}/examples").glob("api_*.rs")
    }
    if files != shipped:
        raise ValueError("manifest and complete API program files disagree")
    if not examples:
        raise ValueError("manifest has no complete API examples")
    return {"nativeInventorySha256": inventory["sha256"], "examples": examples}


def validate_workspace(root: Path) -> dict:
    """Choose manifests from this revision and validate both roots before returning."""
    split = any((root / f"crates/{package}/Cargo.toml").exists() for package in PACKAGES[:3])
    packages = PACKAGES if split else ("libtmux",)
    if split:
        for package in packages:
            path = root / f"crates/{package}/Cargo.toml"
            if (
                not path.is_file()
                or tomllib.loads(path.read_text()).get("package", {}).get("name") != package
            ):
                raise ValueError(f"incomplete Rust driver layout: {package}")
    inventories = {package: native_inventory(root, package) for package in packages}
    examples = []
    for package in ("libtmux-tokio", "libtmux") if split else ("libtmux",):
        path = root / f"crates/{package}/examples/api/manifest.json"
        if not path.is_file():
            raise ValueError(f"missing API example manifest: {path.relative_to(root)}")
        manifest = json.loads(path.read_text())
        result = validate(root, manifest, package=package, split=split, inventories=inventories)
        if not split:
            return result
        examples.extend(result["examples"])
    for key, label in (
        ("id", "example ID"),
        ("sourceFile", "program file"),
        ("targets", "native target"),
    ):
        seen = set()
        for example in examples:
            values = example[key] if key == "targets" else [example[key]]
            for value in values:
                if value in seen:
                    raise ValueError(f"duplicate {label}: {value}")
                seen.add(value)
    return {
        "nativeInventories": [
            {"sourceFile": inventory["sourceFile"], "sha256": inventory["sha256"]}
            for inventory, _ in inventories.values()
        ],
        "examples": examples,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="write exact source payload and hashes")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    result = validate_workspace(root)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    count = sum(len(item["targets"]) for item in result["examples"])
    print(f"validated {len(result['examples'])} complete Rust programs on {count} native targets")


if __name__ == "__main__":
    main()
