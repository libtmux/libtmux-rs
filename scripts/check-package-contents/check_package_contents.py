#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Verify what the published crates contain.

    check_package_contents.py

Lists each published crate's tarball with `cargo package --list` and checks it:

- the files each crate's front page and tests depend on ship;
- no sandbox test ships, since a package has no `.cargo/config.toml` and no runner;
- a shipped document tells a reader to depend on the version this tree has;
- a relative link in a shipped document points at something else that ships;
- `libtmux` carries no macro source, `libtmux-macros` does not depend on
  `libtmux`, and `libtmux` without its default features has no optional
  dependency.

`just package` runs it after `cargo package`. Run from anywhere: the script
changes to the repository root.
"""

from __future__ import annotations

import glob
import json
import os
import re
import subprocess
import sys
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

CRATES = {
    "libtmux": "crates/libtmux",
    "libtmux-macros": "crates/libtmux-macros",
    "tmux-mcp": "crates/tmux-mcp",
    "tmux-workspace": "crates/tmux-workspace",
}

# Each crate ships its own CHANGELOG.md, so a reader of the packaged crate has
# the history of that crate and not of the other three.
#
# The `libtmux` front page links docs/design.md and names the examples, so both
# ship or those references break for anyone reading the packaged crate. The MCP
# front page names its examples too, but its planning notes under docs/
# deliberately do not ship: they are working material for this repository.
#
# `tmux-workspace`'s `docs/libtmux-macros-README.md` is a symlink to the copy
# that crate owns, and the doctest verifying it reads that path, so it has to
# ship or `cargo test --doc` fails for anyone using the published crate.
# `cli/bridge.py` is `include_str!`-ed into the binary, so a package missing it
# fails to build rather than to run. `cargo package`'s own verification build
# catches that, but only after compiling most of the dependency graph; this
# check fails in the time a file listing takes.
REQUIRED = {
    "libtmux": [
        "CHANGELOG.md",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "README.md",
        "docs/design.md",
        "docs/findings.md",
        "docs/format-catalog.md",
        "examples/*.rs",
        "examples/api/README.md",
        "examples/api/manifest.json",
        "examples/api/consumer.toml",
        "schema/*.json",
        "tests/fixtures/filter-v1/*.json",
        "tests/fixtures/dialect-v1/*.json",
    ],
    "libtmux-macros": [
        "CHANGELOG.md",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "README.md",
        "tests/ui/pass/*.rs",
        "tests/ui/fail/*.rs",
        "tests/ui/fail/*.stderr",
    ],
    "tmux-mcp": [
        "CHANGELOG.md",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "README.md",
        "examples/*.rs",
        "tests/snapshots/*",
    ],
    "tmux-workspace": [
        "CHANGELOG.md",
        "LICENSE-APACHE",
        "LICENSE-MIT",
        "README.md",
        "docs/cli.md",
        "docs/library.md",
        "examples/*.rs",
        "libtmux-macros-README.md",
        "tests/fixtures/tmuxp/*",
        "src/cli/bridge.py",
    ],
}

CRATE_NAME = r"\b(libtmux-macros|libtmux|tmux-mcp|tmux-workspace)\b"
MANIFEST_LINE = re.compile(CRATE_NAME + r'\s*=\s*(?:\{[^}]*version\s*=\s*)?"([^"]+)"')
CARGO_ADD = re.compile(r"cargo add " + CRATE_NAME + r"@([0-9A-Za-z.+-]+)")
CARGO_INSTALL = re.compile(
    r"cargo b?install " + CRATE_NAME + r"[^\n]*?--version[ =]([0-9A-Za-z.+-]+)"
)
LINK = re.compile(r"\]\(([^)]+)\)")
JOINED_LINES = re.compile(r"\\\n\s*")
SANDBOX_TEST = re.compile(r"^tests/sandbox_", re.MULTILINE)


def documented_versions(text: str) -> list[tuple[str, str]]:
    """Every `(crate, version)` a document tells a reader to depend on.

    A manifest line, `cargo add crate@version`, and `cargo [b]install crate
    --version version` count, the last read across `\\` continuations because an
    install command is the one most often split over lines.

    >>> documented_versions('libtmux = "0.1.2"\\ncargo add tmux-mcp@0.2.0')
    [('libtmux', '0.1.2'), ('tmux-mcp', '0.2.0')]
    >>> documented_versions('cargo install tmux-workspace \\\\\\n  --version 0.1.2')
    [('tmux-workspace', '0.1.2')]
    >>> documented_versions('libtmux = { version = "0.1.1", features = ["plan"] }')
    [('libtmux', '0.1.1')]
    """
    text = JOINED_LINES.sub(" ", text)
    found: list[tuple[str, str]] = []
    for pattern in (MANIFEST_LINE, CARGO_ADD, CARGO_INSTALL):
        found.extend((match[1], match[2]) for match in pattern.finditer(text))
    return found


def check_versions(
    root: Path, name: str, listing: Iterable[str], versions: Mapping[str, str]
) -> list[str]:
    """Each shipped document names the version of the crate it mentions in this tree.

    Each reference is checked against the version of the crate it names, not
    against one crate's version: `tmux-mcp` carries its own, so a document that
    mentions both cannot be right about both otherwise.
    """
    problems = []
    for doc in listing:
        path = root / doc
        if not doc.endswith(".md") or not path.is_file():
            continue
        for crate, found in documented_versions(path.read_text(encoding="utf-8", errors="replace")):
            if found != versions[crate]:
                problems.append(
                    f"{name} ships {doc}, which tells the reader to depend on "
                    f"{crate} {found}, not {versions[crate]}"
                )
    return problems


def resolve_link(root: Path, doc: str, target: str) -> str | None:
    """The path a link takes in the tarball, or `None` for what is not a file path.

    A link is judged by the path it takes in the tarball rather than by where a
    symlink points in the working tree: the per-crate LICENSE files are
    symlinks, and `cargo package` writes them out as real files, so the sibling
    path is the one that has to resolve.

    >>> resolve_link(Path("/crate"), "docs/a.md", "../README.md#top")
    'README.md'
    >>> resolve_link(Path("/crate"), "README.md", "https://example.com") is None
    True
    >>> resolve_link(Path("/crate"), "README.md", "#section") is None
    True
    """
    # Skip URLs and same-page anchors, which resolve to no file, and rustdoc
    # intra-doc paths such as `crate::command`, which the `-D warnings`
    # documentation build resolves instead.
    if target.startswith(("http://", "https://", "#")) or "::" in target:
        return None
    target = target.split("#", 1)[0]
    if not target:
        return None
    joined = os.path.normpath(os.path.join(root, os.path.dirname(doc), target))
    return os.path.relpath(joined, root)


def check_links(root: Path, name: str, listing: Sequence[str]) -> list[str]:
    """A relative link in a shipped document has to point at something else that ships.

    A file that exists in the working tree but is left out of `include` reads
    fine here and 404s in the tarball and on docs.rs, which is the one place
    nobody checks.
    """
    shipped = set(listing)
    problems = []
    for doc in listing:
        path = root / doc
        if not doc.endswith(".md") or not path.is_file():
            continue
        for link in LINK.findall(path.read_text(encoding="utf-8", errors="replace")):
            resolved = resolve_link(root, doc, link)
            if resolved is not None and resolved not in shipped:
                target = link.split("#", 1)[0]
                problems.append(
                    f"{name} ships {doc}, which links {target} -> {resolved}, which it does not ship"
                )
    return problems


def required_files(root: Path, patterns: Sequence[str]) -> tuple[list[str], list[str]]:
    """The files the patterns match, relative to `root`, and the literal ones that are absent."""
    present: list[str] = []
    absent: list[str] = []
    for pattern in patterns:
        matches = sorted(glob.glob(pattern, root_dir=root))
        if matches:
            present.extend(matches)
        elif not any(character in pattern for character in "*?["):
            absent.append(pattern)
    return present, absent


def check_required(
    root: Path, name: str, listing: Iterable[str], patterns: Sequence[str]
) -> list[str]:
    """Every file the crate depends on is in its tarball."""
    present, absent = required_files(root, patterns)
    if not present:
        return [f"{name}: expected packaged files are missing from the working tree"]
    shipped = set(listing)
    problems = [f"{name}: {path} is missing from the working tree" for path in absent]
    problems += [f"{name} package is missing {path}" for path in present if path not in shipped]
    return problems


def check_sandbox_tests(listing: Iterable[str]) -> list[str]:
    """The sandbox canary and runner tests assert a `.cargo/config.toml` a package does not have."""
    shipped = [entry for entry in listing if SANDBOX_TEST.match(entry)]
    if not shipped:
        return []
    return ["libtmux package ships sandbox tests:\n" + "\n".join(shipped)]


def cargo(*arguments: str) -> str:
    result = subprocess.run(
        ["cargo", *arguments], cwd=ROOT, capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        raise SystemExit(result.returncode)
    return result.stdout


def package_listing(crate: str) -> list[str]:
    output = cargo("package", "--locked", "--allow-dirty", "--package", crate, "--list")
    return output.splitlines()


def crate_versions() -> dict[str, str]:
    metadata = json.loads(cargo("metadata", "--format-version", "1", "--no-deps"))
    versions = {
        package["name"]: package["version"]
        for package in metadata["packages"]
        if package["name"] in CRATES
    }
    for crate in CRATES:
        if crate not in versions:
            raise SystemExit(f"could not read the version of {crate}")
    return versions


def tree(*arguments: str) -> list[str]:
    return cargo("tree", "--locked", *arguments, "--prefix", "none").splitlines()


def check_edges() -> list[str]:
    """What the crates are allowed to depend on."""
    problems = []
    macros_edges = tree("--package", "libtmux-macros", "--edges", "normal,build")
    problems += [
        f"libtmux-macros has a normal or build edge to libtmux: {entry}"
        for entry in macros_edges
        if entry.startswith("libtmux v")
    ]
    core_edges = tree("--package", "libtmux", "--no-default-features", "--edges", "normal")
    optional = ("serde v", "serde_core v", "serde_derive v", "libtmux-macros v")
    problems += [
        f"libtmux default-free normal edges contain an optional dependency: {entry}"
        for entry in core_edges
        if entry.startswith(optional)
    ]
    return problems


def main() -> int:
    os.chdir(ROOT)
    listings = {crate: package_listing(crate) for crate in CRATES}
    versions = crate_versions()
    problems: list[str] = []
    for crate, directory in CRATES.items():
        root = ROOT / directory
        listing = listings[crate]
        problems += check_required(root, crate, listing, REQUIRED[crate])
        problems += check_versions(root, crate, listing, versions)
        problems += check_links(root, crate, listing)
    problems += check_sandbox_tests(listings["libtmux"])
    problems += [
        f"libtmux package contains macro source: {entry}"
        for entry in listings["libtmux"]
        if entry.startswith("libtmux-macros/")
    ]
    problems += check_edges()
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
