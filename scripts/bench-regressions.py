#!/usr/bin/env python3
"""Report criterion's change against the previous run, and fail on a doubling.

Criterion compares each run with the last one it saved under `target/criterion`
and writes the relative change of the mean to `change/estimates.json`. On a
shared runner a few tens of percent either way is weather, so this gates only
on a benchmark that got more than `--limit` times slower -- the size of an
algorithmic regression, not of a noisy neighbour -- and prints every change so
a slower drift is still visible.

The first run has nothing to compare with and passes.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import sys


def changes(root: pathlib.Path) -> list[tuple[str, float, float]]:
    """(benchmark, relative change of the mean, new mean in ns) for each one."""
    found = []
    for estimate in sorted(root.glob("**/change/estimates.json")):
        bench = estimate.parent.parent
        new = bench / "new" / "estimates.json"
        if not new.is_file():
            continue
        change = json.loads(estimate.read_text())["mean"]["point_estimate"]
        mean = json.loads(new.read_text())["mean"]["point_estimate"]
        found.append((str(bench.relative_to(root)), change, mean))
    return found


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=pathlib.Path)
    parser.add_argument("--limit", type=float, default=2.0)
    args = parser.parse_args(argv)

    found = changes(args.root)
    if not found:
        print(f"no earlier run under {args.root} to compare with")
        return 0
    rows = ["| Benchmark | Mean | Change |", "| --- | ---: | ---: |"]
    slower = []
    for name, change, mean in found:
        rows.append(f"| `{name}` | {mean / 1e6:.3f} ms | {change:+.1%} |")
        if 1 + change > args.limit:
            slower.append(f"{name} is {1 + change:.2f}x its previous mean")
    report = "\n".join(rows)
    print(report)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(report + "\n")
    for line in slower:
        print(f"regressed: {line}", file=sys.stderr)
    return 1 if slower else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
