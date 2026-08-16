#!/usr/bin/env python3
"""Fail when a doc comment has been split across two items.

A doc comment is an attribute, so neither rustdoc nor the compiler checks that
the prose describes the item it precedes. A block inserted one line too high
lands inside the previous item's comment, and what renders is one type wearing
another's summary.

A split lands mid-sentence, so the item inheriting the remainder opens with a
fragment. That is why a summary must begin like a sentence.

A split falling on a sentence boundary reads as prose either way. It leaves a
structural mark instead: the displaced block lands below an attribute of the
item above, so a `///` follows a non-doc `#[...]`. Rust accepts that order and
rustdoc renders it.
"""

from __future__ import annotations

import pathlib
import sys

# Lowercase because that is how each spells its own name.
PROPER_NOUNS = ("tmux", "libtmux", "rustc", "cargo", "macOS", "iTerm")


def summary_lines(lines: list[str]) -> list[tuple[int, str]]:
    """Yield `(line number, text)` for the first line of each doc block."""
    found = []
    for index, line in enumerate(lines):
        stripped = line.strip()
        if not (stripped.startswith("///") or stripped.startswith("//!")):
            continue

        # Only the first line of a block.
        previous = lines[index - 1].strip() if index else ""
        if previous.startswith("///") or previous.startswith("//!"):
            continue

        body = stripped[3:].strip()
        if body:
            found.append((index + 1, body))
    return found


def blocks_below_an_attribute(lines: list[str]) -> list[tuple[int, str]]:
    """Yield `(line number, text)` for each doc block sitting under an attribute.

    A doc comment belongs above its item's attributes, not between them.

    An attribute carrying documentation is exempt: `cfg_attr(..., doc = "...")`
    is how a gated example is written, and prose either side of it is meant.
    """
    found = []
    for index, line in enumerate(lines):
        stripped = line.strip()
        if not stripped.startswith("///"):
            continue
        # Only the first line of a block; the rest inherit its position.
        previous = lines[index - 1].strip() if index else ""
        if previous.startswith("///"):
            continue
        if not previous.startswith("#["):
            continue
        if "doc" in previous:
            continue
        found.append((index + 1, stripped[3:].strip() or "<blank>"))
    return found


def offenders(path: pathlib.Path) -> list[str]:
    reported = []
    lines = path.read_text().splitlines()

    for number, body in summary_lines(lines):
        if body.startswith(PROPER_NOUNS):
            continue
        # A sentence, a code span, or a quantity all read as a summary.
        if body[0].isupper() or body[0].isdigit() or body[0] == "`":
            continue
        reported.append(f"{path}:{number}: doc opens mid-sentence: {body[:72]}")

    for number, body in blocks_below_an_attribute(lines):
        reported.append(f"{path}:{number}: doc sits below an attribute: {body[:72]}")

    return reported


def main(roots: list[str]) -> int:
    found = []
    for root in roots:
        base = pathlib.Path(root)
        paths = [base] if base.is_file() else sorted(base.rglob("*.rs"))
        for path in paths:
            found.extend(offenders(path))

    if not found:
        print("doc blocks intact")
        return 0

    for line in found:
        print(line, file=sys.stderr)
    print(
        f"\n{len(found)} doc comment(s) are misplaced. A doc block was probably "
        "split across two items, leaving one wearing the tail of its "
        "neighbour's prose or sitting below an attribute it does not belong to.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:] or ["crates"]))
