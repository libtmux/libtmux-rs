#!/usr/bin/env python3
"""Tests for `compat-lanes.sh`, which says what the compatibility script builds.

The script itself builds tmux and runs the workspace, which no test here does.
What is tested is everything before that: reading the manifest, choosing the
lanes, ordering the builds a lane needs, and resolving a ref the manifest does
not hold.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import tempfile
import unittest

SCRIPTS = pathlib.Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
MANIFEST = ROOT / ".github" / "ci-matrix.txt"
LIBRARY = SCRIPTS / "compat-lanes.sh"

COMMIT = "0123456789abcdef0123456789abcdef01234567"

# Records what the orchestration would do instead of building tmux.
STUBS = """
built=" "
build_lane() {
    case "$built" in
        *" $1 "*) ;;
        *)
            built="$built$1 "
            echo "build $1"
            ;;
    esac
}
test_lane() { echo "test $1"; }
"""


def bash(
    script: str, environment: dict[str, str] | None = None
) -> subprocess.CompletedProcess[str]:
    env = {"PATH": os.environ["PATH"], "HOME": tempfile.gettempdir()}
    env.update(environment or {})
    return subprocess.run(
        ["bash", "-c", f"set -uo pipefail\nsource {LIBRARY}\n{script}"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )


def lines(result: subprocess.CompletedProcess[str]) -> list[str]:
    return result.stdout.split("\n")[:-1] if result.stdout else []


def manifest_lane(kind: str = "supported", tag: str = "3.3a", **overrides: str) -> str:
    fields = {
        "commit": COMMIT,
        "version": tag,
        "contract": "workspace",
        "requires": "-",
    }
    fields.update(overrides)
    return (
        f"{kind} {tag} {fields['commit']} {fields['version']} "
        f"{fields['contract']} {fields['requires']}\n"
    )


class ManifestTests(unittest.TestCase):
    def load(self, text: str) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "ci-matrix.txt"
            path.write_text(text, encoding="utf-8")
            return bash(f'load_lanes {path} && printf "%s\\n" "${{lane_tags[@]}}"')

    def test_the_real_manifest_loads_in_file_order(self) -> None:
        result = bash(f'load_lanes {MANIFEST} && printf "%s\\n" "${{lane_tags[@]}}"')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            lines(result),
            [
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
            ],
        )

    def test_a_lane_carries_its_commit_version_contract_and_prerequisites(self) -> None:
        result = bash(
            f"load_lanes {MANIFEST} && "
            'echo "${lane_kind[3.7]} ${lane_commit[3.7]} ${lane_version[3.7]} '
            '${lane_contract[3.7]} ${lane_requires[3.7]}"'
        )
        self.assertEqual(
            lines(result),
            [
                "witness 81f88f8517c9fc5371b56cf117530c6b477c96ac 3.7 feature-boundary 3.3a,3.7b"
            ],
        )

    def test_comments_blank_lines_and_trailing_comments_are_ignored(self) -> None:
        text = "# a comment\n\n" + manifest_lane().rstrip("\n") + "  # trailing\n"
        self.assertEqual(lines(self.load(text)), ["3.3a"])

    def test_what_it_cannot_trust_is_refused(self) -> None:
        for name, text, message in (
            ("nothing", "# only a comment\n", "lists no lane"),
            ("an unknown kind", manifest_lane("preview"), "unknown lane kind"),
            ("a short line", "supported 3.3a\n", "malformed"),
            ("an extra field", manifest_lane().rstrip("\n") + " extra\n", "malformed"),
            ("a repeat", manifest_lane() + manifest_lane(), "listed twice"),
            ("a short commit", manifest_lane(commit="abc123"), "40-hex commit"),
            ("an invalid tag", manifest_lane(tag="3.3a;id"), "invalid lane tag"),
            ("a leading dash", manifest_lane(tag="-3.3a"), "invalid lane tag"),
            (
                "a prerequisite it does not list",
                manifest_lane(requires="3.9"),
                "requires 3.9",
            ),
        ):
            with self.subTest(name):
                result = self.load(text)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn(message, result.stderr)


class SelectionTests(unittest.TestCase):
    def select(self, request: str | None) -> subprocess.CompletedProcess[str]:
        environment = {} if request is None else {"LIBTMUX_COMPAT_LANES": request}
        return bash(f"load_lanes {MANIFEST} && select_lanes", environment)

    def test_no_request_selects_every_lane_in_manifest_order(self) -> None:
        self.assertEqual(len(lines(self.select(None))), 12)
        self.assertEqual(lines(self.select(None))[0], "3.3a")
        self.assertEqual(lines(self.select(None))[-1], "master")

    def test_a_blank_request_selects_every_lane(self) -> None:
        for request in ("", "  ", ",", " , "):
            with self.subTest(request):
                self.assertEqual(len(lines(self.select(request))), 12)

    def test_a_request_selects_what_it_names_once_in_the_order_given(self) -> None:
        self.assertEqual(
            lines(self.select("3.8, 3.3a 3.8,master")), ["3.8", "3.3a", "master"]
        )

    def test_a_request_may_name_a_ref_the_manifest_does_not_hold(self) -> None:
        self.assertEqual(
            lines(self.select(f"3.9 feature/x {COMMIT}")),
            ["3.9", "feature/x", COMMIT],
        )

    def test_a_request_outside_the_ref_charset_is_refused(self) -> None:
        for request in ("3.8;id", "$(id)", "-3.8", "a" * 65, "3.8|3.3a"):
            with self.subTest(request):
                result = self.select(request)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertIn("invalid lane", result.stderr)
                self.assertEqual(result.stdout, "")

    def test_the_longest_ref_is_accepted(self) -> None:
        self.assertEqual(lines(self.select("a" * 64)), ["a" * 64])


class OrchestrationTests(unittest.TestCase):
    def run_lanes(self, request: str) -> list[str]:
        result = bash(
            f"load_lanes {MANIFEST} || exit 1\n{STUBS}\nrun_selected",
            {"LIBTMUX_COMPAT_LANES": request},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return lines(result)

    def test_a_lane_is_built_then_tested(self) -> None:
        self.assertEqual(self.run_lanes("3.8"), ["build 3.8", "test 3.8"])

    def test_a_lane_that_needs_other_builds_gets_them_first_and_untested(self) -> None:
        self.assertEqual(
            self.run_lanes("3.7"),
            ["build 3.3a", "build 3.7b", "build 3.7", "test 3.7"],
        )
        self.assertEqual(
            self.run_lanes("3.0a"), ["build 3.3a", "build 3.0a", "test 3.0a"]
        )

    def test_a_build_shared_by_two_lanes_happens_once(self) -> None:
        self.assertEqual(
            self.run_lanes("3.3a 3.0a 3.1c"),
            [
                "build 3.3a",
                "test 3.3a",
                "build 3.0a",
                "test 3.0a",
                "build 3.1c",
                "test 3.1c",
            ],
        )

    def test_a_ref_outside_the_manifest_needs_no_prerequisite(self) -> None:
        self.assertEqual(
            self.run_lanes("feature/x"), ["build feature/x", "test feature/x"]
        )

    def test_the_whole_manifest_builds_each_release_once_and_tests_every_lane(
        self,
    ) -> None:
        result = bash(f"load_lanes {MANIFEST} || exit 1\n{STUBS}\nrun_selected")
        self.assertEqual(result.returncode, 0, result.stderr)
        built = [line for line in lines(result) if line.startswith("build ")]
        tested = [line for line in lines(result) if line.startswith("test ")]
        self.assertEqual(len(built), len(set(built)))
        self.assertEqual(len(built), 12)
        self.assertEqual(len(tested), 12)

    def test_an_invalid_request_runs_nothing(self) -> None:
        result = bash(
            f"load_lanes {MANIFEST} || exit 1\n{STUBS}\nrun_selected",
            {"LIBTMUX_COMPAT_LANES": "3.8;id"},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")


class ResolveTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.directory = tempfile.TemporaryDirectory()
        upstream = pathlib.Path(cls.directory.name) / "upstream"
        upstream.mkdir()
        home = pathlib.Path(cls.directory.name) / "home"
        home.mkdir()

        def git(*args: str) -> str:
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
                    *args,
                ],
                cwd=upstream,
                capture_output=True,
                text=True,
                check=True,
                env={
                    "PATH": os.environ["PATH"],
                    "HOME": str(home),
                    "GIT_CONFIG_NOSYSTEM": "1",
                },
            )
            return result.stdout.strip()

        def commit(name: str) -> str:
            (upstream / "VERSION").write_text(name + "\n", encoding="utf-8")
            git("add", ".")
            git("commit", "--quiet", "--message", name)
            return git("rev-parse", "HEAD")

        git("init", "--quiet")
        cls.tagged = commit("tagged")
        git("tag", "--annotate", "--message", "release", "9.9")
        git("branch", "feature/x")
        cls.master = commit("master")
        git("switch", "--quiet", "feature/x")
        cls.branch = commit("branch")
        git("switch", "--quiet", "master")
        cls.url = f"file://{upstream}"

    @classmethod
    def tearDownClass(cls) -> None:
        cls.directory.cleanup()

    def resolve(self, ref: str) -> subprocess.CompletedProcess[str]:
        return bash(
            f"resolve_lane_ref {ref!r}",
            {"LIBTMUX_COMPAT_TMUX_REPOSITORY": self.url},
        )

    def test_an_annotated_tag_resolves_to_the_commit_it_tags(self) -> None:
        self.assertEqual(lines(self.resolve("9.9")), [self.tagged])

    def test_a_branch_resolves_to_its_tip(self) -> None:
        self.assertEqual(lines(self.resolve("master")), [self.master])
        self.assertEqual(lines(self.resolve("feature/x")), [self.branch])

    def test_a_commit_resolves_to_itself(self) -> None:
        self.assertEqual(lines(self.resolve(COMMIT)), [COMMIT])

    def test_a_ref_nothing_names_is_refused(self) -> None:
        result = self.resolve("9.8")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has no tag or branch named 9.8", result.stderr)


if __name__ == "__main__":
    unittest.main()
