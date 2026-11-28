"""The archive proof refuses mixed sources and unrelated stdio evidence."""

from __future__ import annotations

import io
import json
import os
import tarfile
import tomllib
from pathlib import Path

import installed_clients as clients
import pytest
import yaml


def _mcp_discovery() -> tuple[list[dict], dict]:
    direct = {
        "toolset": "inspect",
        "processReach": "none",
        "tmuxEffects": ["observe"],
        "outputClasses": ["tmux-metadata"],
    }
    hints = {
        "readOnlyHint": False,
        "destructiveHint": True,
        "idempotentHint": False,
        "openWorldHint": True,
    }
    tools = [
        {
            "name": "list_sessions",
            "annotations": {**hints, "title": "List sessions"},
            "_meta": {"com.git-pull.libtmux-mcp/capability": direct},
        }
    ]
    report = {"tools": [{"name": "list_sessions", "annotations": hints.copy(), **direct}]}
    return tools, report


@pytest.mark.parametrize("provenance", ["unknown", "minimal"])
def test_installed_discovery_keeps_direct_axes_and_whole_call_hints(provenance: str) -> None:
    tools, report = _mcp_discovery()
    report["connection"] = {"configurationProvenance": provenance}
    clients.check_mcp_discovery(tools, report)


@pytest.mark.parametrize("surface", ["tool", "resource"])
@pytest.mark.parametrize(
    ("hint", "wrong"),
    [
        ("readOnlyHint", True),
        ("destructiveHint", False),
        ("idempotentHint", True),
        ("openWorldHint", False),
    ],
)
def test_installed_discovery_refuses_narrow_whole_call_hints(
    surface: str, hint: str, wrong: bool
) -> None:
    tools, report = _mcp_discovery()
    row = tools[0] if surface == "tool" else report["tools"][0]
    row["annotations"][hint] = wrong
    with pytest.raises(ValueError, match="whole-call annotations"):
        clients.check_mcp_discovery(tools, report)


@pytest.mark.parametrize(
    ("axis", "wrong"),
    [
        ("toolset", "execute"),
        ("processReach", "pane-command"),
        ("tmuxEffects", ["delete"]),
        ("outputClasses", ["terminal-content"]),
    ],
)
def test_installed_discovery_refuses_disagreement_about_direct_capabilities(
    axis: str, wrong: str | list[str]
) -> None:
    tools, report = _mcp_discovery()
    report["tools"][0][axis] = wrong
    with pytest.raises(ValueError, match="direct capability"):
        clients.check_mcp_discovery(tools, report)


def test_blocking_reader_uses_the_archive_readme_bytes(tmp_path: Path) -> None:
    program = 'use libtmux::blocking::Server;\n\nfn main() { println!("README reader"); }\n'
    hidden = (
        '# #[cfg(feature = "blocking")]\n'
        + program
        + '# #[cfg(not(feature = "blocking"))]\n# fn main() {}\n'
    )
    readme = (
        "An unrelated example.\n\n```rust\nlet offline = 1;\n```\n\n"
        "<!-- blocking-example -->\n\n```rust,no_run\n"
        + hidden
        + "```\n\n<!-- /blocking-example -->\n"
    )
    (tmp_path / "README.md").write_text(readme)
    assert clients.readme_program(tmp_path, "blocking") == program
    (tmp_path / "README.md").write_text(readme.replace("README reader", "changed reader"))
    assert clients.readme_program(tmp_path, "blocking") == program.replace(
        "README reader", "changed reader"
    )


@pytest.mark.parametrize("damage", ["missing", "duplicate", "unclosed"])
def test_blocking_reader_refuses_an_ambiguous_readme_program(tmp_path: Path, damage: str) -> None:
    readme = (
        "<!-- blocking-example -->\n\n```rust,no_run\nfn main() {}\n```\n\n"
        "<!-- /blocking-example -->\n"
    )
    if damage == "missing":
        readme = readme.replace("<!-- blocking-example -->", "")
    elif damage == "duplicate":
        readme += readme
    else:
        readme = readme.replace("\n```\n", "\n")
    (tmp_path / "README.md").write_text(readme)
    with pytest.raises(ValueError, match="blocking README program"):
        clients.readme_program(tmp_path, "blocking")


def test_archived_guides_preserve_reader_setup_and_source(tmp_path: Path) -> None:
    crate = tmp_path / "archive"
    examples = crate / "examples"
    (examples / "api").mkdir(parents=True)
    source = b'fn main() { println!("compiled guide"); }\n'
    (examples / "api_read.rs").write_bytes(source)
    (examples / "api/consumer.toml").write_text(
        '[dependencies]\nlibtmux-tokio = { path = "checkout", features = ["query"] }\n'
    )
    manifest = examples / "api/manifest.json"
    entry = {
        "id": "rust-read",
        "cargoExample": "api_read",
        "sourceFile": "crates/libtmux-tokio/examples/api_read.rs",
    }
    staged = {"libtmux-tokio": crate}
    manifest.write_text(json.dumps({"examples": [entry]}))
    project = tmp_path / "reader"
    programs = clients.prepare_guide_readers(project, staged)
    package = tomllib.loads((project / "Cargo.toml").read_text())
    assert package["dependencies"]["libtmux-tokio"] == {"path": str(crate), "features": ["query"]}
    assert programs["api_read"][0].read_bytes() == source

    manifest.write_text(json.dumps({"examples": [{**entry, "sourceFile": "crates/other.rs"}]}))
    with pytest.raises(ValueError, match="source"):
        clients.prepare_guide_readers(tmp_path / "wrong-source", staged)
    manifest.write_text(json.dumps({"examples": [entry, entry]}))
    with pytest.raises(ValueError, match="duplicate"):
        clients.prepare_guide_readers(tmp_path / "duplicate", staged)


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


@pytest.mark.parametrize("owner", ["libtmux", "libtmux-tokio"])
def test_public_reader_features_exclude_the_external_fixture(owner: str) -> None:
    rows = [
        "libtmux-core v0.1.2 (/staged/core)\t",
        "libtmux v0.1.2 (/staged/facade)\tblocking,tokio",
        "libtmux-tokio v0.1.2 (/staged/tokio)\tquery",
    ]
    enabled = clients.check_reader_features(rows)
    assert enabled["libtmux"] == ["blocking", "tokio"]
    assert enabled["libtmux-tokio"] == ["query"]
    mutated = [row + ",test-support" if row.startswith(owner + " v") else row for row in rows]
    with pytest.raises(ValueError, match="fixture feature"):
        clients.check_reader_features(mutated)


def test_cargo_tree_output_survives_colored_ci_logs(tmp_path: Path, monkeypatch) -> None:
    def crate(name, dependencies):
        root = tmp_path / name
        (root / "src").mkdir(parents=True)
        (root / "src/lib.rs").touch()
        (root / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "0.1.2"\nedition = "2021"\n'
            + "[dependencies]\n"
            + "".join(f'{dep} = {{ path = "../{dep}" }}\n' for dep in dependencies)
        )
        return root

    crate("leaf", [])
    core = crate("libtmux-core", ["leaf"])
    crate("libtmux", ["libtmux-core"])
    reader = crate("reader", ["libtmux", "libtmux-core"])
    monkeypatch.setenv("CARGO_TERM_COLOR", "always")
    rows = clients.Cargo(tmp_path).tree(reader, "--offline")
    assert clients.check_dependency_tree(rows, {"libtmux-core": core}, expect_tokio=False) == [
        f"libtmux-core v0.1.2 ({core})"
    ]


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


def test_withdrawn_link_proof_requires_a_stale_refusal_and_unchanged_survivor_focus() -> None:
    generation = {"pid": 123, "started": 456}

    def row(session: str, index: int, window: str, *, active: bool) -> dict:
        return {
            "id": window,
            "session_id": session,
            "index": index,
            "active": active,
            "link": {
                "session_id": session,
                "index": index,
                "window_id": window,
                "generation": generation,
            },
        }

    saved = row("$2", 7, "@3", active=False)["link"]
    before = [row("$1", 0, "@3", active=True), row("$2", 0, "@4", active=True)]
    error = {"data": {"kind": "link_gone", "retryable": False, "stale": True}}
    refusal = {"isError": True, "content": [{"type": "text", "text": json.dumps(error)}]}
    record = clients.check_withdrawn_link(saved, before, before, refusal)
    assert record["focusPreserved"] is True
    assert record["saved"] == saved
    for broken in [
        {**refusal, "isError": False},
        {
            **refusal,
            "content": [
                {"type": "text", "text": json.dumps({"data": {**error["data"], "retryable": True}})}
            ],
        },
        {
            **refusal,
            "content": [
                {
                    "type": "text",
                    "text": json.dumps({"data": {**error["data"], "kind": "object_gone"}}),
                }
            ],
        },
    ]:
        with pytest.raises(ValueError, match="refusal"):
            clients.check_withdrawn_link(saved, before, before, broken)
    for changed in [
        [{**before[0], "active": False}, before[1]],
        before[1:],
        [*before, row("$2", 7, "@3", active=False)],
    ]:
        with pytest.raises(ValueError):
            clients.check_withdrawn_link(saved, before, changed, refusal)
    with pytest.raises(ValueError, match="surviving"):
        clients.check_withdrawn_link(saved, before[1:], before[1:], refusal)


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


def test_native_workflows_share_the_recorder_pin_and_keep_the_proof_artifact() -> None:
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
    docs = yaml.safe_load((clients.ROOT / ".github/workflows/docs.yml").read_text())
    for job, name in [("build", "port-docs"), ("publish", "reusable-deploy")]:
        assert docs["jobs"][job]["uses"] == (
            f"libtmux/docs/.github/workflows/{name}.yml@{clients.RECORDER_REVISION}"
        )
    [verifier] = [
        step
        for step in docs["jobs"]["installed-examples"]["steps"]
        if step.get("with", {}).get("repository") == "libtmux/docs"
    ]
    assert verifier["with"]["ref"] == clients.RECORDER_REVISION


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
