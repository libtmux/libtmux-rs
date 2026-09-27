#!/usr/bin/env python3
"""Report criterion's change against the previous run, and fail on a doubling.

Criterion compares each run with the last one it saved under `target/criterion`
and writes the relative change of the mean to `change/estimates.json`. Two
runs rarely share a machine: a slower runner moves nearly every benchmark
together, 11 of 12 by 50-105% on one observed pair. So each benchmark is judged against the run's
machine factor -- the median change across all of them -- and fails only when
it is more than `--limit` times slower than its neighbours moved: the size of
an algorithmic regression, not of different hardware. A factor past
`--machine-limit` fails on its own, since a slowdown every benchmark shares
would otherwise cancel out. Every change is printed, so a drift below either
limit is still visible.

The first run has nothing to compare with and passes.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import statistics
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
    parser.add_argument("--machine-limit", type=float, default=3.0)
    args = parser.parse_args(argv)

    found = changes(args.root)
    if not found:
        print(f"no earlier run under {args.root} to compare with")
        return 0
    factor = statistics.median(1 + change for _, change, _ in found)
    rows = [
        f"Machine factor: {factor:.2f}x (the median change across {len(found)} benchmarks)",
        "",
        "| Benchmark | Mean | Change | Against the machine |",
        "| --- | ---: | ---: | ---: |",
    ]
    slower = []
    for name, change, mean in found:
        relative = (1 + change) / factor
        rows.append(f"| `{name}` | {mean / 1e6:.3f} ms | {change:+.1%} | {relative:.2f}x |")
        if relative > args.limit:
            slower.append(f"{name} is {relative:.2f}x slower than the run's other benchmarks moved")
    if factor > args.machine_limit:
        slower.append(f"every benchmark moved together by {factor:.2f}x, past {args.machine_limit:g}x")
    if slower:
        # First, so a reader of the summary sees the verdict before the table.
        rows = ["**Regressed:**", ""] + [f"- {line}" for line in slower] + [""] + rows
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
