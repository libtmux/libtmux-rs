"""Which tmux lanes the compatibility run builds and tests.

`.github/ci-matrix.txt` lists the lanes, each with the commit its tag must
name. `LIBTMUX_COMPAT_LANES` selects some of them, and may name a tmux version,
tag, branch or commit the manifest does not hold. `run_selected` calls the
caller's `build` and `test` for each lane; a lane's prerequisites are built, and
not tested, before it.
"""

from __future__ import annotations

import re
import subprocess
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

KINDS = ("supported", "witness", "refused", "development")
REF = re.compile(r"[A-Za-z0-9._/-]{1,64}")
COMMIT = re.compile(r"[0-9a-f]{40}")


class LaneError(Exception):
    """The manifest, a request or a ref cannot be trusted; the message says why."""


@dataclass(frozen=True)
class Lane:
    """One line of the manifest."""

    kind: str
    tag: str
    commit: str
    version: str
    contract: str
    requires: tuple[str, ...]


def valid_ref(ref: str) -> bool:
    """A tmux tag, branch or commit. A name that starts with a dash would read as a git option.

    >>> [valid_ref(ref) for ref in ("3.8", "feature/x", "-3.8", "3.8;id", "a" * 65)]
    [True, True, False, False, False]
    """
    return REF.fullmatch(ref) is not None and not ref.startswith("-")


def load_lanes(manifest: Path) -> list[Lane]:
    """Read the lanes in file order, refusing anything the run could not trust."""
    lanes: list[Lane] = []
    for raw in manifest.read_text(encoding="utf-8").splitlines():
        fields = raw.split("#", 1)[0].split()
        if not fields:
            continue
        if len(fields) != 6:
            raise LaneError(f"malformed lane in {manifest}: {' '.join(fields[:2])}")
        kind, tag, commit, version, contract, requires = fields
        if kind not in KINDS:
            raise LaneError(f"unknown lane kind in {manifest}: {kind}")
        if not valid_ref(tag):
            raise LaneError(f"invalid lane tag in {manifest}: {tag}")
        if COMMIT.fullmatch(commit) is None:
            raise LaneError(f"lane {tag} does not name a 40-hex commit")
        if any(lane.tag == tag for lane in lanes):
            raise LaneError(f"lane {tag} is listed twice")
        needs = () if requires == "-" else tuple(requires.split(","))
        lanes.append(Lane(kind, tag, commit, version, contract, needs))
    if not lanes:
        raise LaneError(f"{manifest} lists no lane")
    known = {lane.tag for lane in lanes}
    for lane in lanes:
        for needed in lane.requires:
            if needed not in known:
                raise LaneError(
                    f"lane {lane.tag} requires {needed}, which the manifest does not list"
                )
    return lanes


def select_lanes(lanes: list[Lane], request: str) -> list[str]:
    """The lanes to run: every one in the manifest, or the ones `request` names.

    Each is named once, in the order given.
    """
    names = request.replace(",", " ").split()
    if not names:
        return [lane.tag for lane in lanes]
    chosen: list[str] = []
    for name in names:
        if not valid_ref(name):
            raise LaneError(f"invalid lane in LIBTMUX_COMPAT_LANES: {name}")
        if name not in chosen:
            chosen.append(name)
    return chosen


def resolve_ref(ref: str, repository: str) -> str:
    """The commit a tag, branch or commit of tmux names.

    A tag wins over a branch, and an annotated tag is followed to the commit it
    tags.
    """
    if COMMIT.fullmatch(ref):
        return ref
    listing = subprocess.run(
        [
            "git",
            "ls-remote",
            "--",
            repository,
            f"refs/tags/{ref}",
            f"refs/tags/{ref}^{{}}",
            f"refs/heads/{ref}",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if listing.returncode != 0:
        raise LaneError(f"could not list {repository}")
    commits = {}
    for line in listing.stdout.splitlines():
        commit, _, name = line.partition("\t")
        commits[name] = commit
    for name in (f"refs/tags/{ref}^{{}}", f"refs/tags/{ref}", f"refs/heads/{ref}"):
        if name in commits:
            return commits[name]
    raise LaneError(f"{repository} has no tag or branch named {ref}")


def run_selected(
    lanes: list[Lane],
    request: str,
    build: Callable[[str], None],
    test: Callable[[str], None],
) -> None:
    """Build and test the selected lanes.

    A lane the manifest does not hold has no prerequisite.
    """
    selected = select_lanes(lanes, request)
    by_tag = {lane.tag: lane for lane in lanes}
    for tag in selected:
        if tag in by_tag:
            for required in by_tag[tag].requires:
                build(required)
        build(tag)
        test(tag)
