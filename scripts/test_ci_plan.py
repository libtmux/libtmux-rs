#!/usr/bin/env python3
"""Tests for `ci-plan.py`, which says which tmux lanes a CI run builds.

Each rule of the plan has a test, and each test fails when its rule is removed.
"""

from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest
from typing import Any

SCRIPTS = pathlib.Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
MANIFEST = ROOT / ".github" / "ci-matrix.txt"

SPEC = importlib.util.spec_from_file_location("ci_plan", SCRIPTS / "ci-plan.py")
assert SPEC is not None and SPEC.loader is not None
ci_plan = importlib.util.module_from_spec(SPEC)
sys.modules["ci_plan"] = ci_plan
SPEC.loader.exec_module(ci_plan)

COMMIT = "0123456789abcdef0123456789abcdef01234567"
OTHER = "fedcba9876543210fedcba9876543210fedcba98"


def lane(kind: str, tag: str, contract: str = "workspace", requires: str = "-") -> str:
    return f"{kind} {tag} {COMMIT} {tag} {contract} {requires}\n"


# A small table with middle releases, a witness, refused releases and a
# development lane, so each rule has something to include or leave out.
TABLE = (
    "# a comment\n\n"
    + lane("supported", "3.3a")
    + lane("supported", "3.4")
    + lane("supported", "3.7").rstrip("\n")
    + "  # trailing comment\n"
    + lane("supported", "3.8")
    + lane("witness", "3.6", "feature-boundary", "3.3a,3.7")
    + lane("refused", "3.2a", "reject-core")
    + lane("refused", "3.3", "reject-boundary")
    + lane("development", "master", "development")
)
LANES = ci_plan.parse_manifest(TABLE)


def run(event: str, **inputs: str) -> Any:
    return ci_plan.plan(LANES, ci_plan.Inputs(event=event, **inputs))


def tags(result: Any) -> list[str]:
    return [leg["lane"] for leg in result.legs]


def refusal(test: unittest.TestCase, event: str, **inputs: str) -> str:
    with test.assertRaises(ci_plan.PlanError) as caught:
        run(event, **inputs)
    return str(caught.exception)


class EventTests(unittest.TestCase):
    def test_a_push_and_a_pull_request_run_only_the_first_and_last_release(
        self,
    ) -> None:
        for event in ("push", "pull_request"):
            with self.subTest(event):
                result = run(event)
                self.assertEqual(tags(result), ["3.3a", "3.8"])
                self.assertEqual(result.scope, "edges")
                self.assertEqual(result.os, "ubuntu-latest")
                self.assertEqual({leg["tmate"] for leg in result.legs}, {"off"})

    def test_the_schedule_and_scope_full_run_every_lane_in_manifest_order(self) -> None:
        every = ["3.3a", "3.4", "3.7", "3.8", "3.6", "3.2a", "3.3", "master"]
        self.assertEqual(tags(run("schedule")), every)
        self.assertEqual(tags(run("workflow_dispatch", scope="full")), every)
        self.assertEqual(run("schedule").scope, "full")

    def test_a_dispatch_defaults_to_the_edges(self) -> None:
        self.assertEqual(tags(run("workflow_dispatch")), ["3.3a", "3.8"])
        self.assertEqual(tags(run("workflow_dispatch", scope="edges")), ["3.3a", "3.8"])

    def test_a_push_pull_request_and_schedule_ignore_every_dispatch_input(self) -> None:
        for event, want in (
            ("push", ["3.3a", "3.8"]),
            ("pull_request", ["3.3a", "3.8"]),
        ):
            with self.subTest(event):
                result = run(
                    event,
                    scope="full",
                    tmux_ref="3.4",
                    rust_toolchain="1.85.0",
                    os="ubuntu-22.04",
                    tmate="before-tests",
                )
                self.assertEqual(tags(result), want)
                self.assertEqual(result.os, "ubuntu-latest")
                self.assertEqual(
                    {(leg["rust-toolchain"], leg["tmate"]) for leg in result.legs},
                    {("", "off")},
                )

    def test_an_event_it_has_no_plan_for_is_refused(self) -> None:
        self.assertIn("no plan for the release event", refusal(self, "release"))

    def test_one_supported_release_is_its_own_first_and_last(self) -> None:
        single = ci_plan.parse_manifest(lane("supported", "3.8"))
        result = ci_plan.plan(single, ci_plan.Inputs(event="push"))
        self.assertEqual([leg["lane"] for leg in result.legs], ["3.8"])

    def test_every_event_plans_at_least_one_lane(self) -> None:
        for event, inputs in (
            ("push", {}),
            ("pull_request", {}),
            ("schedule", {}),
            ("workflow_dispatch", {}),
            ("workflow_dispatch", {"scope": "full"}),
            ("workflow_dispatch", {"scope": "custom", "tmux_ref": "3.2a"}),
        ):
            with self.subTest(event, **inputs):
                self.assertGreater(len(run(event, **inputs).legs), 0)


class PeriodicJobTests(unittest.TestCase):
    def test_the_fuzz_bench_and_parity_jobs_run_on_a_schedule_and_a_dispatch(
        self,
    ) -> None:
        for event in ("schedule", "workflow_dispatch"):
            with self.subTest(event):
                result = run(event)
                self.assertTrue(result.periodic)
                self.assertEqual(result.skip, [])

    def test_they_are_out_of_scope_on_a_push_and_a_pull_request(self) -> None:
        for event in ("push", "pull_request"):
            with self.subTest(event):
                result = run(event)
                self.assertFalse(result.periodic)
                self.assertEqual(result.skip, ["fuzz", "bench", "parity"])


class CustomScopeTests(unittest.TestCase):
    def test_it_runs_the_refs_it_names_once_each_in_the_order_given(self) -> None:
        result = run(
            "workflow_dispatch",
            scope="custom",
            tmux_ref=f" 3.7 , master,feature/x,{COMMIT},3.7,3.2a,3.9",
        )
        self.assertEqual(
            tags(result), ["3.7", "master", "feature/x", COMMIT, "3.2a", "3.9"]
        )
        names = {leg["lane"]: leg["name"] for leg in result.legs}
        self.assertEqual(names[COMMIT], COMMIT[:12])
        self.assertEqual(names["feature/x"], "feature/x")

    def test_the_longest_ref_is_accepted(self) -> None:
        longest = "a" * 64
        result = run("workflow_dispatch", scope="custom", tmux_ref=longest)
        self.assertEqual(tags(result), [longest])

    def test_toolchains_cross_with_the_refs(self) -> None:
        result = run(
            "workflow_dispatch",
            scope="custom",
            tmux_ref="3.4,3.8",
            rust_toolchain="1.85.0, nightly",
            os="ubuntu-24.04",
        )
        self.assertEqual(result.os, "ubuntu-24.04")
        self.assertEqual(
            [(leg["lane"], leg["rust-toolchain"], leg["name"]) for leg in result.legs],
            [
                ("3.4", "1.85.0", "3.4, Rust 1.85.0"),
                ("3.4", "nightly", "3.4, Rust nightly"),
                ("3.8", "1.85.0", "3.8, Rust 1.85.0"),
                ("3.8", "nightly", "3.8, Rust nightly"),
            ],
        )


class RefusalTests(unittest.TestCase):
    def test_what_it_cannot_plan_is_refused(self) -> None:
        custom = {"scope": "custom"}
        for name, inputs, message in (
            ("an unknown scope", {"scope": "everything"}, "scope must be"),
            ("no refs", custom, "tmux-ref is empty"),
            ("blank refs", {**custom, "tmux_ref": "  "}, "tmux-ref is empty"),
            ("an empty item", {**custom, "tmux_ref": "3.8,,3.7"}, "empty item"),
            ("a trailing comma", {**custom, "tmux_ref": "3.8,"}, "empty item"),
            ("a shell metacharacter", {**custom, "tmux_ref": "3.8;id"}, "invalid item"),
            ("a command substitution", {**custom, "tmux_ref": "$(id)"}, "invalid item"),
            (
                "a space inside an item",
                {**custom, "tmux_ref": "3.8 3.7"},
                "invalid item",
            ),
            ("an item too long", {**custom, "tmux_ref": "a" * 65}, "invalid item"),
            ("a leading dash", {**custom, "tmux_ref": "-abc"}, "invalid item"),
            ("an unknown runner", {"os": "macos-latest"}, "os must be"),
            ("an unknown tmate mode", {"tmate": "always"}, "tmate must be"),
            ("a bad toolchain", {"rust_toolchain": "1.85.0;id"}, "invalid item"),
            (
                "an empty toolchain item",
                {"rust_toolchain": "1.85.0,,nightly"},
                "empty item",
            ),
            ("a toolchain with a dash", {"rust_toolchain": "-1.85.0"}, "invalid item"),
        ):
            with self.subTest(name):
                self.assertIn(message, refusal(self, "workflow_dispatch", **inputs))


class TmateTests(unittest.TestCase):
    def test_it_goes_to_the_one_lane_that_runs(self) -> None:
        result = run(
            "workflow_dispatch", scope="custom", tmux_ref="3.8", tmate="on-failure"
        )
        self.assertEqual([leg["tmate"] for leg in result.legs], ["on-failure"])

    def test_it_is_refused_for_more_than_one_lane(self) -> None:
        for name, inputs in (
            (
                "two refs",
                {"scope": "custom", "tmux_ref": "3.7,3.8", "tmate": "before-tests"},
            ),
            (
                "two toolchains",
                {
                    "scope": "custom",
                    "tmux_ref": "3.8",
                    "rust_toolchain": "1.85.0,nightly",
                    "tmate": "on-failure",
                },
            ),
            ("the edges", {"scope": "edges", "tmate": "on-failure"}),
            ("every lane", {"scope": "full", "tmate": "on-failure"}),
        ):
            with self.subTest(name):
                self.assertIn(
                    "tmate needs exactly one tmux lane",
                    refusal(self, "workflow_dispatch", **inputs),
                )


class ManifestTests(unittest.TestCase):
    def test_what_it_cannot_trust_is_refused(self) -> None:
        valid = lane("supported", "3.3a")
        for name, text, message in (
            ("nothing", "# only a comment\n", "lists no lane"),
            (
                "no supported release",
                lane("refused", "3.2a", "reject-core"),
                "no supported",
            ),
            ("an unknown kind", valid + lane("preview", "3.9"), "unknown lane kind"),
            ("a short line", valid + "supported 3.4\n", "malformed"),
            (
                "an extra field",
                valid + lane("supported", "3.4").rstrip("\n") + " x\n",
                "malformed",
            ),
            ("a repeat", valid + valid, "listed twice"),
            (
                "a short commit",
                valid + "supported 3.4 abc123 3.4 workspace -\n",
                "40-hex commit",
            ),
            ("an invalid tag", valid + lane("supported", "3.4;id"), "invalid lane tag"),
            ("a leading dash", valid + lane("supported", "-3.4"), "invalid lane tag"),
            (
                "a prerequisite it does not list",
                valid + lane("supported", "3.4", requires="3.9"),
                "requires 3.9",
            ),
        ):
            with self.subTest(name):
                with self.assertRaises(ci_plan.PlanError) as caught:
                    ci_plan.parse_manifest(text)
                self.assertIn(message, str(caught.exception))


class VerifyTests(unittest.TestCase):
    def test_only_a_job_the_plan_left_out_may_be_skipped(self) -> None:
        passed = {"result": "success"}
        for name, needs, allowed, want in (
            ("every job passed", {"plan": passed, "check": passed}, [], []),
            (
                "a job the plan left out was skipped",
                {"plan": passed, "fuzz": {"result": "skipped"}},
                ["fuzz", "bench"],
                [],
            ),
            (
                "a job failed",
                {"plan": passed, "check": {"result": "failure"}},
                [],
                ["check: failure"],
            ),
            (
                "a job was cancelled",
                {"plan": passed, "check": {"result": "cancelled"}},
                [],
                ["check: cancelled"],
            ),
            (
                "a job the plan kept in scope was skipped",
                {"plan": passed, "compat": {"result": "skipped"}},
                ["fuzz"],
                ["compat: skipped"],
            ),
            (
                "the plan failed and skipped the rest",
                {"plan": {"result": "failure"}, "compat": {"result": "skipped"}},
                [],
                ["plan: failure", "compat: skipped"],
            ),
            (
                "a job with no result",
                {"plan": passed, "check": {}},
                [],
                ["check: null"],
            ),
        ):
            with self.subTest(name):
                self.assertEqual(ci_plan.unpassed(needs, allowed), want)

    def test_a_run_with_no_jobs_is_an_error(self) -> None:
        with self.assertRaises(ci_plan.PlanError):
            ci_plan.unpassed({}, [])


class CommandTests(unittest.TestCase):
    def invoke(self, environment: dict[str, str], *argv: str) -> tuple[int, str, str]:
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "output"
            output.write_text("", encoding="utf-8")
            env = {"PATH": os.environ["PATH"], "GITHUB_OUTPUT": str(output)}
            env.update(environment)
            result = subprocess.run(
                [sys.executable, str(SCRIPTS / "ci-plan.py"), *argv],
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )
            return result.returncode, output.read_text(encoding="utf-8"), result.stderr

    def test_it_writes_the_plan_to_github_output(self) -> None:
        code, output, _ = self.invoke({"GITHUB_EVENT_NAME": "pull_request"})
        self.assertEqual(code, 0)
        fields = dict(line.split("=", 1) for line in output.splitlines())
        self.assertEqual(fields["scope"], "edges")
        self.assertEqual(fields["periodic"], "false")
        self.assertEqual(fields["skip"], "fuzz bench parity")
        self.assertEqual(fields["jobs"], "2")
        self.assertEqual(
            [leg["lane"] for leg in json.loads(fields["lanes"])], ["3.3a", "3.8"]
        )

    def test_it_reads_its_inputs_from_the_environment_and_refuses_a_bad_one(
        self,
    ) -> None:
        code, output, stderr = self.invoke(
            {
                "GITHUB_EVENT_NAME": "workflow_dispatch",
                "INPUT_SCOPE": "custom",
                "INPUT_TMUX_REF": "3.8;id",
            }
        )
        self.assertEqual(code, 1)
        self.assertIn("invalid item", stderr)
        self.assertEqual(output, "")

    def test_verify_fails_on_a_failed_job(self) -> None:
        code, _, stderr = self.invoke(
            {"NEEDS_JSON": '{"check": {"result": "failure"}}'}, "verify"
        )
        self.assertEqual(code, 1)
        self.assertIn("check: failure", stderr)
        code, _, _ = self.invoke(
            {"NEEDS_JSON": '{"check": {"result": "success"}}'}, "verify"
        )
        self.assertEqual(code, 0)


RELEASE = re.compile(r"^(\d+)\.(\d+)([a-z]?)$")


def release_key(name: str) -> tuple[int, int, str]:
    match = RELEASE.fullmatch(name)
    assert match is not None, f"{name} is not a tmux release"
    return int(match.group(1)), int(match.group(2)), match.group(3)


class RealManifestTests(unittest.TestCase):
    lanes = ci_plan.parse_manifest(MANIFEST.read_text(encoding="utf-8"))

    def test_supported_releases_run_oldest_first_and_the_edges_are_the_ends(
        self,
    ) -> None:
        supported = [lane.tag for lane in self.lanes if lane.kind == "supported"]
        self.assertEqual(supported, sorted(supported, key=release_key))
        result = ci_plan.plan(self.lanes, ci_plan.Inputs(event="pull_request"))
        self.assertEqual(
            [leg["lane"] for leg in result.legs], [supported[0], supported[-1]]
        )

    def test_the_bash_loader_and_the_plan_read_the_same_lanes(self) -> None:
        script = (
            f"source {SCRIPTS / 'compat-lanes.sh'}; load_lanes {MANIFEST} || exit 1; "
            'for tag in "${lane_tags[@]}"; do '
            'echo "${lane_kind[$tag]} $tag ${lane_commit[$tag]} ${lane_version[$tag]} '
            '${lane_contract[$tag]} ${lane_requires[$tag]}"; done'
        )
        result = subprocess.run(
            ["bash", "-c", script], capture_output=True, text=True, check=True
        )
        from_bash = [tuple(line.split()) for line in result.stdout.splitlines()]
        from_plan = [
            (
                lane.kind,
                lane.tag,
                lane.commit,
                lane.version,
                lane.contract,
                ",".join(lane.requires) or "-",
            )
            for lane in self.lanes
        ]
        self.assertEqual(from_bash, from_plan)

    def test_each_kind_has_a_contract_the_script_runs(self) -> None:
        allowed = {
            "supported": {"workspace", "format-catalog"},
            "witness": {"feature-boundary"},
            "refused": {"reject-core", "reject-boundary", "reject-pre-probe"},
            "development": {"development"},
        }
        script = (SCRIPTS / "test-tmux-format-compat.sh").read_text(encoding="utf-8")
        for lane in self.lanes:
            with self.subTest(lane.tag):
                self.assertIn(lane.contract, allowed[lane.kind])
                if lane.contract != "workspace":
                    self.assertIn(f'"{lane.contract}"', script)


if __name__ == "__main__":
    unittest.main()
