"""Tests for `ci_plan.py`, which says which tmux lanes a CI run builds.

Each rule of the plan has a test, and each test fails when its rule is removed.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import sys
from typing import Any

import ci_plan
import compat_lanes
import pytest

SCRIPTS = pathlib.Path(__file__).resolve().parents[2]
ROOT = SCRIPTS.parent
MANIFEST = ROOT / ".github" / "ci-matrix.txt"

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


def refusal(event: str, **inputs: str) -> str:
    with pytest.raises(ci_plan.PlanError) as caught:
        run(event, **inputs)
    return str(caught.value)


@pytest.mark.parametrize("event", ["push", "pull_request"])
def test_a_push_and_a_pull_request_run_only_the_first_and_last_release(event: str) -> None:
    result = run(event)
    assert tags(result) == ["3.3a", "3.8"]
    assert result.scope == "edges"
    assert result.os == "ubuntu-latest"
    assert {leg["tmate"] for leg in result.legs} == {"off"}


def test_the_schedule_and_scope_full_run_every_lane_in_manifest_order() -> None:
    every = ["3.3a", "3.4", "3.7", "3.8", "3.6", "3.2a", "3.3", "master"]
    assert tags(run("schedule")) == every
    assert tags(run("workflow_dispatch", scope="full")) == every
    assert run("schedule").scope == "full"


def test_a_dispatch_defaults_to_the_edges() -> None:
    assert tags(run("workflow_dispatch")) == ["3.3a", "3.8"]
    assert tags(run("workflow_dispatch", scope="edges")) == ["3.3a", "3.8"]


@pytest.mark.parametrize(
    ("event", "want"),
    [
        ("push", ["3.3a", "3.8"]),
        ("pull_request", ["3.3a", "3.8"]),
    ],
)
def test_a_push_pull_request_and_schedule_ignore_every_dispatch_input(
    event: str, want: list[str]
) -> None:
    result = run(
        event,
        scope="full",
        tmux_ref="3.4",
        rust_toolchain="1.85.0",
        os="ubuntu-22.04",
        tmate="before-tests",
    )
    assert tags(result) == want
    assert result.os == "ubuntu-latest"
    assert {(leg["rust-toolchain"], leg["tmate"]) for leg in result.legs} == {("", "off")}


def test_an_event_it_has_no_plan_for_is_refused() -> None:
    assert "no plan for the release event" in refusal("release")


def test_one_supported_release_is_its_own_first_and_last() -> None:
    single = ci_plan.parse_manifest(lane("supported", "3.8"))
    result = ci_plan.plan(single, ci_plan.Inputs(event="push"))
    assert [leg["lane"] for leg in result.legs] == ["3.8"]


@pytest.mark.parametrize(
    ("event", "inputs"),
    [
        ("push", {}),
        ("pull_request", {}),
        ("schedule", {}),
        ("workflow_dispatch", {}),
        ("workflow_dispatch", {"scope": "full"}),
        ("workflow_dispatch", {"scope": "custom", "tmux_ref": "3.2a"}),
    ],
)
def test_every_event_plans_at_least_one_lane(event: str, inputs: dict[str, str]) -> None:
    assert len(run(event, **inputs).legs) > 0


@pytest.mark.parametrize("event", ["schedule", "workflow_dispatch"])
def test_the_fuzz_and_bench_jobs_run_on_a_schedule_and_a_dispatch(event: str) -> None:
    result = run(event)
    assert result.periodic
    assert result.skip == []


@pytest.mark.parametrize("event", ["push", "pull_request"])
def test_they_are_out_of_scope_on_a_push_and_a_pull_request(event: str) -> None:
    result = run(event)
    assert not result.periodic
    assert result.skip == ["fuzz", "bench"]


def test_it_runs_the_refs_it_names_once_each_in_the_order_given() -> None:
    result = run(
        "workflow_dispatch",
        scope="custom",
        tmux_ref=f" 3.7 , master,feature/x,{COMMIT},3.7,3.2a,3.9",
    )
    assert tags(result) == ["3.7", "master", "feature/x", COMMIT, "3.2a", "3.9"]
    names = {leg["lane"]: leg["name"] for leg in result.legs}
    assert names[COMMIT] == COMMIT[:12]
    assert names["feature/x"] == "feature/x"


def test_the_longest_ref_is_accepted() -> None:
    longest = "a" * 64
    result = run("workflow_dispatch", scope="custom", tmux_ref=longest)
    assert tags(result) == [longest]


def test_toolchains_cross_with_the_refs() -> None:
    result = run(
        "workflow_dispatch",
        scope="custom",
        tmux_ref="3.4,3.8",
        rust_toolchain="1.85.0, nightly",
        os="ubuntu-24.04",
    )
    assert result.os == "ubuntu-24.04"
    assert [(leg["lane"], leg["rust-toolchain"], leg["name"]) for leg in result.legs] == [
        ("3.4", "1.85.0", "3.4, Rust 1.85.0"),
        ("3.4", "nightly", "3.4, Rust nightly"),
        ("3.8", "1.85.0", "3.8, Rust 1.85.0"),
        ("3.8", "nightly", "3.8, Rust nightly"),
    ]


CUSTOM = {"scope": "custom"}


@pytest.mark.parametrize(
    ("inputs", "message"),
    [
        pytest.param({"scope": "everything"}, "scope must be", id="an unknown scope"),
        pytest.param(CUSTOM, "tmux-ref is empty", id="no refs"),
        pytest.param({**CUSTOM, "tmux_ref": "  "}, "tmux-ref is empty", id="blank refs"),
        pytest.param({**CUSTOM, "tmux_ref": "3.8,,3.7"}, "empty item", id="an empty item"),
        pytest.param({**CUSTOM, "tmux_ref": "3.8,"}, "empty item", id="a trailing comma"),
        pytest.param({**CUSTOM, "tmux_ref": "3.8;id"}, "invalid item", id="a shell metacharacter"),
        pytest.param({**CUSTOM, "tmux_ref": "$(id)"}, "invalid item", id="a command substitution"),
        pytest.param(
            {**CUSTOM, "tmux_ref": "3.8 3.7"}, "invalid item", id="a space inside an item"
        ),
        pytest.param({**CUSTOM, "tmux_ref": "a" * 65}, "invalid item", id="an item too long"),
        pytest.param({**CUSTOM, "tmux_ref": "-abc"}, "invalid item", id="a leading dash"),
        pytest.param({"os": "macos-latest"}, "os must be", id="an unknown runner"),
        pytest.param({"tmate": "always"}, "tmate must be", id="an unknown tmate mode"),
        pytest.param({"rust_toolchain": "1.85.0;id"}, "invalid item", id="a bad toolchain"),
        pytest.param(
            {"rust_toolchain": "1.85.0,,nightly"}, "empty item", id="an empty toolchain item"
        ),
        pytest.param({"rust_toolchain": "-1.85.0"}, "invalid item", id="a toolchain with a dash"),
    ],
)
def test_what_it_cannot_plan_is_refused(inputs: dict[str, str], message: str) -> None:
    assert message in refusal("workflow_dispatch", **inputs)


def test_it_goes_to_the_one_lane_that_runs() -> None:
    result = run("workflow_dispatch", scope="custom", tmux_ref="3.8", tmate="on-failure")
    assert [leg["tmate"] for leg in result.legs] == ["on-failure"]


@pytest.mark.parametrize(
    "inputs",
    [
        pytest.param(
            {"scope": "custom", "tmux_ref": "3.7,3.8", "tmate": "before-tests"}, id="two refs"
        ),
        pytest.param(
            {
                "scope": "custom",
                "tmux_ref": "3.8",
                "rust_toolchain": "1.85.0,nightly",
                "tmate": "on-failure",
            },
            id="two toolchains",
        ),
        pytest.param({"scope": "edges", "tmate": "on-failure"}, id="the edges"),
        pytest.param({"scope": "full", "tmate": "on-failure"}, id="every lane"),
    ],
)
def test_it_is_refused_for_more_than_one_lane(inputs: dict[str, str]) -> None:
    assert "tmate needs exactly one tmux lane" in refusal("workflow_dispatch", **inputs)


VALID = lane("supported", "3.3a")


@pytest.mark.parametrize(
    ("text", "message"),
    [
        pytest.param("# only a comment\n", "lists no lane", id="nothing"),
        pytest.param(
            lane("refused", "3.2a", "reject-core"), "no supported", id="no supported release"
        ),
        pytest.param(VALID + lane("preview", "3.9"), "unknown lane kind", id="an unknown kind"),
        pytest.param(VALID + "supported 3.4\n", "malformed", id="a short line"),
        pytest.param(
            VALID + lane("supported", "3.4").rstrip("\n") + " x\n", "malformed", id="an extra field"
        ),
        pytest.param(VALID + VALID, "listed twice", id="a repeat"),
        pytest.param(
            VALID + "supported 3.4 abc123 3.4 workspace -\n", "40-hex commit", id="a short commit"
        ),
        pytest.param(VALID + lane("supported", "3.4;id"), "invalid lane tag", id="an invalid tag"),
        pytest.param(VALID + lane("supported", "-3.4"), "invalid lane tag", id="a leading dash"),
        pytest.param(
            VALID + lane("supported", "3.4", requires="3.9"),
            "requires 3.9",
            id="a prerequisite it does not list",
        ),
    ],
)
def test_what_it_cannot_trust_is_refused(text: str, message: str) -> None:
    with pytest.raises(ci_plan.PlanError) as caught:
        ci_plan.parse_manifest(text)
    assert message in str(caught.value)


PASSED = {"result": "success"}


@pytest.mark.parametrize(
    ("needs", "allowed", "want"),
    [
        pytest.param({"plan": PASSED, "check": PASSED}, [], [], id="every job passed"),
        pytest.param(
            {"plan": PASSED, "fuzz": {"result": "skipped"}},
            ["fuzz", "bench"],
            [],
            id="a job the plan left out was skipped",
        ),
        pytest.param(
            {"plan": PASSED, "check": {"result": "failure"}},
            [],
            ["check: failure"],
            id="a job failed",
        ),
        pytest.param(
            {"plan": PASSED, "check": {"result": "cancelled"}},
            [],
            ["check: cancelled"],
            id="a job was cancelled",
        ),
        pytest.param(
            {"plan": PASSED, "compat": {"result": "skipped"}},
            ["fuzz"],
            ["compat: skipped"],
            id="a job the plan kept in scope was skipped",
        ),
        pytest.param(
            {"plan": {"result": "failure"}, "compat": {"result": "skipped"}},
            [],
            ["plan: failure", "compat: skipped"],
            id="the plan failed and skipped the rest",
        ),
        pytest.param({"plan": PASSED, "check": {}}, [], ["check: null"], id="a job with no result"),
    ],
)
def test_only_a_job_the_plan_left_out_may_be_skipped(
    needs: dict[str, dict[str, str]], allowed: list[str], want: list[str]
) -> None:
    assert ci_plan.unpassed(needs, allowed) == want


def test_a_run_with_no_jobs_is_an_error() -> None:
    with pytest.raises(ci_plan.PlanError):
        ci_plan.unpassed({}, [])


def invoke(
    directory: pathlib.Path, environment: dict[str, str], *argv: str
) -> tuple[int, str, str]:
    output = directory / "output"
    output.write_text("", encoding="utf-8")
    env = {"PATH": os.environ["PATH"], "GITHUB_OUTPUT": str(output)}
    env.update(environment)
    result = subprocess.run(
        [sys.executable, str(SCRIPTS / "ci-plan" / "ci_plan.py"), *argv],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    return result.returncode, output.read_text(encoding="utf-8"), result.stderr


def test_it_writes_the_plan_to_github_output(tmp_path: pathlib.Path) -> None:
    code, output, _ = invoke(tmp_path, {"GITHUB_EVENT_NAME": "pull_request"})
    assert code == 0
    fields = dict(line.split("=", 1) for line in output.splitlines())
    assert fields["scope"] == "edges"
    assert fields["periodic"] == "false"
    assert fields["skip"] == "fuzz bench"
    assert fields["jobs"] == "2"
    assert [leg["lane"] for leg in json.loads(fields["lanes"])] == ["3.3a", "3.8"]


def test_it_reads_its_inputs_from_the_environment_and_refuses_a_bad_one(
    tmp_path: pathlib.Path,
) -> None:
    code, output, stderr = invoke(
        tmp_path,
        {
            "GITHUB_EVENT_NAME": "workflow_dispatch",
            "INPUT_SCOPE": "custom",
            "INPUT_TMUX_REF": "3.8;id",
        },
    )
    assert code == 1
    assert "invalid item" in stderr
    assert output == ""


def test_verify_fails_on_a_failed_job(tmp_path: pathlib.Path) -> None:
    code, _, stderr = invoke(tmp_path, {"NEEDS_JSON": '{"check": {"result": "failure"}}'}, "verify")
    assert code == 1
    assert "check: failure" in stderr
    code, _, _ = invoke(tmp_path, {"NEEDS_JSON": '{"check": {"result": "success"}}'}, "verify")
    assert code == 0


RELEASE = re.compile(r"^(\d+)\.(\d+)([a-z]?)$")


def release_key(name: str) -> tuple[int, int, str]:
    match = RELEASE.fullmatch(name)
    assert match is not None, f"{name} is not a tmux release"
    return int(match.group(1)), int(match.group(2)), match.group(3)


REAL_LANES = ci_plan.parse_manifest(MANIFEST.read_text(encoding="utf-8"))


def test_supported_releases_run_oldest_first_and_the_edges_are_the_ends() -> None:
    supported = [lane.tag for lane in REAL_LANES if lane.kind == "supported"]
    assert supported == sorted(supported, key=release_key)
    result = ci_plan.plan(REAL_LANES, ci_plan.Inputs(event="pull_request"))
    assert [leg["lane"] for leg in result.legs] == [supported[0], supported[-1]]


def test_the_harness_loader_and_the_plan_read_the_same_lanes() -> None:
    from_harness = [
        (lane.kind, lane.tag, lane.commit, lane.version, lane.contract, lane.requires)
        for lane in compat_lanes.load_lanes(MANIFEST)
    ]
    from_plan = [
        (lane.kind, lane.tag, lane.commit, lane.version, lane.contract, lane.requires)
        for lane in REAL_LANES
    ]
    assert from_harness == from_plan
