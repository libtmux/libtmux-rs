#!/usr/bin/env python3
"""Fail when a doc comment has been split across two items.

A doc comment is an attribute, so neither rustdoc nor the compiler checks that
the prose describes the item it precedes. A block inserted one line too high
lands inside the previous item's comment, and what renders is one type wearing
another's summary.

A split lands mid-sentence, so the item inheriting the remainder opens with a
fragment. That is the only tell, and it is why a summary must begin like a
sentence.

A split falling exactly on a sentence boundary leaves both halves reading as
prose; no textual rule separates that from deliberate wording.
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


def offenders(path: pathlib.Path) -> list[str]:
    reported = []
    for number, body in summary_lines(path.read_text().splitlines()):
        if body.startswith(PROPER_NOUNS):
            continue
        # A sentence, a code span, or a quantity all read as a summary.
        if body[0].isupper() or body[0].isdigit() or body[0] == "`":
            continue
        reported.append(f"{path}:{number}: doc opens mid-sentence: {body[:72]}")
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
        f"\n{len(found)} doc comment(s) open mid-sentence. A doc block was "
        "probably split across two items, leaving this one wearing the tail of "
        "its neighbour's prose.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:] or ["crates"]))
