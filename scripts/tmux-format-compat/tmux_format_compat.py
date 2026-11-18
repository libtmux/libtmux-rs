#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Build each pinned tmux release from source and run the workspace against it.

    tmux_format_compat.py

tmux's own output and error wording change between releases, so each lane
builds its release from the commit `.github/ci-matrix.txt` pins and runs the
whole workspace, or proves a refusal. `LIBTMUX_COMPAT_LANES` names the lanes,
such as "3.3a 3.8", and may name a tmux tag, branch or commit the manifest does
not hold. Unset, every lane in the manifest runs.

The run is one worker lineage under `tmux_format_compat_supervisor.py`, which
contains every process the builds and tests start and removes the build
directory afterwards. The supervisor needs Linux, a `C.UTF-8` locale and a
Python with pidfd support.

Supported lanes run the whole workspace against their build. The older lanes
prove refusal without effects. The crate's version sensitivity is not confined
to the codec: flags, control mode, and the stderr wording that separates a
missing target from a refusal all differ by release in principle, and asserting
them on one version proves nothing about the others. The manifest says which
lanes run and why each is there.
"""

from __future__ import annotations

import os
import platform
import signal
import subprocess
import sys
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path

from compat_lanes import Lane, LaneError, load_lanes, resolve_ref, run_selected

WORKER_MODE = "__libtmux_tfc_worker"
BUILD_ROOT_PREFIX = "/tmp/libtmux-tmux-format-compat."
ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / ".github" / "ci-matrix.txt"
REQUIRE_TESTS = ROOT / "scripts" / "require-tests" / "require_tests.py"
FORMAT_COVERAGE = ROOT / "scripts" / "format-coverage.py"
DEFAULT_REPOSITORY = "https://github.com/tmux/tmux.git"
LOCALE = {"LC_ALL": "C.UTF-8"}
REQUIRED_FAMILIES = (
    "real_tmux_compat_format_",
    "real_tmux_compat_aggregate_",
    "real_tmux_compat_projection_",
    "real_tmux_compat_error_",
)
GENERIC_CONTRACTS = ("workspace", "format-catalog", "development", "custom")
REFUSAL_CONTRACTS = ("reject-core", "reject-boundary", "reject-pre-probe", "feature-boundary")


class CompatFailure(Exception):
    """A step failed; `status` is what the run exits with."""

    def __init__(self, message: str, status: int = 1) -> None:
        super().__init__(message)
        self.status = status


@dataclass(frozen=True)
class Step:
    """One command of a lane's test, and the environment it adds."""

    argv: tuple[str, ...]
    env: Mapping[str, str] = field(default_factory=dict)
    require: bool = True
    """Whether to fail the step when it ran no test (a name filter that matched nothing)."""


def cargo_test(package: str, *arguments: str, features: bool = True) -> tuple[str, ...]:
    """`cargo test --locked --package PACKAGE --all-features ARGUMENTS`."""
    command = ["cargo", "test", "--locked", "--package", package]
    if features:
        command.append("--all-features")
    return (*command, *arguments)


def refusal_steps(
    contract: str, tmux_binary: Path, current_binary: Path, newer_binary: Path
) -> list[Step]:
    """The steps of a lane that proves a refusal rather than running the workspace.

    `LIBTMUX_REQUIRE_NATIVE` names the variables a lane provides: a case gated on
    one fails instead of skipping.
    """
    binary = str(tmux_binary)
    if contract == "reject-core":
        env = {
            "LIBTMUX_TEST_UNSUPPORTED_TMUX": binary,
            "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_UNSUPPORTED_TMUX",
        }
        return [
            Step(
                cargo_test("libtmux", "--test", "daemon_version_check", "--", "--test-threads=1"),
                env,
            ),
            Step(
                cargo_test(
                    "tmux-workspace",
                    "--test",
                    "cli",
                    "old_daemon_refuses_load_before_scripts_or_mutation",
                    "--",
                    "--exact",
                    "--test-threads=1",
                ),
                env,
            ),
            Step(
                cargo_test(
                    "tmux-workspace",
                    "--test",
                    "cli",
                    "the_attach_handoff",
                    "--",
                    "--test-threads=1",
                ),
                env,
            ),
            Step(
                cargo_test(
                    "tmux-mcp",
                    "--lib",
                    "native_frame_generation_refuses_an_older_replacement",
                    "--",
                    "--test-threads=1",
                ),
                env,
            ),
        ]
    if contract == "reject-pre-probe":
        return [
            Step(
                cargo_test(
                    "libtmux",
                    "--test",
                    "daemon_version_check",
                    "real_tmux_compat_pre_probe_",
                    "--",
                    "--test-threads=1",
                ),
                {
                    "LIBTMUX_TEST_PRE_PROBE_TMUX": binary,
                    "LIBTMUX_TEST_TMUX": str(current_binary),
                    "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_PRE_PROBE_TMUX",
                },
            )
        ]
    if contract == "reject-boundary":
        return [
            Step(
                cargo_test("libtmux", "--test", "daemon_version_check", "--", "--test-threads=1"),
                {
                    "LIBTMUX_TEST_BOUNDARY_TMUX": binary,
                    "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_BOUNDARY_TMUX",
                },
            )
        ]
    if contract == "feature-boundary":
        return [
            Step(
                cargo_test("libtmux", "--test", "optional_features", "--", "--test-threads=1"),
                {
                    "LIBTMUX_REQUIRE_NATIVE": (
                        "LIBTMUX_TEST_BROKEN_BREAK_TMUX,"
                        "LIBTMUX_TEST_MINIMUM_TMUX,LIBTMUX_TEST_NEWER_TMUX"
                    ),
                    "LIBTMUX_TEST_BROKEN_BREAK_TMUX": binary,
                    "LIBTMUX_TEST_MINIMUM_TMUX": str(current_binary),
                    "LIBTMUX_TEST_NEWER_TMUX": str(newer_binary),
                },
            )
        ]
    raise ValueError(f"{contract} is not a refusal contract")


def feature_environment(tag: str, tmux_binary: Path) -> dict[str, str]:
    """The variables a lane provides to the whole workspace run."""
    if tag == "3.3a":
        return {
            "LIBTMUX_TEST_MINIMUM_TMUX": str(tmux_binary),
            "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_MINIMUM_TMUX",
        }
    if tag == "3.4":
        return {
            "LIBTMUX_TEST_SAFE_BUFFER_TMUX": str(tmux_binary),
            "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_SAFE_BUFFER_TMUX",
        }
    return {}


def require_family(test_list: str, family: str) -> None:
    """Fail when the list of tests has no test of the family.

    >>> require_family("real_tmux_compat_format_a: test", "real_tmux_compat_format_")
    >>> require_family("nothing", "real_tmux_compat_error_")
    Traceback (most recent call last):
        ...
    tmux_format_compat.CompatFailure: missing required Rust test family: real_tmux_compat_error_
    """
    if family not in test_list:
        raise CompatFailure(f"missing required Rust test family: {family}")


def run(
    argv: Sequence[str],
    *,
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
    capture: bool = False,
) -> str:
    """Run a command to completion, with this process's output, and return what it printed."""
    result = subprocess.run(
        list(argv),
        cwd=cwd,
        env={**os.environ, **(env or {})},
        stdout=subprocess.PIPE if capture else None,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise CompatFailure("", result.returncode)
    return result.stdout or ""


def git(directory: Path, *arguments: str) -> str:
    return run(["git", "-C", str(directory), *arguments], capture=True).strip()


def normalize_bootstrap_mtimes(source: Path, epoch: int) -> None:
    """Give every file of the tree the commit's time.

    Automake rejects a new conftest when the wall clock steps backward. Keep
    this disposable tree at its authenticated commit time instead.
    """
    names: set[str] = set()
    for flags in (
        ("--cached", "--others", "--exclude-standard"),
        ("--others", "--ignored", "--exclude-standard"),
    ):
        listing = run(["git", "-C", str(source), "ls-files", "-z", *flags], capture=True)
        names.update(name for name in listing.split("\0") if name)
    for name in names:
        os.utime(source / name, (epoch, epoch), follow_symlinks=False)


class Harness:
    """Builds and tests the lanes of one run in one build directory."""

    def __init__(self, lanes: list[Lane], build_root: Path, repository: str) -> None:
        self.lanes = {lane.tag: lane for lane in lanes}
        self.build_root = build_root
        self.repository = repository
        self.built: set[str] = set()

    def install_prefix(self, tag: str) -> Path:
        return self.build_root / f"tmux-{tag.replace('/', '_')}-install"

    def binary(self, tag: str) -> Path:
        return self.install_prefix(tag) / "bin" / "tmux"

    def build_lane(self, tag: str) -> None:
        """The lane's tmux, built from its pinned commit.

        A lane the manifest does not hold is a tag, branch or commit named by
        `LIBTMUX_COMPAT_LANES`: it resolves to a commit and is built as a
        development build, with no version to compare.
        """
        if tag in self.built:
            return
        lane = self.lanes.get(tag)
        contract = lane.contract if lane else "custom"
        expected_version = f"tmux {lane.version}" if lane else ""
        expected_commit = lane.commit if lane else resolve_ref(tag, self.repository)
        source = self.build_root / f"tmux-{tag.replace('/', '_')}-source"
        prefix = self.install_prefix(tag)

        if contract in ("development", "custom"):
            # A development build has no release tag: pin the exact commit.
            run(["git", "init", str(source)])
            git(source, "fetch", "--depth", "1", self.repository, expected_commit)
            git(source, "checkout", "--detach", "FETCH_HEAD")
        else:
            run(
                [
                    "git",
                    "clone",
                    "--branch",
                    tag,
                    "--depth",
                    "1",
                    "--single-branch",
                    self.repository,
                    str(source),
                ]
            )
            if git(source, "cat-file", "-t", f"refs/tags/{tag}") != "tag":
                raise CompatFailure(f"tmux {tag} is not an annotated tag")
            if git(source, "rev-parse", f"refs/tags/{tag}^{{}}") != expected_commit:
                raise CompatFailure(f"tmux {tag} peeled commit does not match")

        checkout = git(source, "rev-parse", "HEAD")
        if checkout != expected_commit:
            raise CompatFailure(f"tmux {tag} checkout commit does not match")
        epoch = int(git(source, "show", "-s", "--format=%ct", checkout))

        if contract == "format-catalog":
            run([sys.executable, str(FORMAT_COVERAGE), "--check-source", str(source)], cwd=ROOT)

        run(["sh", "autogen.sh"], cwd=source)
        normalize_bootstrap_mtimes(source, epoch)
        run(["./configure", f"--prefix={prefix}"], cwd=source)
        run(["make", f"-j{len(os.sched_getaffinity(0))}"], cwd=source)
        run(["make", "install"], cwd=source)

        binary = self.binary(tag)
        if not (binary.is_file() and os.access(binary, os.X_OK)):
            raise CompatFailure(f"tmux {tag} did not install an executable")
        installed = run([str(binary), "-V"], capture=True).strip()
        if expected_version and installed != expected_version:
            raise CompatFailure(f"tmux {tag} version output does not match")
        print(installed, flush=True)
        self.built.add(tag)

    def run_step(self, step: Step) -> None:
        argv = list(step.argv)
        if step.require:
            argv = [sys.executable, str(REQUIRE_TESTS), *argv]
        run(argv, cwd=ROOT, env=step.env)

    def test_lane(self, tag: str) -> None:
        """Run a lane's tests: the workspace, or the refusal it exists to prove."""
        lane = self.lanes.get(tag)
        contract = lane.contract if lane else "custom"
        binary = self.binary(tag)
        # The lane's prerequisites are built before it, never tested.
        current = self.binary("3.3a")
        newer = self.binary("3.7b")

        if contract == "reject-pre-probe" and not (
            current.is_file() and os.access(current, os.X_OK)
        ):
            raise CompatFailure(f"tmux {tag} runs behind the 3.3a client, which is not built")
        if contract in REFUSAL_CONTRACTS:
            for step in refusal_steps(contract, binary, current, newer):
                self.run_step(step)
            return

        if tag == "3.3a":
            self.run_step(
                Step(
                    cargo_test(
                        "libtmux",
                        "--test",
                        "daemon_version_check",
                        "real_tmux_compat_minimum_daemon_accepts_newer_clients",
                        "--",
                        "--test-threads=1",
                    ),
                    {
                        "LIBTMUX_TEST_MINIMUM_TMUX": str(binary),
                        "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_MINIMUM_TMUX",
                    },
                )
            )

        if tag == "3.8":
            environment = {
                "LIBTMUX_TEST_TMUX": str(binary),
                "LIBTMUX_TEST_MINIMUM_TMUX": str(current),
                "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_MINIMUM_TMUX",
                "PATH": f"{self.install_prefix(tag) / 'bin'}{os.pathsep}{os.environ['PATH']}",
            }
            self.run_step(
                Step(
                    cargo_test("libtmux", "--test", "capabilities", "--", "--test-threads=1"),
                    environment,
                )
            )
            self.run_step(
                Step(
                    cargo_test(
                        "tmux-mcp",
                        "--test",
                        "protocol",
                        "daemon_capability_tool_",
                        "--",
                        "--test-threads=1",
                    ),
                    environment,
                )
            )

        self.run_workspace(tag, lane, binary)

    def run_workspace(self, tag: str, lane: Lane | None, binary: Path) -> None:
        inherited = os.environ.get("PATH", "")
        if not inherited:
            raise CompatFailure("inherited PATH must be nonempty")
        lane_path = f"{self.install_prefix(tag) / 'bin'}{os.pathsep}{inherited}"
        selected = run(["tmux", "-V"], env={**LOCALE, "PATH": lane_path}, capture=True).strip()
        if lane and selected != f"tmux {lane.version}":
            raise CompatFailure(f"command-local PATH did not select tmux {tag}")

        run(
            ["cargo", "test", "--locked", "--workspace", "--all-targets", "--all-features"],
            cwd=ROOT,
            env={
                **LOCALE,
                "LIBTMUX_TEST_TMUX": str(binary),
                "PATH": lane_path,
                **feature_environment(tag, binary),
            },
        )

        compat = ["cargo", "test", "--locked", "--package", "libtmux", "--lib", "--all-features"]
        environment = {**LOCALE, "LIBTMUX_TEST_TMUX": str(binary)}
        listing = run(
            [*compat, "real_tmux_compat_", "--", "--test-threads=1", "--list"],
            cwd=ROOT,
            env=environment,
            capture=True,
        )
        for family in REQUIRED_FAMILIES:
            require_family(listing, family)
        run([*compat, "real_tmux_compat_", "--", "--test-threads=1"], cwd=ROOT, env=environment)


def preflight() -> None:
    """The supervisor needs Linux, a UTF-8 locale and pidfd support."""
    if platform.system() != "Linux":
        raise CompatFailure("tmux format compatibility requires Linux")
    charmap = subprocess.run(
        ["locale", "charmap"],
        env={**os.environ, **LOCALE},
        capture_output=True,
        text=True,
        check=False,
    )
    if charmap.returncode != 0:
        raise CompatFailure("tmux format compatibility requires C.UTF-8 locale")
    if charmap.stdout.strip() != "UTF-8":
        raise CompatFailure("tmux format compatibility requires C.UTF-8")
    try:
        if not hasattr(signal, "pidfd_send_signal"):
            raise OSError
        os.close(os.pidfd_open(os.getpid()))
    except OSError:
        raise CompatFailure("tmux format compatibility requires Python pidfd support") from None


def valid_build_root(build_root: str) -> bool:
    """A directory the supervisor made: under /tmp, named for this run, not a link.

    >>> valid_build_root("/etc")
    False
    """
    return (
        build_root.startswith(BUILD_ROOT_PREFIX)
        and not os.path.islink(build_root)
        and os.path.isdir(build_root)
    )


def worker(arguments: Sequence[str]) -> int:
    """The worker the supervisor starts: it stops itself, then builds and tests."""
    if len(arguments) != 1:
        raise CompatFailure("invalid compatibility worker invocation")
    build_root = arguments[0]
    if not valid_build_root(build_root):
        raise CompatFailure("invalid compatibility worker build root")
    # The supervisor registers this process while it is stopped, then resumes it.
    os.kill(os.getpid(), signal.SIGSTOP)

    os.chdir(ROOT)
    lanes = load_lanes(MANIFEST)
    repository = os.environ.get("LIBTMUX_COMPAT_TMUX_REPOSITORY") or DEFAULT_REPOSITORY
    harness = Harness(lanes, Path(build_root), repository)
    run_selected(
        lanes,
        os.environ.get("LIBTMUX_COMPAT_LANES", ""),
        harness.build_lane,
        harness.test_lane,
    )
    return 0


def main(arguments: Sequence[str]) -> int:
    try:
        if arguments[:1] == [WORKER_MODE]:
            return worker(arguments[1:])
        if arguments:
            raise CompatFailure("tmux format compatibility accepts no arguments")
        preflight()
    except (CompatFailure, LaneError) as failure:
        if str(failure):
            print(failure, file=sys.stderr)
        return getattr(failure, "status", 1)
    # The supervisor needs Linux, so it is imported once the platform is known.
    # It takes the place of this process and runs the worker.
    import tmux_format_compat_supervisor as supervisor

    return supervisor.main(["--", sys.executable, str(Path(__file__).resolve()), WORKER_MODE])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
