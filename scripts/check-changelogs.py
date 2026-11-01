#!/usr/bin/env python3
"""Fail when a published crate has no changelog it ships and links.

Four crates are published and tagged on their own, so a reader of one should
not have to read the history of the other three. Each keeps a `CHANGELOG.md`
beside its `Cargo.toml`. This check holds that file to what a reader and the
release rely on: it opens with the crate's own name, so one pasted over another
crate's is noticed; it has an Unreleased section for the next release to land
under; the crate's `include` list ships it; and the crate's README links it.

`scripts/check-package-contents.sh` checks the same file in the packaged
tarball. This check reads the tree, so a change that deletes a changelog fails
without a `cargo package`.

It needs Python 3.11 or newer for `tomllib`. Run from the repository root:

    python3 scripts/check-changelogs.py
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
FENCE = re.compile(r"^ {0,3}(```|~~~)")


def outside_fences(text: str) -> list[str]:
    """Return the lines of `text` that are not inside a fenced code block."""
    kept = []
    fenced = False
    for line in text.splitlines():
        if FENCE.match(line):
            fenced = not fenced
        elif not fenced:
            kept.append(line)
    return kept


def published_crates(root: pathlib.Path) -> dict[str, pathlib.Path]:
    """Return each published crate's name and directory.

    A crate is published unless its manifest says `publish = false`.
    """
    crates = {}
    for manifest in sorted((root / "crates").glob("*/Cargo.toml")):
        package = tomllib.loads(manifest.read_text())["package"]
        if package.get("publish", True) not in (False, []):
            crates[package["name"]] = manifest.parent
    return crates


def check(root: pathlib.Path) -> list[str]:
    """Return one message per way a published crate falls short."""
    crates = published_crates(root)
    if not crates:
        return ["crates/: no published crate found"]
    problems = []
    root_changelog = root / "CHANGELOG.md"
    if not root_changelog.is_file() or "## Unreleased" not in outside_fences(root_changelog.read_text()):
        problems.append('CHANGELOG.md is missing or has no "## Unreleased" section')
    for name, directory in crates.items():
        label = directory.relative_to(root).as_posix()
        path = directory / "CHANGELOG.md"
        if not path.is_file():
            problems.append(f"{label}/CHANGELOG.md is missing; {name} is published")
            continue
        lines = outside_fences(path.read_text())
        if not any(line.startswith(f"Notable changes to `{name}`.") for line in lines):
            problems.append(f'{label}/CHANGELOG.md does not open with "Notable changes to `{name}`."')
        if "## Unreleased" not in lines:
            problems.append(f'{label}/CHANGELOG.md has no "## Unreleased" section')
        include = tomllib.loads((directory / "Cargo.toml").read_text())["package"].get("include", [])
        if "/CHANGELOG.md" not in include:
            problems.append(f"{label}/Cargo.toml does not include /CHANGELOG.md, so {name} does not ship it")
        readme = (directory / "README.md").read_text()
        if "](CHANGELOG.md)" not in readme and "]: CHANGELOG.md" not in readme:
            problems.append(f"{label}/README.md does not link CHANGELOG.md")
    return problems


def main(argv: list[str] | None = None) -> int:
    """Print every shortfall and exit nonzero if there is one."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=pathlib.Path, default=ROOT)
    args = parser.parse_args(argv)
    problems = check(args.root.resolve())
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
