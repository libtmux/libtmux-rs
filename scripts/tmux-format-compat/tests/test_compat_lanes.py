"""Reading the manifest, choosing lanes, ordering builds and resolving refs.

The harness builds tmux and runs the workspace, which none of this does.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest
from compat_lanes import LaneError, load_lanes, resolve_ref, run_selected, select_lanes

ROOT = Path(__file__).resolve().parents[3]
MANIFEST = ROOT / ".github" / "ci-matrix.txt"
COMMIT = "0123456789abcdef0123456789abcdef01234567"
REAL_TAGS = [
    "3.3a",
    "3.4",
    "3.5a",
    "3.6b",
    "3.7b",
    "3.8",
    "3.7",
    "3.2a",
    "3.3",
    "3.0a",
    "3.1c",
    "master",
]


def manifest_lane(kind: str = "supported", tag: str = "3.3a", **overrides: str) -> str:
    fields = {"commit": COMMIT, "version": tag, "contract": "workspace", "requires": "-"}
    fields.update(overrides)
    return f"{kind} {tag} {fields['commit']} {fields['version']} {fields['contract']} {fields['requires']}\n"


def load(tmp_path: Path, text: str):
    path = tmp_path / "ci-matrix.txt"
    path.write_text(text, encoding="utf-8")
    return load_lanes(path)


def test_the_real_manifest_loads_in_file_order() -> None:
    assert [lane.tag for lane in load_lanes(MANIFEST)] == REAL_TAGS


def test_a_lane_carries_its_commit_version_contract_and_prerequisites() -> None:
    lane = next(lane for lane in load_lanes(MANIFEST) if lane.tag == "3.7")
    assert (lane.kind, lane.commit, lane.version, lane.contract, lane.requires) == (
        "witness",
        "81f88f8517c9fc5371b56cf117530c6b477c96ac",
        "3.7",
        "feature-boundary",
        ("3.3a", "3.7b"),
    )


def test_comments_blank_lines_and_trailing_comments_are_ignored(tmp_path: Path) -> None:
    text = "# a comment\n\n" + manifest_lane().rstrip("\n") + "  # trailing\n"
    assert [lane.tag for lane in load(tmp_path, text)] == ["3.3a"]


@pytest.mark.parametrize(
    ("text", "message"),
    [
        ("# only a comment\n", "lists no lane"),
        (manifest_lane("preview"), "unknown lane kind"),
        ("supported 3.3a\n", "malformed"),
        (manifest_lane().rstrip("\n") + " extra\n", "malformed"),
        (manifest_lane() + manifest_lane(), "listed twice"),
        (manifest_lane(commit="abc123"), "40-hex commit"),
        (manifest_lane(tag="3.3a;id"), "invalid lane tag"),
        (manifest_lane(tag="-3.3a"), "invalid lane tag"),
        (manifest_lane(requires="3.9"), "requires 3.9"),
    ],
)
def test_what_it_cannot_trust_is_refused(tmp_path: Path, text: str, message: str) -> None:
    with pytest.raises(LaneError, match=message):
        load(tmp_path, text)


@pytest.fixture(scope="module")
def lanes():
    return load_lanes(MANIFEST)


def test_no_request_selects_every_lane_in_manifest_order(lanes) -> None:
    assert select_lanes(lanes, "") == REAL_TAGS


@pytest.mark.parametrize("request_text", ["", "  ", ",", " , "])
def test_a_blank_request_selects_every_lane(lanes, request_text: str) -> None:
    assert len(select_lanes(lanes, request_text)) == len(REAL_TAGS)


def test_a_request_selects_what_it_names_once_in_the_order_given(lanes) -> None:
    assert select_lanes(lanes, "3.8, 3.3a 3.8,master") == ["3.8", "3.3a", "master"]


def test_a_request_may_name_a_ref_the_manifest_does_not_hold(lanes) -> None:
    assert select_lanes(lanes, f"3.9 feature/x {COMMIT}") == ["3.9", "feature/x", COMMIT]


@pytest.mark.parametrize("request_text", ["3.8;id", "$(id)", "-3.8", "a" * 65, "3.8|3.3a"])
def test_a_request_outside_the_ref_charset_is_refused(lanes, request_text: str) -> None:
    with pytest.raises(LaneError, match="invalid lane"):
        select_lanes(lanes, request_text)


def test_the_longest_ref_is_accepted(lanes) -> None:
    assert select_lanes(lanes, "a" * 64) == ["a" * 64]


def run_lanes(lanes, request: str) -> list[str]:
    log: list[str] = []
    built: set[str] = set()

    def build(tag: str) -> None:
        if tag not in built:
            built.add(tag)
            log.append(f"build {tag}")

    run_selected(lanes, request, build, lambda tag: log.append(f"test {tag}"))
    return log


def test_a_lane_is_built_then_tested(lanes) -> None:
    assert run_lanes(lanes, "3.8") == ["build 3.3a", "build 3.8", "test 3.8"]


def test_a_lane_that_needs_other_builds_gets_them_first_and_untested(lanes) -> None:
    assert run_lanes(lanes, "3.7") == ["build 3.3a", "build 3.7b", "build 3.7", "test 3.7"]
    assert run_lanes(lanes, "3.0a") == ["build 3.3a", "build 3.0a", "test 3.0a"]


def test_a_build_shared_by_two_lanes_happens_once(lanes) -> None:
    assert run_lanes(lanes, "3.3a 3.0a 3.1c") == [
        "build 3.3a",
        "test 3.3a",
        "build 3.0a",
        "test 3.0a",
        "build 3.1c",
        "test 3.1c",
    ]


def test_a_ref_outside_the_manifest_needs_no_prerequisite(lanes) -> None:
    assert run_lanes(lanes, "feature/x") == ["build feature/x", "test feature/x"]


def test_the_whole_manifest_builds_each_release_once_and_tests_every_lane(lanes) -> None:
    log = run_lanes(lanes, "")
    built = [line for line in log if line.startswith("build ")]
    tested = [line for line in log if line.startswith("test ")]
    assert len(built) == len(set(built)) == len(REAL_TAGS)
    assert len(tested) == len(REAL_TAGS)


def test_an_invalid_request_runs_nothing(lanes) -> None:
    log: list[str] = []
    with pytest.raises(LaneError):
        run_selected(lanes, "3.8;id", log.append, log.append)
    assert log == []


class Upstream:
    """A small git repository with an annotated tag, a branch and a commit on each."""

    def __init__(self, directory: Path) -> None:
        self.directory = directory / "upstream"
        self.directory.mkdir()
        home = directory / "home"
        home.mkdir()
        self.environment = {
            "PATH": os.environ["PATH"],
            "HOME": str(home),
            "GIT_CONFIG_NOSYSTEM": "1",
        }
        self.git("init", "--quiet")
        self.tagged = self.commit("tagged")
        self.git("tag", "--annotate", "--message", "release", "9.9")
        self.git("branch", "feature/x")
        self.master = self.commit("master")
        self.git("switch", "--quiet", "feature/x")
        self.branch = self.commit("branch")
        self.git("switch", "--quiet", "master")
        self.url = f"file://{self.directory}"

    def git(self, *arguments: str) -> str:
        result = subprocess.run(
            [
                "git",
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "tag.gpgsign=false",
                "-c",
                "init.defaultBranch=master",
                *arguments,
            ],
            cwd=self.directory,
            capture_output=True,
            text=True,
            check=True,
            env=self.environment,
        )
        return result.stdout.strip()

    def commit(self, name: str) -> str:
        (self.directory / "VERSION").write_text(name + "\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "--quiet", "--message", name)
        return self.git("rev-parse", "HEAD")


@pytest.fixture(scope="module")
def upstream(tmp_path_factory: pytest.TempPathFactory) -> Upstream:
    return Upstream(tmp_path_factory.mktemp("upstream"))


def test_an_annotated_tag_resolves_to_the_commit_it_tags(upstream: Upstream) -> None:
    assert resolve_ref("9.9", upstream.url) == upstream.tagged


def test_a_branch_resolves_to_its_tip(upstream: Upstream) -> None:
    assert resolve_ref("master", upstream.url) == upstream.master
    assert resolve_ref("feature/x", upstream.url) == upstream.branch


def test_a_commit_resolves_to_itself(upstream: Upstream) -> None:
    assert resolve_ref(COMMIT, upstream.url) == COMMIT


def test_a_ref_nothing_names_is_refused(upstream: Upstream) -> None:
    with pytest.raises(LaneError, match=r"has no tag or branch named 9\.8"):
        resolve_ref("9.8", upstream.url)
