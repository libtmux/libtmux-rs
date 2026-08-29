#!/usr/bin/env python3
"""Check that every done parity row names a caller-reachable Rust path.

The checked public-API ledger comes from rustdoc JSON and preserves each
associated item's owner. Private implementation details may explain a row but
do not prove a public parity claim.
"""

from __future__ import annotations

import pathlib
import re
import sys


DONE = {"implemented", "verified"}
# Python names may add context, but none count as Rust evidence.
EXPECTED_ABSENT = {
    "LibTmuxException",
    "QueryList",
    "UnknownOption",
    "UserWarning",
    "ValueError",
    "exc",
    "remove_environment",
    "run_hook",
}
IDENTIFIER = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
CODE_PATH = re.compile(r"^(?:[A-Za-z_][A-Za-z0-9_]*::)*[A-Za-z_][A-Za-z0-9_]*")
PATH_SUFFIX = re.compile(r"^(?:<|\(|\s*->|\s*\{|::\{|$)")
def public_paths(roots: list[pathlib.Path]) -> set[str]:
    """Caller-reachable paths recorded beside the crate source."""
    paths: set[str] = set()
    ledgers = [root.parent / "docs/public-api.txt" for root in roots]
    for ledger in ledgers:
        if not ledger.is_file():
            continue
        for line in ledger.read_text(encoding="utf-8").splitlines():
            _, separator, path = line.partition(" ")
            if not separator or "::" not in path:
                continue
            paths.add(path.split("::", 1)[1])
    return paths


def mentioned_names(roots: list[pathlib.Path]) -> set[str]:
    """Identifiers present anywhere in Rust source."""
    names: set[str] = set()
    for root in roots:
        for path in root.rglob("*.rs"):
            names.update(WORD.findall(path.read_text(encoding="utf-8")))
    return names


def named(quoted: str) -> list[str]:
    """Identifier components named by one code span."""
    head = quoted.split("(")[0]
    parts = (part.strip().removesuffix("!").strip() for part in head.split("::"))
    return [part for part in parts if IDENTIFIER.match(part)]


def claimed_path(quoted: str) -> str | None:
    """Return a canonical Rust path when a code span starts with one."""
    quoted = quoted.removeprefix("libtmux::")
    match = CODE_PATH.match(quoted)
    if match is None or PATH_SUFFIX.match(quoted[match.end() :]) is None:
        return None
    return match.group().removesuffix("::")


def is_public(path: str, available: set[str]) -> bool:
    """Whether rustdoc records this path directly or below its public module."""
    if path in available:
        return True
    return ("::" in path or path[:1].isupper()) and any(
        item.endswith(f"::{path}") for item in available
    )


def main(ledger: str, crates: str) -> int:
    document = pathlib.Path(ledger).read_text(encoding="utf-8")
    roots = sorted(path for path in pathlib.Path(crates).glob("*/src") if path.is_dir())
    if not roots:
        print(f"no crate sources under {crates}", file=sys.stderr)
        return 2

    available = public_paths(roots)
    if not available:
        print(f"no public API ledger under {crates}", file=sys.stderr)
        return 2
    public_owners: set[str] = set()
    for path in available:
        parts = path.split("::")
        public_owners.update(parts[:-1] or parts)
    mentioned = mentioned_names(roots)

    invalid: list[tuple[int, str, str]] = []
    missing: list[tuple[int, str, str]] = []
    unchecked: list[tuple[int, str]] = []
    done = 0
    rust = 3
    for number, line in enumerate(document.splitlines(), start=1):
        if not line.startswith("|"):
            continue
        columns = [cell.strip() for cell in line.split("|")]
        if len(columns) < 6:
            continue
        if columns[-2] == "Status":
            heading = next((i for i, cell in enumerate(columns) if "Rust" in cell), None)
            if heading is not None:
                rust = heading
            continue
        if columns[-2].strip("`").strip() not in DONE:
            continue

        done += 1
        found = False
        for quoted in re.findall(r"`([^`]+)`", columns[rust]):
            path = claimed_path(quoted)
            if path is not None and is_public(path, available):
                found = True
                continue
            if (
                path is not None
                and "::" in path
                and path.split("::", 1)[0] in public_owners
            ):
                invalid.append((number, path, columns[1]))
            names = [name for name in named(quoted) if name not in EXPECTED_ABSENT]
            for name in names:
                if name not in mentioned:
                    missing.append((number, quoted, columns[1]))
        if not found:
            unchecked.append((number, columns[1]))

    for number, path, row in invalid:
        print(f"{ledger}:{number}: `{path}` is not a public Rust path ({row})")
    for number, quoted, row in missing:
        print(f"{ledger}:{number}: `{quoted}` names no Rust source symbol ({row})")
    for number, row in unchecked:
        print(f"{ledger}:{number}: done row names no public Rust path ({row})")

    if invalid or missing or unchecked:
        print(
            f"\n{len(invalid)} invalid path(s); {len(missing)} missing symbol(s); "
            f"{len(unchecked)} done row(s) unchecked.",
            file=sys.stderr,
        )
        return 1
    print(
        f"every done row names a public Rust path "
        f"({done} done rows, {len(available)} public paths checked)"
    )
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("usage: check-parity-claims.py <parity.md> <crates-dir>", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1], sys.argv[2]))
