#!/usr/bin/env python3
"""Check blocking and native dependencies across every Cargo target."""

from __future__ import annotations

import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def dependencies(package: str, *features: str) -> set[str]:
    output = subprocess.check_output(
        ["cargo", "tree", "--locked", "--package", package, "--target", "all", "--edges",
         "normal,build", "--prefix", "none", "--format", "{p}", *features],
        cwd=ROOT, text=True,
    )
    return {line.split()[0] for line in output.splitlines() if line.strip()}


def main() -> None:
    for features in ((), ("--no-default-features",),
                     ("--no-default-features", "--features", "control-mode,plan")):
        graph = dependencies("libtmux", *features)
        if graph != {"libtmux"}:
            raise SystemExit(f"blocking default/control/plan has dependencies: {sorted(graph)}")
    optional = dependencies("libtmux", "--all-features")
    forbidden = optional & {"tokio", "tokio-util", "libtmux-tokio"}
    if forbidden:
        raise SystemExit(f"blocking client pulls in Tokio: {sorted(forbidden)}")
    native = dependencies("libtmux-tokio", "--no-default-features")
    if not {"libtmux", "libtmux-tokio", "tokio"}.issubset(native):
        raise SystemExit("native client must depend on shared libtmux and Tokio")
    print("runtime boundaries: std-only default/control/plan; optional features without Tokio; native async client")


if __name__ == "__main__":
    main()
