#!/usr/bin/env python3
"""Tests that hold the CI workflow and the documents about it to the manifest.

The workflow takes its tmux lanes from the plan, which reads
`.github/ci-matrix.txt`, so nothing here compares a list in one file with a
list in another. What is checked is that no second list has grown back, that
every job is behind the one required check, and that the documents state the
range the manifest tests.
"""

from __future__ import annotations

import importlib.util
import pathlib
import re
import sys
import unittest

SCRIPTS = pathlib.Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
WORKFLOWS = ROOT / ".github" / "workflows"

SPEC = importlib.util.spec_from_file_location("ci_plan", SCRIPTS / "ci-plan.py")
assert SPEC is not None and SPEC.loader is not None
ci_plan = importlib.util.module_from_spec(SPEC)
sys.modules["ci_plan"] = ci_plan
SPEC.loader.exec_module(ci_plan)

CI = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
JOB = re.compile(r"^  ([a-z][a-z-]*):\s*$", re.MULTILINE)


def job_section(text: str, name: str) -> str:
    _, _, jobs = text.partition("\njobs:\n")
    headings = list(JOB.finditer(jobs))
    for index, heading in enumerate(headings):
        if heading.group(1) == name:
            end = (
                headings[index + 1].start() if index + 1 < len(headings) else len(jobs)
            )
            return jobs[heading.start() : end]
    raise AssertionError(f"ci.yml has no {name} job")


class WorkflowTests(unittest.TestCase):
    def test_the_compatibility_matrix_comes_from_the_plan(self) -> None:
        compat = job_section(CI, "compat")
        self.assertIn("include: ${{ fromJSON(needs.plan.outputs.lanes) }}", compat)
        self.assertIn("runs-on: ${{ needs.plan.outputs.os }}", compat)
        self.assertIsNone(
            re.search(r"^\s+(?:lane|tmux-version|tag)s?:\s*\[", CI, re.MULTILINE),
            "ci.yml lists tmux versions inline; they belong in .github/ci-matrix.txt",
        )

    def test_the_compatibility_job_runs_the_lane_the_plan_gave_it(self) -> None:
        compat = job_section(CI, "compat")
        self.assertIn("LIBTMUX_COMPAT_LANES: ${{ matrix.lane }}", compat)
        self.assertIn("just compat", compat)

    def test_the_periodic_jobs_run_when_the_plan_says_so(self) -> None:
        for name in ci_plan.PERIODIC_JOBS:
            with self.subTest(name):
                section = job_section(CI, name)
                self.assertIn("needs: plan", section)
                self.assertIn("if: needs.plan.outputs.periodic == 'true'", section)

    def test_inputs_reach_the_plan_only_through_the_environment(self) -> None:
        for line in CI.splitlines():
            if "${{ inputs." in line:
                self.assertTrue(
                    line.strip().startswith("INPUT_"),
                    f"an input reaches the workflow outside an INPUT_ entry: {line.strip()}",
                )

    def test_the_weekly_schedule_stays_the_only_one(self) -> None:
        self.assertEqual(len(re.findall(r"^\s+- cron:", CI, re.MULTILINE)), 1)

    def test_a_dispatch_takes_the_documented_inputs(self) -> None:
        _, _, after = CI.partition("  workflow_dispatch:\n    inputs:\n")
        block = after.split("\n  # Scheduled weekly", 1)[0]
        names = re.findall(r"^      ([a-z-]+):\s*$", block, re.MULTILINE)
        self.assertEqual(names, ["scope", "tmux-ref", "rust-toolchain", "os"])

    def test_a_release_does_not_wait_on_the_narrowed_lanes(self) -> None:
        release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
        self.assertNotIn("ci.yml", release)
        self.assertNotIn("ci-matrix", release)


if __name__ == "__main__":
    unittest.main()
