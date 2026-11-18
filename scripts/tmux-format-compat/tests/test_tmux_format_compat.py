"""The compatibility harness: what each lane runs, and one whole run against a stand-in tmux.

The whole-run test replaces `cargo` and the tmux source with small stand-ins, so
it checks the harness and the supervisor together without building tmux or the
workspace.
"""

from __future__ import annotations

import os
import platform
import signal
import stat
import subprocess
import sys
import tempfile
from pathlib import Path

import pytest
import tmux_format_compat as harness
from compat_lanes import load_lanes

ENTRY = Path(harness.__file__).resolve()
LINUX = sys.platform == "linux"
BINARY = Path("/b/tmux-3.8-install/bin/tmux")
CURRENT = Path("/b/tmux-3.3a-install/bin/tmux")
NEWER = Path("/b/tmux-3.7b-install/bin/tmux")


def test_every_contract_in_the_manifest_is_one_the_harness_runs() -> None:
    allowed = {
        "supported": {"workspace", "format-catalog"},
        "witness": {"feature-boundary"},
        "refused": {"reject-core", "reject-boundary", "reject-pre-probe"},
        "development": {"development"},
    }
    handled = {*harness.GENERIC_CONTRACTS, *harness.REFUSAL_CONTRACTS}
    for lane in load_lanes(harness.MANIFEST):
        assert lane.contract in allowed[lane.kind], lane
        assert lane.contract in handled, lane


def test_a_refusal_lane_names_the_tmux_it_proves_the_crate_refuses() -> None:
    steps = harness.refusal_steps("reject-core", BINARY, CURRENT, NEWER)
    assert len(steps) == 4
    for step in steps:
        assert step.require
        assert step.env["LIBTMUX_TEST_UNSUPPORTED_TMUX"] == str(BINARY)
        assert step.env["LIBTMUX_REQUIRE_NATIVE"] == "LIBTMUX_TEST_UNSUPPORTED_TMUX"
    assert [step.argv[4] for step in steps] == [
        "libtmux",
        "tmux-workspace",
        "tmux-workspace",
        "tmux-mcp",
    ]
    assert "--exact" in steps[1].argv


def test_the_pre_probe_lane_runs_behind_the_minimum_client() -> None:
    (step,) = harness.refusal_steps("reject-pre-probe", BINARY, CURRENT, NEWER)
    assert step.env["LIBTMUX_TEST_PRE_PROBE_TMUX"] == str(BINARY)
    assert step.env["LIBTMUX_TEST_TMUX"] == str(CURRENT)
    assert "real_tmux_compat_pre_probe_" in step.argv


def test_the_boundary_lane_requires_its_variable() -> None:
    (step,) = harness.refusal_steps("reject-boundary", BINARY, CURRENT, NEWER)
    assert step.env == {
        "LIBTMUX_TEST_BOUNDARY_TMUX": str(BINARY),
        "LIBTMUX_REQUIRE_NATIVE": "LIBTMUX_TEST_BOUNDARY_TMUX",
    }


def test_the_witness_lane_pins_the_broken_release_between_the_two_it_needs() -> None:
    (step,) = harness.refusal_steps("feature-boundary", BINARY, CURRENT, NEWER)
    assert step.env["LIBTMUX_TEST_BROKEN_BREAK_TMUX"] == str(BINARY)
    assert step.env["LIBTMUX_TEST_MINIMUM_TMUX"] == str(CURRENT)
    assert step.env["LIBTMUX_TEST_NEWER_TMUX"] == str(NEWER)
    assert step.env["LIBTMUX_REQUIRE_NATIVE"].count(",") == 2


def test_a_workspace_contract_has_no_refusal_steps() -> None:
    with pytest.raises(ValueError, match="not a refusal contract"):
        harness.refusal_steps("workspace", BINARY, CURRENT, NEWER)


def test_only_the_minimum_and_the_safe_buffer_releases_provide_a_variable() -> None:
    assert harness.feature_environment("3.3a", BINARY)["LIBTMUX_TEST_MINIMUM_TMUX"] == str(BINARY)
    assert harness.feature_environment("3.4", BINARY)["LIBTMUX_TEST_SAFE_BUFFER_TMUX"] == str(
        BINARY
    )
    assert harness.feature_environment("3.8", BINARY) == {}


def test_current_lane_builds_the_floor_for_connected_discovery() -> None:
    current = next(lane for lane in load_lanes(harness.MANIFEST) if lane.tag == "3.8")
    assert "3.3a" in current.requires


def test_current_lane_qualifies_core_and_mcp_discovery(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    run = harness.Harness(load_lanes(harness.MANIFEST), tmp_path, "unused")
    steps: list[harness.Step] = []
    monkeypatch.setattr(run, "run_step", steps.append)
    monkeypatch.setattr(run, "run_workspace", lambda *_: None)
    run.test_lane("3.8")
    assert len(steps) == 2
    assert "capabilities" in steps[0].argv
    assert "daemon_capability_tool_" in steps[1].argv
    for step in steps:
        assert step.env["LIBTMUX_TEST_TMUX"] == str(run.binary("3.8"))
        assert step.env["LIBTMUX_TEST_MINIMUM_TMUX"] == str(run.binary("3.3a"))
        assert step.env["PATH"].startswith(str(run.install_prefix("3.8") / "bin"))
        assert step.env["LIBTMUX_REQUIRE_NATIVE"] == "LIBTMUX_TEST_MINIMUM_TMUX"


@pytest.mark.parametrize("family", harness.REQUIRED_FAMILIES)
def test_a_missing_test_family_fails_the_lane(family: str) -> None:
    listing = "\n".join(f"{other}x: test" for other in harness.REQUIRED_FAMILIES if other != family)
    with pytest.raises(harness.CompatFailure, match=family):
        harness.require_family(listing, family)


def test_a_build_root_must_be_a_directory_the_supervisor_made(tmp_path: Path) -> None:
    assert not harness.valid_build_root(str(tmp_path))
    link = tmp_path / "link"
    link.symlink_to(tmp_path)
    assert not harness.valid_build_root(str(link))


def test_a_worker_refuses_a_root_it_did_not_get_from_the_supervisor() -> None:
    with pytest.raises(harness.CompatFailure, match="invalid compatibility worker build root"):
        harness.worker(["/etc"])
    with pytest.raises(harness.CompatFailure, match="invalid compatibility worker invocation"):
        harness.worker([])


def test_arguments_are_refused(capsys: pytest.CaptureFixture[str]) -> None:
    assert harness.main(["--all"]) == 1
    assert "accepts no arguments" in capsys.readouterr().err


def test_another_platform_is_refused(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(platform, "system", lambda: "Darwin")
    assert harness.main([]) == 1
    assert "requires Linux" in capsys.readouterr().err


@pytest.mark.skipif(not LINUX, reason="pidfd")
def test_a_python_without_pidfd_is_refused(monkeypatch: pytest.MonkeyPatch) -> None:
    def refuse(_pid: int) -> int:
        raise OSError

    monkeypatch.setattr(os, "pidfd_open", refuse)
    if (
        platform.system() == "Linux"
        and subprocess.run(
            ["locale", "charmap"],
            env={**os.environ, "LC_ALL": "C.UTF-8"},
            capture_output=True,
            check=False,
        ).stdout.strip()
        != b"UTF-8"
    ):
        pytest.skip("no C.UTF-8 locale")
    with pytest.raises(harness.CompatFailure, match="pidfd"):
        harness.preflight()


def test_mtimes_are_set_on_tracked_and_ignored_files(tmp_path: Path) -> None:
    subprocess.run(["git", "init", "--quiet", str(tmp_path)], check=True, capture_output=True)
    (tmp_path / ".gitignore").write_text("built\n")
    (tmp_path / "tracked").write_text("x")
    (tmp_path / "built").write_text("y")
    (tmp_path / "link").symlink_to("tracked")
    harness.normalize_bootstrap_mtimes(tmp_path, 1_000_000_000)
    for name in ("tracked", "built", ".gitignore"):
        assert (tmp_path / name).stat().st_mtime == 1_000_000_000, name
    assert (tmp_path / "link").lstat().st_mtime == 1_000_000_000


@pytest.mark.skipif(not LINUX, reason="the worker is stopped by the supervisor on Linux")
def test_a_worker_stops_itself_before_it_does_any_work() -> None:
    with tempfile.TemporaryDirectory(prefix="libtmux-tmux-format-compat.") as root:
        child = subprocess.Popen([sys.executable, str(ENTRY), harness.WORKER_MODE, root])
        try:
            _, status = os.waitpid(child.pid, os.WUNTRACED)
            assert os.WIFSTOPPED(status)
            assert os.WSTOPSIG(status) == signal.SIGSTOP
        finally:
            child.kill()
            child.wait()


FAKE_CARGO = """#!/usr/bin/env python3
import os
import sys

with open(@LOG@, "a", encoding="utf-8") as log:
    log.write(" ".join(["cargo", *sys.argv[1:]]) + "\\n")
    for name in ("LIBTMUX_TEST_TMUX", "LIBTMUX_REQUIRE_NATIVE", "LIBTMUX_TEST_MINIMUM_TMUX", "LC_ALL"):
        log.write(f"  {name}={os.environ.get(name, 'unset')}\\n")
if "--list" in sys.argv:
    for family in ("format", "aggregate", "projection", "error"):
        print(f"real_tmux_compat_{family}_x: test")
print("running 1 test")
"""

# The tmux source the stand-in run builds: no autogen step to run, a configure
# that writes a Makefile, and an installed tmux that reports a development version.
STAND_IN_SOURCE = {
    "autogen.sh": "#!/usr/bin/env python3\n",
    "configure": """#!/usr/bin/env python3
import sys
from pathlib import Path

prefix = next(a.removeprefix("--prefix=") for a in sys.argv[1:] if a.startswith("--prefix="))
Path("Makefile").write_text(
    f"all:\\n\\t@true\\ninstall:\\n\\tmkdir -p {prefix}/bin\\n\\tcp tmux {prefix}/bin/tmux\\n"
)
""",
    "tmux": "#!/usr/bin/env python3\nprint('tmux next-9.9')\n",
}


def locale_available() -> bool:
    result = subprocess.run(
        ["locale", "charmap"],
        env={**os.environ, "LC_ALL": "C.UTF-8"},
        capture_output=True,
        text=True,
        check=False,
    )
    return result.returncode == 0 and result.stdout.strip() == "UTF-8"


@pytest.mark.skipif(not (LINUX and locale_available()), reason="needs Linux and C.UTF-8")
def test_a_whole_run_builds_a_pinned_commit_and_tests_it_under_the_supervisor(
    tmp_path: Path,
) -> None:
    upstream = tmp_path / "tmux"
    upstream.mkdir()
    for name, source in STAND_IN_SOURCE.items():
        (upstream / name).write_text(source)
        (upstream / name).chmod(0o755)
    git = ["git", "-c", "user.name=f", "-c", "user.email=f@example.invalid", "-C", str(upstream)]
    subprocess.run([*git, "init", "--quiet"], check=True)
    subprocess.run([*git, "add", "."], check=True)
    subprocess.run([*git, "commit", "--quiet", "--message", "stand-in"], check=True)
    commit = subprocess.run(
        [*git, "rev-parse", "HEAD"], capture_output=True, text=True, check=True
    ).stdout.strip()

    log = tmp_path / "cargo.log"
    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    cargo = fake_bin / "cargo"
    cargo.write_text(FAKE_CARGO.replace("@LOG@", repr(str(log))))
    cargo.chmod(cargo.stat().st_mode | stat.S_IXUSR)

    result = subprocess.run(
        [sys.executable, str(ENTRY)],
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
        env={
            **os.environ,
            "PATH": f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
            "LIBTMUX_COMPAT_LANES": commit,
            "LIBTMUX_COMPAT_TMUX_REPOSITORY": f"file://{upstream}",
        },
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert "tmux next-9.9" in result.stdout
    calls = [line for line in log.read_text().splitlines() if line.startswith("cargo")]
    assert calls == [
        "cargo test --locked --workspace --all-targets --all-features",
        "cargo test --locked --package libtmux --lib --all-features real_tmux_compat_ -- --test-threads=1 --list",
        "cargo test --locked --package libtmux --lib --all-features real_tmux_compat_ -- --test-threads=1",
    ]
