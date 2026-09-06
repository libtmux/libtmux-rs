#!/usr/bin/env python3
"""Fail when a documented example's output no longer matches the example.

A block of program output pasted into prose is a copy, and a copy drifts
silently: the example gains a column, the prose keeps rendering, and nothing
here fails. What a reader is shown is then a table that no command produces.

The block says which example it came from, so this runs that example and
compares. Wall-clock columns are excluded from the comparison and checked for
shape instead: a timing that reproduced exactly would mean the number was not
measured. Everything else -- dispatch counts, process counts, attribution,
query results, the lines around the table -- is exact, and a difference there
is the drift this exists to catch.

Usage:

    check-example-tables.py [README.md ...]
"""

from __future__ import annotations

import pathlib
import re
import shutil
import subprocess
import sys

MARKER = re.compile(r"<!--\s*example-output:\s*(?P<argv>[^>]+?)\s*-->")
FENCE = re.compile(r"```text\n(?P<body>.*?)```", re.DOTALL)

# A cell holding a measurement rather than a fact about the run. Recognised by
# shape rather than by column, because the example aligns its header with
# single spaces in places and no split of it lines up with the rows beneath.
# Two such cells match each other whatever they read: a timing that reproduced
# exactly would mean the number was not measured.
TIMING_CELL = re.compile(r"^\d+(\.\d+)?(ms|s|µs|us)$")


class Drift(Exception):
    """A documented block and its example disagree."""


def blocks(text: str) -> list[tuple[int, str, str]]:
    """Yield `(line number, example argv, block body)` for each marked block."""
    found = []
    for marker in MARKER.finditer(text):
        # Anchored to what follows the marker rather than searched for, so a
        # marker whose block was deleted claims the next one down the page
        # instead of reporting itself missing.
        rest = text[marker.end() :]
        fence = FENCE.match(rest.lstrip())
        if fence is None:
            raise Drift(
                f"the marker for `{marker.group('argv')}` is followed by no "
                "```text block"
            )
        line = text.count("\n", 0, marker.start()) + 1
        found.append((line, marker.group("argv"), fence.group("body")))
    return found


def run(argv: str) -> str:
    """Run one example and return what it printed."""
    command = ["cargo", "run", "--quiet", "--example", *argv.split()]
    result = subprocess.run(
        command,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise Drift(
            f"`{' '.join(command)}` exited {result.returncode}:\n{result.stderr}"
        )
    return result.stdout


def cells(line: str) -> list[str]:
    """Split one table row into its columns.

    The example aligns with runs of spaces, and a cell may hold single spaces
    of its own -- `2 panes, 2 windows, 2 active` is one column, not five.
    """
    return re.split(r"\s{2,}", line.strip())


def comparable(left: str, right: str) -> bool:
    """Whether two cells agree, treating any two timings as agreeing."""
    if left == right:
        return True
    return bool(TIMING_CELL.match(left) and TIMING_CELL.match(right))


def compare(documented: str, produced: str, argv: str, line: int) -> list[str]:
    """Report every way the documented block differs from the real output."""
    shown = documented.strip("\n").splitlines()
    actual = produced.strip("\n").splitlines()

    if not shown:
        return [f"README:{line}: the block for `{argv}` is empty"]

    problems = []
    if len(shown) != len(actual):
        problems.append(
            f"README:{line}: `{argv}` printed {len(actual)} lines, the block "
            f"shows {len(shown)}"
        )

    for offset, (want, got) in enumerate(zip(shown, actual)):
        if want == got:
            continue

        want_cells, got_cells = cells(want), cells(got)
        if len(want_cells) == len(got_cells) and all(
            comparable(left, right)
            for left, right in zip(want_cells, got_cells)
        ):
            continue

        problems.append(
            f"README:{line + offset}: `{argv}` printed\n"
            f"    {got}\n"
            f"  where the block shows\n"
            f"    {want}"
        )

    return problems


def main(paths: list[str]) -> int:
    if shutil.which("cargo") is None:
        print("cargo is not on PATH", file=sys.stderr)
        return 1

    problems: list[str] = []
    checked = 0
    for name in paths:
        text = pathlib.Path(name).read_text(encoding="utf-8")
        try:
            for line, argv, documented in blocks(text):
                checked += 1
                problems.extend(compare(documented, run(argv), argv, line))
        except Drift as drift:
            problems.append(f"{name}: {drift}")

    if problems:
        for problem in problems:
            print(problem, file=sys.stderr)
        print(
            "\nA documented block no longer matches what its example prints. "
            "Re-run the example and paste its output, or fix the example if "
            "the block is what the reader should see.",
            file=sys.stderr,
        )
        return 1

    print(f"{checked} example output block(s) match")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:] or ["README.md"]))
