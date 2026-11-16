"""Tests that hold the CI workflow and the documents about it to the manifest.

The workflow takes its tmux lanes from the plan, which reads
`.github/ci-matrix.txt`, so nothing here compares a list in one file with a
list in another. What is checked is that no second list has grown back, that
every job is behind the one required check, that no step calls `apt-get`
without the shared apt-get action, that the shared actions are pinned to a
full commit, and that the documents state the range the
manifest tests.
"""

from __future__ import annotations

import pathlib
import re

import ci_plan
import pytest
import yaml

ROOT = pathlib.Path(__file__).resolve().parents[3]
WORKFLOWS = ROOT / ".github" / "workflows"
ACTIONS = ROOT / ".github" / "actions"

CI = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
JOB = re.compile(r"^  ([a-z][a-z-]*):\s*$", re.MULTILINE)
RELEASE = re.compile(r"\b\d+\.\d+[a-z]?\b")
TMATE = "mxschmitt/action-tmate@"
# `apt` or `apt-get` in command position, past `sudo` and `VAR=value` prefixes.
# An `apt-get` action in a `uses:` line is not a `run` line, so it never matches.
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
            end = headings[index + 1].start() if index + 1 < len(headings) else len(jobs)
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
                    "use the shared libtmux/.github apt-get action"
                )
    return found


def test_the_compatibility_matrix_comes_from_the_plan() -> None:
    compat = job_section(CI, "compat")
    assert "include: ${{ fromJSON(needs.plan.outputs.lanes) }}" in compat
    assert "runs-on: ${{ needs.plan.outputs.os }}" in compat
    assert re.search(r"^\s+(?:lane|tmux-version|tag)s?:\s*\[", CI, re.MULTILINE) is None, (
        "ci.yml lists tmux versions inline; they belong in .github/ci-matrix.txt"
    )


def test_the_compatibility_job_runs_the_lane_the_plan_gave_it() -> None:
    compat = job_section(CI, "compat")
    assert "LIBTMUX_COMPAT_LANES: ${{ matrix.lane }}" in compat
    assert "just compat" in compat


@pytest.mark.parametrize("name", ci_plan.PERIODIC_JOBS)
def test_the_periodic_jobs_run_when_the_plan_says_so(name: str) -> None:
    section = job_section(CI, name)
    assert "needs: plan" in section
    assert "if: needs.plan.outputs.periodic == 'true'" in section


def test_inputs_reach_the_plan_only_through_the_environment() -> None:
    for line in CI.splitlines():
        if "${{ inputs." in line:
            assert line.strip().startswith("INPUT_"), (
                f"an input reaches the workflow outside an INPUT_ entry: {line.strip()}"
            )


def test_the_weekly_schedule_stays_the_only_one() -> None:
    assert len(re.findall(r"^\s+- cron:", CI, re.MULTILINE)) == 1


def test_a_dispatch_takes_the_documented_inputs() -> None:
    _, _, after = CI.partition("  workflow_dispatch:\n    inputs:\n")
    block = after.split("\n  # Scheduled weekly", 1)[0]
    names = re.findall(r"^      ([a-z-]+):\s*$", block, re.MULTILINE)
    assert names == ["scope", "tmux-ref", "rust-toolchain", "os", "tmate"]


def test_a_release_does_not_wait_on_the_narrowed_lanes() -> None:
    release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    assert "ci.yml" not in release
    assert "ci-matrix" not in release


def test_a_dispatch_adds_its_run_id_to_the_group() -> None:
    match = re.search(r"^concurrency:\n  group: (.+)$", CI, re.MULTILINE)
    assert match is not None
    assert "workflow_dispatch" in match.group(1)
    assert "github.run_id" in match.group(1)


def test_the_required_job_waits_for_every_other_job() -> None:
    section = job_section(CI, "required")
    needs = re.search(r"^    needs: \[([^\]]+)\]", section, re.MULTILINE)
    assert needs is not None
    waits = sorted(entry.strip() for entry in needs.group(1).split(","))
    others = sorted(name for name in job_ids(CI) if name != "required")
    assert waits == others
    assert "if: ${{ always() }}" in section
    assert "ALLOW_SKIPPED: ${{ needs.plan.outputs.skip }}" in section


def test_off_is_quoted_in_the_options_and_the_default() -> None:
    # GitHub reads a bare `off` as false. A default that is not one of the
    # options refuses every dispatch that sets nothing.
    assert "options: ['off', before-tests, on-failure]" in CI
    assert re.search(r"(?m)^        default: ['\"]off['\"]$", CI)


def test_tmate_is_pinned_limited_and_confined_to_the_compatibility_job() -> None:
    uses = CI.count(TMATE)
    assert uses > 0
    assert len(re.findall(TMATE + r"[0-9a-f]{40}\b", CI)) == uses
    assert CI.count("limit-access-to-actor: true") == uses
    compat = job_section(CI, "compat")
    assert compat.count(TMATE) == uses
    assert "timeout-minutes:" in compat


@pytest.mark.parametrize(
    "path",
    [path for path in sorted(WORKFLOWS.glob("*.yml")) if path.name != "ci.yml"],
    ids=lambda path: path.name,
)
def test_no_other_workflow_opens_a_session(path: pathlib.Path) -> None:
    assert "action-tmate" not in path.read_text(encoding="utf-8")


def test_no_step_calls_apt_without_the_wrapper() -> None:
    paths = sorted(WORKFLOWS.glob("*.yml")) + sorted(ACTIONS.glob("*/action.yml"))
    assert WORKFLOWS / "ci.yml" in paths
    found = []
    for path in paths:
        doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        found += raw_apt_calls(doc, path.relative_to(ROOT).as_posix())
    assert found == []


def test_a_raw_apt_get_is_named_with_its_fix() -> None:
    doc = yaml.safe_load(
        """
jobs:
  build:
    steps:
      - name: Install tmux
        run: |
          # sudo apt-get update is only mentioned here
          sudo apt-get update
          echo done
"""
    )
    assert raw_apt_calls(doc, "ci.yml") == [
        (
            "ci.yml: job build, step Install tmux runs `sudo apt-get update` "
            "directly; use the shared libtmux/.github apt-get action"
        )
    ]


def test_the_shared_action_passes() -> None:
    doc = yaml.safe_load(
        """
jobs:
  build:
    steps:
      - uses: libtmux/.github/actions/apt-get@30f81842b1e639ea4e25f93f1f2543504178e228
        with:
          update: "true"
          args: install --yes tmux
"""
    )
    assert raw_apt_calls(doc, "ci.yml") == []


def test_the_shared_actions_are_pinned_to_a_full_commit() -> None:
    pattern = re.compile(r"uses:\s*(libtmux/\.github/\S+)@(\S+)")
    found = [
        (path.name, action, ref)
        for path in sorted(WORKFLOWS.glob("*.yml"))
        for action, ref in pattern.findall(path.read_text(encoding="utf-8"))
    ]
    assert found, "no workflow uses a shared action"
    unpinned = [
        f"{name}: {action}@{ref}"
        for name, action, ref in found
        if not re.fullmatch(r"[0-9a-f]{40}", ref)
    ]
    assert unpinned == []


@pytest.mark.parametrize(
    "call",
    [
        "apt update",
        "sudo -E apt-get install -y tmux",
        "sudo DEBIAN_FRONTEND=noninteractive apt-get install -y tmux",
        "test -x /usr/bin/tmux || sudo apt-get install -y tmux",
    ],
)
def test_every_spelling_of_a_raw_call_is_caught(call: str) -> None:
    action = {"runs": {"steps": [{"run": call}]}}
    assert raw_apt_calls(action, "action.yml") == [
        (
            f"action.yml: step #1 runs `{call}` directly; "
            "use the shared libtmux/.github apt-get action"
        )
    ]


def test_a_python_file_is_executable_exactly_when_it_has_a_shebang() -> None:
    # ruff's EXE001 and EXE002 check this, but not under WSL, so a contributor
    # there would only learn of a wrong mode from CI.
    files = [
        *sorted((ROOT / "scripts").rglob("*.py")),
        *sorted((ROOT / "crates/tmux-workspace/tests/fixtures").glob("*.py")),
    ]
    assert len(files) > 20
    wrong = []
    for path in files:
        has_shebang = path.read_text(encoding="utf-8").startswith("#!")
        is_executable = bool(path.stat().st_mode & 0o111)
        if has_shebang != is_executable:
            wrong.append(path.relative_to(ROOT).as_posix())
    assert wrong == []


LANES = ci_plan.parse_manifest((ROOT / ".github" / "ci-matrix.txt").read_text(encoding="utf-8"))


def tags(*kinds: str) -> set[str]:
    return {lane.tag for lane in LANES if lane.kind in kinds}


def test_the_readme_states_the_floor_the_manifest_tests() -> None:
    readme = (ROOT / "README.md").read_text(encoding="utf-8")
    start = readme.index("- **tmux ")
    bullet = readme[start : readme.index("\n- **", start + 1)]
    named = RELEASE.findall(bullet)
    supported = [lane.tag for lane in LANES if lane.kind == "supported"]
    assert named[:1] == supported[:1], "the floor is the oldest lane"
    assert set(named) <= set(supported)


def test_contributing_states_every_release_the_manifest_builds() -> None:
    text = (ROOT / ".github" / "CONTRIBUTING.md").read_text(encoding="utf-8")
    start = text.index("\n## Compatibility\n")
    end = text.find("\n## ", start + 1)
    section = text[start:] if end == -1 else text[start:end]
    bold = " ".join(re.findall(r"\*\*([^*]+)\*\*", section))
    assert set(RELEASE.findall(bold)) == tags("supported", "witness", "refused")
