#!/usr/bin/env python3
"""Validate complete API programs against the recorded native public surface."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re


MANIFEST = Path("crates/libtmux/examples/api/manifest.json")
INVENTORY = "crates/libtmux/docs/public-api.txt"


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
    return {"sourceFile": path, "code": data.decode("utf-8"),
            "sha256": hashlib.sha256(data).hexdigest()}


def validate(root: Path, manifest: dict) -> dict:
    """Check explicit native targets and return the exact displayed file hashes."""
    keys = {"schemaVersion", "language", "nativeInventory", "setupFiles",
            "programFileName", "runCommand", "examples"}
    if set(manifest) != keys or manifest["schemaVersion"] != 1:
        raise ValueError("unsupported API example manifest schema")
    if (manifest["language"] != "rust" or manifest["nativeInventory"] != INVENTORY
            or manifest["programFileName"] != "src/main.rs"
            or manifest["runCommand"] != "cargo run --quiet"):
        raise ValueError("unexpected Rust example setup")
    inventory = source_file(root, INVENTORY)
    native = set(re.findall(r"^\w+ (\S+?)(?=: |$)", inventory["code"], re.MULTILINE))
    if not native:
        raise ValueError("native public inventory has no declarations")
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
        if set(item) != {"id", "title", "description", "sourceFile", "cargoExample",
                         "targets", "expectedOutput"}:
            raise ValueError("unsupported API example fields")
        if not re.fullmatch(r"rust-[a-z]+", item["id"]) or item["id"] in identifiers:
            raise ValueError(f"invalid or duplicate example ID: {item['id']}")
        identifiers.add(item["id"])
        if not all(isinstance(item[key], str) and item[key].strip()
                   for key in ["title", "description", "expectedOutput"]):
            raise ValueError(f"example prose or expected output is empty: {item['id']}")
        name = item["id"].removeprefix("rust-")
        if (item["cargoExample"] != f"api_{name}"
                or item["sourceFile"] != f"crates/libtmux/examples/api_{name}.rs"
                or item["sourceFile"] in files):
            raise ValueError(f"unexpected or duplicate program file: {item['id']}")
        files.add(item["sourceFile"])
        if not isinstance(item["targets"], list) or not item["targets"]:
            raise ValueError(f"example has no native targets: {item['id']}")
        for target in item["targets"]:
            if target not in native:
                raise ValueError(f"unknown native target: {target}")
            if target in targets:
                raise ValueError(f"duplicate native target: {target}")
            targets.add(target)
        program = source_file(root, item["sourceFile"])
        code = program["code"]
        if "#[tokio::main" not in code or "async fn main()" not in code:
            raise ValueError(f"program has no displayed entrypoint: {item['id']}")
        if re.search(r"\binclude(?:_str|_bytes)?!|^\s*mod\s+\w+\s*;|use\s+(?:crate|super)::",
                     code, re.MULTILINE):
            raise ValueError(f"program depends on a hidden source helper: {item['id']}")
        examples.append({**item, "files": [*setup, {**program, "name": "src/main.rs"}]})
    shipped = {path.relative_to(root).as_posix()
               for path in (root / "crates/libtmux/examples").glob("api_*.rs")}
    if files != shipped:
        raise ValueError("manifest and complete API program files disagree")
    if not examples:
        raise ValueError("manifest has no complete API examples")
    return {"nativeInventorySha256": inventory["sha256"], "examples": examples}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="write exact source payload and hashes")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    manifest = json.loads((root / MANIFEST).read_text())
    result = validate(root, manifest)
    if args.output:
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    count = sum(len(item["targets"]) for item in result["examples"])
    print(f"validated {len(result['examples'])} complete Rust programs on {count} native targets")


if __name__ == "__main__":
    main()
