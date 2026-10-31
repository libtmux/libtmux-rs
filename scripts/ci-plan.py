#!/usr/bin/env python3
"""Plan the tmux compatibility lanes of the CI workflow.

    ci-plan.py          read .github/ci-matrix.txt, the event and the dispatch
                        inputs, and write the plan to $GITHUB_OUTPUT
    ci-plan.py verify   read NEEDS_JSON and ALLOW_SKIPPED, and fail unless every
                        job the plan put in scope passed

A push or pull request runs the first and last supported release. The whole
manifest runs for the weekly schedule and for a dispatch with scope=full. A
dispatch with scope=custom runs the tmux versions, tags, branches or commits it
names. The fuzz, bench and parity jobs run on a schedule and on any dispatch.

Inputs reach this script only through the environment: INPUT_SCOPE,
INPUT_TMUX_REF, INPUT_RUST_TOOLCHAIN, INPUT_OS and INPUT_TMATE. Any other event
ignores them. CI_MATRIX names another manifest, for the tests.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import sys
from collections.abc import Mapping
from dataclasses import dataclass

REF = re.compile(r"^[A-Za-z0-9._/-]{1,64}$")
TOOLCHAIN = re.compile(r"^[A-Za-z0-9._-]{1,32}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
KINDS = ("supported", "witness", "refused", "development")
RUNNERS = ("ubuntu-latest", "ubuntu-24.04", "ubuntu-22.04")
TMATE_MODES = ("off", "before-tests", "on-failure")
SCOPES = ("edges", "full", "custom")
PERIODIC_JOBS = ("fuzz", "bench", "parity")
DEFAULT_MANIFEST = (
    pathlib.Path(__file__).resolve().parent.parent / ".github" / "ci-matrix.txt"
)


class PlanError(Exception):
    """The inputs or the manifest cannot be planned."""


def valid_ref(value: str) -> bool:
    """A tmux version, tag, branch or commit; a leading dash would read as an option."""
    return REF.fullmatch(value) is not None and not value.startswith("-")


@dataclass(frozen=True)
class Lane:
    """One line of the manifest."""

    kind: str
    tag: str
    commit: str
    version: str
    contract: str
    requires: tuple[str, ...]


def parse_manifest(text: str) -> list[Lane]:
    """Read the lanes in file order, refusing anything the plan could not trust."""
    lanes: list[Lane] = []
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        fields = line.split()
        if len(fields) != 6:
            raise PlanError(f"malformed lane: {line}")
        kind, tag, commit, version, contract, requires = fields
        if kind not in KINDS:
            raise PlanError(f"unknown lane kind: {kind}")
        if not valid_ref(tag):
            raise PlanError(f"invalid lane tag: {tag}")
        if COMMIT.fullmatch(commit) is None:
            raise PlanError(f"lane {tag} does not name a 40-hex commit")
        if any(lane.tag == tag for lane in lanes):
            raise PlanError(f"lane {tag} is listed twice")
        needs = () if requires == "-" else tuple(requires.split(","))
        lanes.append(Lane(kind, tag, commit, version, contract, needs))
    if not lanes:
        raise PlanError("the manifest lists no lane")
    if not any(lane.kind == "supported" for lane in lanes):
        raise PlanError("the manifest lists no supported release")
    known = {lane.tag for lane in lanes}
    for lane in lanes:
        for needed in lane.requires:
            if needed not in known:
                raise PlanError(
                    f"lane {lane.tag} requires {needed}, "
                    "which the manifest does not list"
                )
    return lanes


def split_list(text: str, label: str) -> list[str]:
    """Split a comma-separated input; an empty item is an error."""
    if not text.strip():
        raise PlanError(f"{label} is empty")
    items: list[str] = []
    for raw in text.split(","):
        item = raw.strip()
        if not item:
            raise PlanError(f"{label} has an empty item: {text}")
        if item not in items:
            items.append(item)
    return items


def one_of(allowed: tuple[str, ...], value: str, label: str) -> str:
    if value not in allowed:
        raise PlanError(f"{label} must be {', '.join(allowed)}, got: {value}")
    return value


@dataclass(frozen=True)
class Inputs:
    """The run's event and its dispatch inputs, as the workflow passes them."""

    event: str
    scope: str = ""
    tmux_ref: str = ""
    rust_toolchain: str = ""
    os: str = ""
    tmate: str = ""


@dataclass(frozen=True)
class Plan:
    """What one run does."""

    scope: str
    #: The compatibility legs, one tmux lane and one toolchain each.
    legs: list[dict[str, str]]
    os: str
    #: Whether the fuzz, bench and parity jobs run.
    periodic: bool
    #: Jobs the plan leaves out of scope, which may be skipped.
    skip: list[str]


def plan(lanes: list[Lane], inputs: Inputs) -> Plan:
    """Plan one run."""
    dispatch = inputs.event == "workflow_dispatch"
    if inputs.event in ("push", "pull_request"):
        scope = "edges"
    elif inputs.event == "schedule":
        scope = "full"
    elif dispatch:
        scope = one_of(SCOPES, inputs.scope or "edges", "scope")
    else:
        raise PlanError(f"no plan for the {inputs.event} event")

    runner = "ubuntu-latest"
    tmate = "off"
    toolchains = [""]
    if dispatch:
        if inputs.os:
            runner = one_of(RUNNERS, inputs.os, "os")
        if inputs.tmate:
            tmate = one_of(TMATE_MODES, inputs.tmate, "tmate")
        if inputs.rust_toolchain.strip():
            toolchains = split_list(inputs.rust_toolchain, "rust-toolchain")
            for toolchain in toolchains:
                if TOOLCHAIN.fullmatch(toolchain) is None or toolchain.startswith("-"):
                    raise PlanError(f"rust-toolchain has an invalid item: {toolchain}")

    supported = [lane.tag for lane in lanes if lane.kind == "supported"]
    if scope == "edges":
        refs = (
            [supported[0]]
            if supported[0] == supported[-1]
            else [supported[0], supported[-1]]
        )
    elif scope == "full":
        refs = [lane.tag for lane in lanes]
    else:
        refs = split_list(inputs.tmux_ref, "tmux-ref")
        for ref in refs:
            if not valid_ref(ref):
                raise PlanError(
                    f"tmux-ref has an invalid item (want {REF.pattern}, "
                    f"no leading dash): {ref}"
                )

    legs: list[dict[str, str]] = []
    for ref in refs:
        label = ref[:12] if COMMIT.fullmatch(ref) else ref
        for toolchain in toolchains:
            legs.append(
                {
                    "lane": ref,
                    "name": f"{label}, Rust {toolchain}" if toolchain else label,
                    "rust-toolchain": toolchain,
                    "tmate": tmate,
                }
            )

    if tmate != "off" and len(legs) != 1:
        raise PlanError(
            f"tmate needs exactly one tmux lane and the plan has {len(legs)}; "
            "name one tmux-ref and at most one rust-toolchain"
        )

    periodic = inputs.event in ("schedule", "workflow_dispatch")
    return Plan(scope, legs, runner, periodic, [] if periodic else list(PERIODIC_JOBS))


def unpassed(needs: Mapping[str, object], allow_skipped: list[str]) -> list[str]:
    """The jobs that did not pass; only a job the plan left out may be skipped."""
    if not needs:
        raise PlanError("NEEDS_JSON holds no jobs")
    failures: list[str] = []
    for name, need in needs.items():
        result = need.get("result") if isinstance(need, dict) else None
        if result == "success":
            continue
        if result == "skipped" and name in allow_skipped:
            continue
        failures.append(f"{name}: {result if isinstance(result, str) else 'null'}")
    return failures


def write_outputs(result: Plan, environ: Mapping[str, str]) -> None:
    lines = [
        f"scope={result.scope}",
        f"lanes={json.dumps(result.legs, separators=(',', ':'))}",
        f"os={result.os}",
        f"periodic={'true' if result.periodic else 'false'}",
        f"skip={' '.join(result.skip)}",
        f"jobs={len(result.legs)}",
    ]
    destination = environ.get("GITHUB_OUTPUT")
    if destination:
        with open(destination, "a", encoding="utf-8") as handle:
            handle.write("\n".join(lines) + "\n")
    else:
        sys.stdout.write("\n".join(lines) + "\n")

    summary = environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(
                "### tmux plan\n\n"
                f"- scope: `{result.scope}`\n"
                f"- tmux lanes: `{' '.join(leg['lane'] for leg in result.legs)}`\n"
                f"- planned compatibility jobs: {len(result.legs)}\n"
            )


def verify(environ: Mapping[str, str]) -> None:
    raw = environ.get("NEEDS_JSON", "")
    if not raw:
        raise PlanError("verify needs NEEDS_JSON")
    try:
        needs = json.loads(raw)
    except json.JSONDecodeError as error:
        raise PlanError("NEEDS_JSON is not JSON") from error
    if not isinstance(needs, dict):
        raise PlanError("NEEDS_JSON is not an object of jobs")
    failures = unpassed(needs, environ.get("ALLOW_SKIPPED", "").split())
    if failures:
        raise PlanError("these jobs did not pass:\n" + "\n".join(failures))
    sys.stdout.write("required: every planned job passed\n")


def main(argv: list[str], environ: Mapping[str, str]) -> int:
    try:
        if argv == ["verify"]:
            verify(environ)
            return 0
        if argv:
            raise PlanError(f"unknown argument: {' '.join(argv)}")
        event = environ.get("GITHUB_EVENT_NAME", "")
        if not event:
            raise PlanError("GITHUB_EVENT_NAME is not set")
        manifest = pathlib.Path(environ.get("CI_MATRIX") or DEFAULT_MANIFEST)
        lanes = parse_manifest(manifest.read_text(encoding="utf-8"))
        result = plan(
            lanes,
            Inputs(
                event=event,
                scope=environ.get("INPUT_SCOPE", ""),
                tmux_ref=environ.get("INPUT_TMUX_REF", ""),
                rust_toolchain=environ.get("INPUT_RUST_TOOLCHAIN", ""),
                os=environ.get("INPUT_OS", ""),
                tmate=environ.get("INPUT_TMATE", ""),
            ),
        )
        write_outputs(result, environ)
    except PlanError as error:
        sys.stderr.write(f"::error::ci-plan: {error}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], os.environ))
