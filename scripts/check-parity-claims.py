#!/usr/bin/env python3
"""Check that a parity row marked done names symbols that exist.

`parity.md` is what this project calls its definition of done, so a row
claiming a type nobody wrote is worse than a row that is merely out of date: a
reader has no way to tell a delivered capability from a described one. Six such
rows named a capability that does not exist anywhere in the workspace, and
five more named a type that was never written.

The check is deliberately crude. It takes the identifiers a row's Rust column
puts in backticks, reduces each to its leaf name, and asks whether that name
appears anywhere in any crate's `src`. That cannot tell a method on the wrong
type from one on the right type, and it is not meant to: what it catches is a
name that exists in the ledger and nowhere else.

A row that deliberately names something absent -- because it records a Python
symbol, or says why a capability stays out -- goes in EXPECTED_ABSENT with its
reason, so the exception is stated rather than silently tolerated.
"""

from __future__ import annotations

import pathlib
import re
import sys

DONE = {"implemented", "verified"}

# Names a done row may mention without the workspace defining them, and why.
EXPECTED_ABSENT = {
    "LibTmuxException": "the Python exception a row is comparing against",
    "ValueError": "a Python exception name",
    "UserWarning": "a Python warning name",
    "QueryList": "the Python collection a row is comparing against",
    "exc": "a Python module name",
    "remove_environment": "recorded as having no separate meaning in Rust",
    "run_hook": "recorded as deliberately out: a library has no use for firing one",
    "UnknownOption": "named by a row explaining there are three kinds and not four",
}

# A backticked cell holds prose as often as code. Only leaf names that look
# like Rust identifiers are worth asking about.
IDENTIFIER = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def named(quoted: str) -> list[str]:
    """The identifiers a backticked cell names.

    Every `::`-separated part is asked about rather than only the last, because
    a row writing `CaptureOutput::{Lines, BufferWritten}` names a type whose
    variant list is not itself an identifier, and the type is the claim.
    """
    head = quoted.split("(")[0]
    parts = (part.strip().removesuffix("!").strip() for part in head.split("::"))
    return [part for part in parts if IDENTIFIER.match(part)]


def defined_names(roots: list[pathlib.Path]) -> set[str]:
    """Every identifier the workspace's sources mention."""
    seen: set[str] = set()
    word = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
    for root in roots:
        for path in root.rglob("*.rs"):
            seen.update(word.findall(path.read_text(encoding="utf-8")))
    return seen


def main(ledger: str, crates: str) -> int:
    document = pathlib.Path(ledger).read_text(encoding="utf-8")
    roots = sorted(p for p in pathlib.Path(crates).glob("*/src") if p.is_dir())
    if not roots:
        print(f"no crate sources under {crates}", file=sys.stderr)
        return 2
    defined = defined_names(roots)

    missing: list[tuple[int, str, str]] = []
    done = 0
    unchecked = 0
    # Which column holds the Rust side is a property of the table, not a
    # constant. `parity.md` carries two shapes -- one keyed by Python symbol
    # and one by Python behaviour -- and a fixed index reads the Rust column of
    # the first and the delivery slice of the second, which names no
    # identifier and so quietly checked nothing for every row of it.
    rust = 3
    for number, line in enumerate(document.splitlines(), start=1):
        if not line.startswith("|"):
            continue
        columns = [cell.strip() for cell in line.split("|")]
        if len(columns) < 6:
            continue
        heading = next(
            (i for i, cell in enumerate(columns) if "Rust" in cell), None
        )
        if heading is not None:
            rust = heading
            continue
        status = columns[-2].strip("`").strip()
        if status not in DONE:
            continue

        done += 1
        # A row whose Rust column is prose names nothing to look up, so this
        # check passes over it without reading anything. Counted rather than
        # skipped in silence: a row that says `implemented` and nothing this
        # can verify is exactly where an overclaim survives, and three of them
        # did -- `list_commands`, `if_shell`, and the `Path` half of the buffer
        # methods, each sharing a row with a sibling that was implemented.
        quotes = re.findall(r"`([^`]+)`", columns[rust])
        if not quotes:
            unchecked += 1
        for quoted in quotes:
            for name in named(quoted):
                if name in EXPECTED_ABSENT or name in defined:
                    continue
                missing.append((number, quoted, columns[1]))

    for number, quoted, row in missing:
        print(f"{ledger}:{number}: `{quoted}` is named by a done row and defined nowhere ({row})")

    if missing:
        print(
            f"\n{len(missing)} name(s) claimed by a row marked done exist only in the ledger.",
            file=sys.stderr,
        )
        return 1
    print(
        f"every name in a done row is defined "
        f"({done} done rows, {len(roots)} crate sources scanned)"
    )
    if unchecked:
        print(
            f"  {unchecked} of those name no Rust identifier, so nothing in them was checked"
        )
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("usage: check-parity-claims.py <parity.md> <crates-dir>", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1], sys.argv[2]))
