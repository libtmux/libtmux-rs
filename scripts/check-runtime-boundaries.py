#!/usr/bin/env python3
"""Check value, shared, and Tokio dependency contracts for every Cargo target."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def cargo(*args: str) -> str:
    return subprocess.check_output(["cargo", *args], cwd=ROOT, text=True)


def dependencies(package: str, *features: str) -> set[str]:
    tree = cargo("tree", "--locked", "--package", package, "--target", "all", "--edges",
                 "normal,build", "--prefix", "none", "--format", "{p}", *features)
    return {line.split()[0] for line in tree.splitlines() if line.strip()}


def main() -> None:
    metadata = json.loads(cargo("metadata", "--locked", "--no-deps",
                                "--format-version", "1"))
    values = next(package for package in metadata["packages"]
                  if package["name"] == "libtmux-core")
    if values["dependencies"]:
        raise SystemExit("libtmux-core must have no normal, build, or dev dependencies")
    if dependencies("libtmux-core", "--all-features") != {"libtmux-core"}:
        raise SystemExit("libtmux-core dependency graph is not empty")
    for features in (("--no-default-features",), ("--all-features",)):
        shared = dependencies("libtmux", *features)
        forbidden = shared & {"tokio", "tokio-util", "libtmux-tokio"}
        if forbidden:
            raise SystemExit(f"libtmux pulls in Tokio: {sorted(forbidden)}")
    native = dependencies("libtmux-tokio", "--no-default-features")
    if not {"libtmux", "libtmux-core", "tokio"}.issubset(native):
        raise SystemExit("libtmux-tokio must use the shared client and native Tokio")
    print("dependency boundaries: zero-dependency values; shared client without Tokio; native Tokio adapter")


if __name__ == "__main__":
    main()
