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
TMATE = "mxschmitt/action-tmate@"


def job_ids(text: str) -> list[str]:
    _, _, jobs = text.partition("\njobs:\n")
    return JOB.findall(jobs)


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
        self.assertEqual(names, ["scope", "tmux-ref", "rust-toolchain", "os", "tmate"])

    def test_a_release_does_not_wait_on_the_narrowed_lanes(self) -> None:
        release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
        self.assertNotIn("ci.yml", release)
        self.assertNotIn("ci-matrix", release)


class ConcurrencyTests(unittest.TestCase):
    def test_a_dispatch_adds_its_run_id_to_the_group(self) -> None:
        match = re.search(r"^concurrency:\n  group: (.+)$", CI, re.MULTILINE)
        self.assertIsNotNone(match)
        assert match is not None
        self.assertIn("workflow_dispatch", match.group(1))
        self.assertIn("github.run_id", match.group(1))


class RequiredTests(unittest.TestCase):
    def test_the_required_job_waits_for_every_other_job(self) -> None:
        section = job_section(CI, "required")
        needs = re.search(r"^    needs: \[([^\]]+)\]", section, re.MULTILINE)
        self.assertIsNotNone(needs)
        assert needs is not None
        waits = sorted(entry.strip() for entry in needs.group(1).split(","))
        others = sorted(name for name in job_ids(CI) if name != "required")
        self.assertEqual(waits, others)
        self.assertIn("if: ${{ always() }}", section)
        self.assertIn("ALLOW_SKIPPED: ${{ needs.plan.outputs.skip }}", section)


class TmateTests(unittest.TestCase):
    def test_tmate_is_pinned_limited_and_confined_to_the_compatibility_job(
        self,
    ) -> None:
        uses = CI.count(TMATE)
        self.assertGreater(uses, 0)
        self.assertEqual(len(re.findall(TMATE + r"[0-9a-f]{40}\b", CI)), uses)
        self.assertEqual(CI.count("limit-access-to-actor: true"), uses)
        compat = job_section(CI, "compat")
        self.assertEqual(compat.count(TMATE), uses)
        self.assertIn("timeout-minutes:", compat)

    def test_no_other_workflow_opens_a_session(self) -> None:
        for path in sorted(WORKFLOWS.glob("*.yml")):
            if path.name == "ci.yml":
                continue
            with self.subTest(path.name):
                self.assertNotIn("action-tmate", path.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
