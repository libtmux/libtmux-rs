"""The archive proof refuses mixed sources and unrelated stdio evidence."""

from __future__ import annotations

import io
import json
import os
import tarfile
from pathlib import Path

import installed_clients as clients
import pytest
import yaml


def test_native_dependency_identity_requires_the_selected_archive(tmp_path: Path) -> None:
    staged = {"libtmux-core": tmp_path / "libtmux-core-0.1.2"}
    row = f"libtmux-core v0.1.2 ({staged['libtmux-core']})"
    assert clients.check_dependency_tree([row, row + " (*)"], staged, expect_tokio=False) == [row]
    with pytest.raises(ValueError, match="staged source"):
        clients.check_dependency_tree(["libtmux-core v0.1.2"], staged, expect_tokio=False)
    with pytest.raises(ValueError, match="one staged core"):
        clients.check_dependency_tree([], staged, expect_tokio=False)
    with pytest.raises(ValueError, match="Tokio"):
        clients.check_dependency_tree([row, "tokio v1.52.0"], staged, expect_tokio=False)


def test_exact_sibling_pins_cover_target_specific_edges() -> None:
    manifest = {
        "package": {"name": "reader"},
        "dependencies": {"libtmux-core": {"version": "=0.1.2"}},
        "target": {
            "cfg(unix)": {
                "build-dependencies": {"core": {"package": "libtmux-core", "version": "0.1.2"}}
            }
        },
    }
    with pytest.raises(ValueError, match="pin libtmux-core exactly"):
        clients.check_sibling_pins(manifest, {"libtmux-core": "0.1.2"})
    manifest["target"].clear()
    clients.check_sibling_pins(manifest, {"libtmux-core": "0.1.2"})


def test_recorder_proof_requires_raw_stdout_and_completed_cleanup(tmp_path: Path) -> None:
    (tmp_path / "stdout.bin").write_bytes(b"mixed runtime\n")
    record = {
        "passed": True,
        "status": 0,
        "timedOut": False,
        "stdoutHex": b"mixed runtime\n".hex(),
        "cleanup": {"required": True, "passed": True},
        "native": {"beforeHex": b"mixed runtime\n".hex(), "afterHex": b"mixed runtime\n".hex()},
    }
    assert (
        clients.check_record(record, tmp_path, b"mixed runtime\n", require_capture=True) == record
    )
    for field, value in [
        ("passed", False),
        ("status", 1),
        ("timedOut", True),
        ("stdoutHex", "00"),
        ("cleanup", {"required": True, "passed": False}),
        ("native", None),
    ]:
        broken = {**record, field: value}
        with pytest.raises(ValueError):
            clients.check_record(broken, tmp_path, b"mixed runtime\n", require_capture=True)


def test_mcp_capture_preserves_the_documented_terminal_newline() -> None:
    capture = {"pane": "%7", "text": "installed app pane\n\n\n", "lines": 4}
    assert clients.capture_bytes(capture, "%7") == b"installed app pane\n\n\n\n"
    with pytest.raises(ValueError, match="pane"):
        clients.capture_bytes(capture, "%8")
    with pytest.raises(ValueError, match="line count"):
        clients.capture_bytes({**capture, "lines": 3}, "%7")


def test_cli_summary_requires_created_session_and_pane_identities() -> None:
    summary = {
        "schema_version": 1,
        "command": "load",
        "status": "ok",
        "errors": [],
        "results": [
            {
                "session_name": "installed-workspace",
                "session_id": "$2",
                "owned_session": True,
                "window_ids": ["@3"],
                "pane_ids": ["%4"],
            }
        ],
    }
    clients.check_workspace(summary)
    with pytest.raises(ValueError):
        clients.check_workspace({**summary, "status": "failed"})
    with pytest.raises(ValueError):
        clients.check_workspace({**summary, "results": []})


def test_staging_includes_both_apps_and_binds_every_archive_to_one_revision(tmp_path: Path) -> None:
    revision = "a" * 40

    class ArchiveCargo(clients.Cargo):
        def run(self, cwd: Path, *arguments: str) -> bytes:
            assert [
                arguments[index + 1] for index, arg in enumerate(arguments) if arg == "-p"
            ] == list(clients.PACKAGES)
            archive_dir = self.target / "package"
            archive_dir.mkdir(parents=True)
            for package in clients.PACKAGES:
                with tarfile.open(archive_dir / f"{package}-0.1.2.crate", "w:gz") as archive:
                    for filename, content in {
                        "Cargo.toml": f'[package]\nname = "{package}"\nversion = "0.1.2"\n',
                        ".cargo_vcs_info.json": json.dumps({"git": {"sha1": revision}}),
                    }.items():
                        info = tarfile.TarInfo(f"{package}-0.1.2/{filename}")
                        info.size = len(content.encode())
                        archive.addfile(info, io.BytesIO(content.encode()))
            return b""

    output = tmp_path / "proof"
    output.mkdir()
    cargo = ArchiveCargo(tmp_path)
    staged, versions, archives = clients.stage_archives(cargo, tmp_path, output, revision)
    assert set(staged) == set(clients.PACKAGES)
    assert set(versions.values()) == {"0.1.2"}
    assert {item["package"] for item in archives} >= {"tmux-mcp", "tmux-workspace"}
    for archive in archives:
        assert archive["sha256"] == clients.sha256(
            output / "archives" / f"{archive['package']}-0.1.2.crate"
        )


def test_stdio_reader_keeps_bytes_after_the_last_expected_response() -> None:
    reader, writer = os.pipe()
    with os.fdopen(reader, "rb") as source, os.fdopen(writer, "wb") as sink:
        sink.write(b'{"jsonrpc":"2.0","id":1,"result":{}}\nlate-human-output\n')
        sink.flush()
        sink.close()
        records = clients.JsonLines(source)
        assert json.loads(records.line())["id"] == 1
        assert records.finish() == b"late-human-output\n"
        assert bytes(records.raw).endswith(b"late-human-output\n")


def test_stdio_reader_rejects_an_unterminated_record_at_eof() -> None:
    reader, writer = os.pipe()
    os.write(writer, b"partial")
    os.close(writer)
    with os.fdopen(reader, "rb") as source:
        records = clients.JsonLines(source)
        with pytest.raises(ValueError, match="incomplete"):
            records.finish()


def test_stdio_reader_enforces_its_byte_limit(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(clients, "STREAM_LIMIT", 8)
    reader, writer = os.pipe()
    os.write(writer, b"too-many-bytes\n")
    os.close(writer)
    with os.fdopen(reader, "rb") as source, pytest.raises(ValueError, match="byte limit"):
        clients.JsonLines(source).line()


def test_ci_uses_the_same_pinned_recorder_and_keeps_the_proof_artifact() -> None:
    workflow = yaml.safe_load((clients.ROOT / ".github/workflows/ci.yml").read_text())
    steps = workflow["jobs"]["native-clients"]["steps"]
    [checkout] = [
        step for step in steps if step.get("with", {}).get("repository") == "libtmux/docs"
    ]
    assert checkout["with"]["ref"] == clients.RECORDER_REVISION
    [command] = [step["run"] for step in steps if "installed_clients.py" in step.get("run", "")]
    assert "--recorder .native-proof-docs/scripts/example_tmux_sandbox.py" in command
    assert "--output-dir artifacts/native-clients" in command
    [upload] = [
        step for step in steps if step.get("uses", "").startswith("actions/upload-artifact@")
    ]
    assert upload["if"] == "always()"
    assert upload["with"]["path"] == "artifacts/native-clients"
    assert "native-clients" in workflow["jobs"]["required"]["needs"]


def test_install_preserves_external_lockfile_checksums_and_versions() -> None:
    original = {"name": "external", "version": "1.0.0", "source": "registry", "checksum": "abc"}
    shipped = {
        "package": [original, {"name": "libtmux-core", "version": "0.1.2", "source": "registry"}]
    }
    staged = {"libtmux-core": Path("/staged/core")}
    rebound = {"package": [original, {"name": "libtmux-core", "version": "0.1.2"}]}
    clients.verify_external_pins(shipped, rebound, staged)
    for field, update in [("version", "1.0.1"), ("checksum", "def")]:
        with pytest.raises(ValueError, match=r"shipped Cargo\.lock"):
            clients.verify_external_pins(
                shipped, {"package": [{**original, field: update}]}, staged
            )
