#!/usr/bin/env python3
"""Record or check the public surface and examples of the client packages."""

from __future__ import annotations

import argparse
import difflib
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
PACKAGES = ("libtmux-core", "libtmux", "libtmux-tokio")

DECLARATIONS = (*PACKAGES, "libtmux-macros")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="update API inventories")
    parser.add_argument("--examples", action="store_true", help="check root type examples")
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT, text=True))
    docs = Path(metadata["target_directory"]) / "doc"
    failed = False
    for package in DECLARATIONS:
        subprocess.run(["cargo", "+nightly-2026-09-20", "rustdoc", "--locked", "--package",
                        package, "--all-features", "--", "-Zunstable-options",
                        "--output-format", "json"], cwd=ROOT, check=True)

    # Every declaration must come from this run, including workspace re-exports.
    for package in PACKAGES:
        source = docs / f"{package.replace('-', '_')}.json"
        dependencies = [argument for peer in DECLARATIONS if peer != package
                        for argument in ("--dependency-json", str(docs / f"{peer.replace('-', '_')}.json"))]
        if args.examples:
            subprocess.run([sys.executable, "scripts/example-coverage.py",
                            "--require-all", str(source), *dependencies], cwd=ROOT, check=True)
            continue
        current = subprocess.check_output(
            [sys.executable, "scripts/public-api.py", str(source), *dependencies], cwd=ROOT, text=True)
        if not current.strip():
            raise SystemExit(f"{package}: public inventory is empty")
        ledger = ROOT / "crates" / package / "docs/public-api.txt"
        if args.write:
            ledger.parent.mkdir(parents=True, exist_ok=True)
            ledger.write_text(current)
        else:
            saved = ledger.read_text() if ledger.exists() else ""
            if saved != current:
                failed = True
                sys.stdout.writelines(difflib.unified_diff(
                    saved.splitlines(keepends=True), current.splitlines(keepends=True),
                    fromfile=str(ledger.relative_to(ROOT)), tofile=f"{package} current"))
    if failed:
        raise SystemExit("public API changed; run `just api` and review the inventories")


if __name__ == "__main__":
    main()
