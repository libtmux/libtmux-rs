#!/usr/bin/env python3
"""Tests that hold the CI workflow and the documents about it to the manifest.

The workflow takes its tmux lanes from the plan, which reads
`.github/ci-matrix.txt`, so nothing here compares a list in one file with a
list in another. What is checked is that no second list has grown back, that
every job is behind the one required check, that no step calls `apt-get`
without `scripts/apt-get.sh`, and that the documents state the range the
manifest tests.
"""

from __future__ import annotations

import importlib.util
import pathlib
import re
import sys
import unittest

import yaml

SCRIPTS = pathlib.Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
WORKFLOWS = ROOT / ".github" / "workflows"
ACTIONS = ROOT / ".github" / "actions"

SPEC = importlib.util.spec_from_file_location("ci_plan", SCRIPTS / "ci-plan.py")
assert SPEC is not None and SPEC.loader is not None
ci_plan = importlib.util.module_from_spec(SPEC)
sys.modules["ci_plan"] = ci_plan
SPEC.loader.exec_module(ci_plan)

CI = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
JOB = re.compile(r"^  ([a-z][a-z-]*):\s*$", re.MULTILINE)
RELEASE = re.compile(r"\b\d+\.\d+[a-z]?\b")
TMATE = "mxschmitt/action-tmate@"
# `apt` or `apt-get` in command position, past `sudo` and `VAR=value` prefixes.
# `./scripts/apt-get.sh` never matches: the name follows a `/` and a `.`.
RAW_APT = re.compile(
    r"(?:^|[;&|(`]|\b(?:then|do|else)\b)\s*"
    r"(?:\w+=\S*\s+)*(?:sudo\s+(?:-\S+\s+)*)?(?:\w+=\S*\s+)*apt(?:-get)?(?=\s|$)"
)


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


def run_scripts(doc: object, path: str) -> list[tuple[str, str]]:
    """Return a location and the `run` script of every step in a workflow or action."""
    if not isinstance(doc, dict):
        return []
    found = []
    for job, body in (doc.get("jobs") or {}).items():
        for number, step in enumerate(body.get("steps") or [], 1):
            if "run" in step:
                label = step.get("name") or f"#{number}"
                found.append((f"{path}: job {job}, step {label}", step["run"]))
    for number, step in enumerate((doc.get("runs") or {}).get("steps") or [], 1):
        if "run" in step:
            label = step.get("name") or f"#{number}"
            found.append((f"{path}: step {label}", step["run"]))
    return found


def raw_apt_calls(doc: object, path: str) -> list[str]:
    """Describe each `run` line that calls `apt` or `apt-get` directly."""
    found = []
    for where, script in run_scripts(doc, path):
        for line in script.splitlines():
            if not line.lstrip().startswith("#") and RAW_APT.search(line):
                found.append(
                    f"{where} runs `{line.strip()}` directly; "
                    "run it through scripts/apt-get.sh"
                )
    return found


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
    def test_off_is_quoted_in_the_options_and_the_default(self) -> None:
        # GitHub reads a bare `off` as false. A default that is not one of the
        # options refuses every dispatch that sets nothing.
        self.assertIn("options: ['off', before-tests, on-failure]", CI)
        self.assertRegex(CI, r"(?m)^        default: ['\"]off['\"]$")

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


class AptTests(unittest.TestCase):
    def test_no_step_calls_apt_without_the_wrapper(self) -> None:
        paths = sorted(WORKFLOWS.glob("*.yml")) + sorted(ACTIONS.glob("*/action.yml"))
        self.assertIn(WORKFLOWS / "ci.yml", paths)
        found = []
        for path in paths:
            doc = yaml.safe_load(path.read_text(encoding="utf-8"))
            found += raw_apt_calls(doc, path.relative_to(ROOT).as_posix())
        self.assertEqual(found, [])

    def test_a_raw_apt_get_is_named_with_its_fix(self) -> None:
        doc = yaml.safe_load(
            """
jobs:
  build:
    steps:
      - name: Install tmux
        run: |
          # sudo apt-get update is only mentioned here
          sudo apt-get update
          ./scripts/apt-get.sh install -y tmux
"""
        )
        self.assertEqual(
            raw_apt_calls(doc, "ci.yml"),
            [
                "ci.yml: job build, step Install tmux runs `sudo apt-get update` "
                "directly; run it through scripts/apt-get.sh"
            ],
        )

    def test_the_wrapper_passes(self) -> None:
        doc = yaml.safe_load(
            """
jobs:
  build:
    steps:
      - run: ./scripts/apt-get.sh update
"""
        )
        self.assertEqual(raw_apt_calls(doc, "ci.yml"), [])

    def test_every_spelling_of_a_raw_call_is_caught(self) -> None:
        for call in (
            "apt update",
            "sudo -E apt-get install -y tmux",
            "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y tmux",
            "test -x /usr/bin/tmux || sudo apt-get install -y tmux",
        ):
            with self.subTest(call):
                action = {"runs": {"steps": [{"run": call}]}}
                self.assertEqual(
                    raw_apt_calls(action, "action.yml"),
                    [
                        f"action.yml: step #1 runs `{call}` directly; "
                        "run it through scripts/apt-get.sh"
                    ],
                )


class DocumentTests(unittest.TestCase):
    lanes = ci_plan.parse_manifest(
        (ROOT / ".github" / "ci-matrix.txt").read_text(encoding="utf-8")
    )

    def tags(self, *kinds: str) -> set[str]:
        return {lane.tag for lane in self.lanes if lane.kind in kinds}

    def test_the_readme_states_the_supported_releases_the_manifest_tests(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        start = readme.index("- **tmux 3.3a or newer.**")
        bullet = readme[start : readme.index("\n- **", start + 1)]
        self.assertEqual(set(RELEASE.findall(bullet)), self.tags("supported"))

    def test_contributing_states_every_release_the_manifest_builds(self) -> None:
        text = (ROOT / ".github" / "CONTRIBUTING.md").read_text(encoding="utf-8")
        start = text.index("\n## Compatibility\n")
        end = text.find("\n## ", start + 1)
        section = text[start:] if end == -1 else text[start:end]
        bold = " ".join(re.findall(r"\*\*([^*]+)\*\*", section))
        self.assertEqual(
            set(RELEASE.findall(bold)), self.tags("supported", "witness", "refused")
        )


if __name__ == "__main__":
    unittest.main()
