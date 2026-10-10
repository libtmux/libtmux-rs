#!/usr/bin/env python3
"""Repeat the real-tmux test suite and report each test's failure count.

A test that fails one run in twenty reads as noise in a single CI run and as a
rate here. The suite is built once, then run `--repeat` times with
`--no-fail-fast`, so one failing test does not hide the ones after it. Each
run's libtest output is parsed for `test <name> ... ok|FAILED`, keyed by the
test binary that printed it, and the counts are written as a CSV and a
Markdown table.

A run that fails with no `FAILED` line (a build error, a crash before libtest
reports) is counted under the name `<run failed before any test reported>`, so
it cannot read as a green run.

Exit status is 0 whatever the counts, unless `--fail-on-failure` is given: the
scheduled job reports a rate, and the pull-request gate is a different job.
"""

from __future__ import annotations

import argparse
import collections
import csv
import pathlib
import re
import subprocess
import sys

RUNNING = re.compile(r"^\s*(?:Running|Doc-tests)\s+(.+?)(?:\s+\(.*\))?\s*$")
RESULT = re.compile(r"^test (.+) \.\.\. (ok|FAILED|ignored)\b")
UNREPORTED = "<run failed before any test reported>"


def parse(output: str) -> dict[str, str]:
    """Map `binary::test` to `ok`, `FAILED` or `ignored` for one run's output."""
    binary = "?"
    results: dict[str, str] = {}
    for line in output.splitlines():
        running = RUNNING.match(line)
        if running:
            binary = running.group(1)
            continue
        result = RESULT.match(line)
        if result:
            results[f"{binary}::{result.group(1)}"] = result.group(2)
    return results


def tally(runs: list[tuple[int, dict[str, str]]]) -> list[tuple[str, int, int]]:
    """(test, runs it was seen in, runs it failed) for every test that failed."""
    seen: collections.Counter[str] = collections.Counter()
    failed: collections.Counter[str] = collections.Counter()
    for status, results in runs:
        for name, outcome in results.items():
            if outcome == "ignored":
                continue
            seen[name] += 1
            if outcome == "FAILED":
                failed[name] += 1
        if status != 0 and "FAILED" not in results.values():
            seen[UNREPORTED] += 1
            failed[UNREPORTED] += 1
    return sorted(
        ((name, seen[name], count) for name, count in failed.items()),
        key=lambda row: (-row[2], row[0]),
    )


def table(rows: list[tuple[str, int, int]], repeat: int, label: str) -> str:
    lines = [f"### {label}: {repeat} repetitions", ""]
    if not rows:
        lines.append(f"No test failed in {repeat} runs.")
        return "\n".join(lines) + "\n"
    lines += ["| Test | Failed | Of runs |", "| --- | ---: | ---: |"]
    lines += [f"| `{name}` | {count} | {seen} |" for name, seen, count in rows]
    return "\n".join(lines) + "\n"


def run_suite(command: list[str], repeat: int, log: pathlib.Path) -> list[tuple[int, dict[str, str]]]:
    runs = []
    with log.open("w") as sink:
        for index in range(1, repeat + 1):
            done = subprocess.run(
                command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace"
            )
            sink.write(f"===== run {index} exit {done.returncode}\n{done.stdout}\n")
            sink.flush()
            results = parse(done.stdout)
            failures = sum(1 for outcome in results.values() if outcome == "FAILED")
            print(f"run {index}/{repeat}: exit {done.returncode}, {failures} failed", flush=True)
            runs.append((done.returncode, results))
    return runs


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--repeat", type=int, default=10)
    parser.add_argument("--label", default="tests")
    parser.add_argument("--csv", type=pathlib.Path, default=pathlib.Path("stress-failures.csv"))
    parser.add_argument("--markdown", type=pathlib.Path, default=pathlib.Path("stress-summary.md"))
    parser.add_argument("--log", type=pathlib.Path, default=pathlib.Path("stress.log"))
    parser.add_argument("--fail-on-failure", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER, help="after --; the test command")
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or args.repeat < 1:
        parser.error("a command and a positive --repeat are required")

    runs = run_suite(command, args.repeat, args.log)
    rows = tally(runs)
    with args.csv.open("w", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(["label", "test", "runs", "failed"])
        writer.writerows((args.label, name, seen, count) for name, seen, count in rows)
    args.markdown.write_text(table(rows, args.repeat, args.label))
    print(args.markdown.read_text())
    return 1 if rows and args.fail_on_failure else 0


if __name__ == "__main__":
    sys.exit(main())
