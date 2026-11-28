#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Build external archive consumers and installed apps, retaining their native proof.

The recorder is the pinned standalone script from libtmux/docs. Cargo work
uses one fresh target; no source path in this checkout patches a dependency.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import re
import runpy
import selectors
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from collections.abc import Iterator, Mapping, Sequence
from pathlib import Path
from typing import IO

LIBRARIES = ("libtmux-core", "libtmux-blocking", "libtmux-tokio", "libtmux-macros", "libtmux")
APPLICATIONS = ("tmux-mcp", "tmux-workspace")
PACKAGES = (*LIBRARIES, *APPLICATIONS)
ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path("/tmp/libtmux-rs-test")
RECORDER_REVISION = "f2e7244263b4e7ee2185b46dc91e43416625a9a1"
STREAM_LIMIT = 8 * 1024 * 1024

FIXTURE_PROGRAM = """\
use std::io::{self, Write};
use libtmux::tokio::test::TestServer;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let guard = runtime.block_on(TestServer::new())?;
    let prepared = runtime.block_on(async {
        let receipt = guard.server().new_session(libtmux::NewSession::new("installed-keeper")
            .size(40, 4).command("printf 'installed app pane\\n'; exec cat")).await?;
        let pane = guard.server().pane_by_id(&receipt.pane().id()).await?.ok_or("missing fixture pane")?;
        let arrived = pane.wait_for_text("installed app pane", std::time::Duration::from_secs(5)).await?;
        if arrived != libtmux::tokio::PaneWait::Arrived { return Err("fixture pane did not print".into()); }
        println!("{}", guard.socket_path().display());
        println!("{}", receipt.pane().id());
        io::stdout().flush()?;
        let window = guard.server().window_by_id(&receipt.window()).await?
            .ok_or("missing fixture window")?;
        if window.key() != &receipt.window_key() { return Err("fixture window generation changed".into()); }
        let mut linked = None;
        loop {
            let mut line = String::new();
            if io::stdin().read_line(&mut line)? == 0 { break; }
            let fields: Vec<_> = line.split_whitespace().collect();
            match fields.as_slice() {
                ["link", session, index] if linked.is_none() => {
                    let session = guard.server().session_by_id(&session.parse()?).await?
                        .ok_or("missing workspace session")?;
                    let index = index.parse()?;
                    window.link_to(&session, Some(index)).await?;
                    linked = Some(session.window_at(index).await?.ok_or("missing new link")?.link());
                    println!("linked");
                }
                ["unlink"] => {
                    linked.take().ok_or("fixture link is not present")?.unlink().await?;
                    println!("unlinked");
                }
                _ => return Err("invalid fixture control command".into()),
            }
            io::stdout().flush()?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    });
    let cleaned = runtime.block_on(guard.shutdown());
    prepared?;
    cleaned?;
    Ok(())
}
"""


def sha256(path: Path) -> str:
    """Hash the exact file used by the downstream proof."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def dependencies(manifest: Mapping) -> Iterator[tuple[str, Mapping]]:
    """Visit native edges, including renamed and target-specific dependencies."""
    for field in ("dependencies", "dev-dependencies", "build-dependencies"):
        for alias, specification in manifest.get(field, {}).items():
            spec = {"version": specification} if isinstance(specification, str) else specification
            yield spec.get("package", alias), spec
    for target in manifest.get("target", {}).values():
        yield from dependencies(target)


def check_sibling_pins(manifest: Mapping, versions: Mapping[str, str]) -> None:
    """Refuse an archive whose native requirements can select another release."""
    for package, spec in dependencies(manifest):
        if package in versions and spec.get("version") != f"={versions[package]}":
            raise ValueError(f"{manifest['package']['name']} does not pin {package} exactly")


def check_dependency_tree(
    rows: Sequence[str], staged: Mapping[str, Path], *, expect_tokio: bool
) -> list[str]:
    """Require one staged core and reject native dependencies from other sources."""
    rows = sorted({row.removesuffix(" (*)") for row in rows})
    native = []
    for row in rows:
        for package, path in staged.items():
            if row.startswith(f"{package} v"):
                if not row.endswith(f" ({path})"):
                    raise ValueError(f"{package} did not resolve its staged source: {row}")
                native.append(row)
    if len([row for row in native if row.startswith("libtmux-core v")]) != 1:
        raise ValueError("consumer did not resolve one staged core")
    if any(row.startswith("tokio v") for row in rows) != expect_tokio:
        raise ValueError("consumer resolved an unexpected Tokio dependency")
    return native


def check_reader_features(rows: Sequence[str]) -> dict[str, list[str]]:
    """Keep fixture features out of the reader's resolved native dependencies."""
    selected = {}
    for row in rows:
        identity, separator, features = row.removesuffix(" (*)").rpartition("\t")
        package = identity.split(" ", 1)[0]
        if package not in LIBRARIES:
            continue
        if not separator:
            raise ValueError("reader feature tree omitted resolved features")
        enabled = sorted({feature.strip() for feature in features.split(",") if feature.strip()})
        if "test-support" in enabled:
            raise ValueError(f"reader enabled the fixture feature in {package}")
        selected[package] = enabled
    if "libtmux-core" not in selected:
        raise ValueError("reader feature tree omitted the shared core")
    return dict(sorted(selected.items()))


def check_record(record: dict, output: Path, expected: bytes, *, require_capture: bool) -> dict:
    """Reject stale stdout, incomplete execution and failed owned-endpoint cleanup."""
    if (
        record.get("passed") is not True
        or record.get("status") != 0
        or record.get("timedOut") is not False
        or record.get("stdoutHex") != expected.hex()
        or (output / "stdout.bin").read_bytes() != expected
        or record.get("cleanup", {}).get("required") is not True
        or record.get("cleanup", {}).get("passed") is not True
    ):
        raise ValueError("bare recorder did not confirm stdout and completed cleanup")
    if require_capture:
        witness = record.get("native")
        if (
            not witness
            or witness.get("beforeHex") != expected.hex()
            or witness.get("afterHex") != expected.hex()
        ):
            raise ValueError("bare recorder did not confirm the native screen bytes")
    return record


def capture_bytes(capture: Mapping, pane: str) -> bytes:
    """Restore the capture tool's documented omitted terminal newline."""
    if capture.get("pane") != pane or not isinstance(capture.get("text"), str):
        raise ValueError("MCP capture did not report the requested pane")
    text = capture["text"]
    if capture.get("lines") != text.count("\n") + 1:
        raise ValueError("MCP capture line count differs from its text")
    return text.encode("utf-8") + b"\n"


def check_window_link(row: Mapping) -> dict:
    """Require the exact reusable placement carried beside one listed window."""
    link = row.get("link")
    if not isinstance(link, dict) or set(link) != {
        "session_id",
        "index",
        "window_id",
        "generation",
    }:
        raise ValueError("MCP window omitted its exact link reference")
    generation = link["generation"]
    if (
        not isinstance(generation, dict)
        or set(generation) != {"pid", "started"}
        or type(generation["pid"]) is not int
        or not 0 < generation["pid"] < 2**32
        or type(generation["started"]) is not int
        or not -(2**63) <= generation["started"] < 2**63
        or not isinstance(link["session_id"], str)
        or not re.fullmatch(r"\$[0-9]+", link["session_id"])
        or not isinstance(link["window_id"], str)
        or not re.fullmatch(r"@[0-9]+", link["window_id"])
        or type(link["index"]) is not int
        or not -(2**31) <= link["index"] < 2**31
        or (link["session_id"], link["index"], link["window_id"])
        != (row.get("session_id"), row.get("index"), row.get("id"))
    ):
        raise ValueError("MCP window link differs from its listed placement or generation")
    return link


def check_withdrawn_link(
    saved: Mapping, before: Sequence[Mapping], after: Sequence[Mapping], refusal: Mapping
) -> dict:
    """Confirm stale-link refusal without changing any surviving placement or focus."""
    try:
        [content] = refusal["content"]
        error = json.loads(content["text"])
        detail = error["data"]
    except (KeyError, ValueError, TypeError) as cause:
        raise ValueError("MCP omitted its withdrawn-link refusal") from cause
    if (
        refusal.get("isError") is not True
        or content.get("type") != "text"
        or detail.get("kind") != "link_gone"
        or detail.get("retryable") is not False
        or detail.get("stale") is not True
    ):
        raise ValueError("MCP omitted its non-retryable stale-link refusal")

    def state(rows: Sequence[Mapping]) -> list[tuple]:
        result = []
        for row in rows:
            link = check_window_link(row)
            if link["generation"] != saved["generation"] or type(row.get("active")) is not bool:
                raise ValueError("MCP link proof changed generation or omitted focus")
            if link == saved:
                raise ValueError("withdrawn placement is still present")
            result.append((link["session_id"], link["index"], link["window_id"], row["active"]))
        if len(set(result)) != len(result):
            raise ValueError("MCP link proof repeats a placement")
        return sorted(result)

    before_state, after_state = state(before), state(after)
    survivors = [row for row in before if row["id"] == saved["window_id"]]
    if not survivors:
        raise ValueError("MCP link proof has no surviving intrinsic window")
    if before_state != after_state:
        raise ValueError("MCP stale-link call changed a surviving placement or focus")
    return {
        "saved": dict(saved),
        "refusal": error,
        "before": list(before),
        "after": list(after),
        "survivingLinks": survivors,
        "focusPreserved": True,
    }


def check_workspace(summary: Mapping) -> None:
    """Require a successful machine load with newly created native identities."""
    if any(
        summary.get(key) != value
        for key, value in {
            "schema_version": 1,
            "command": "load",
            "status": "ok",
            "errors": [],
        }.items()
    ):
        raise ValueError("installed workspace command did not report a successful load")
    results = summary.get("results", [])
    if len(results) != 1:
        raise ValueError("installed workspace command did not report one input")
    result = results[0]
    if (
        result.get("session_name") != "installed-workspace"
        or result.get("owned_session") is not True
        or not re.fullmatch(r"\$[0-9]+", result.get("session_id", ""))
        or len(result.get("window_ids", [])) != 1
        or len(result.get("pane_ids", [])) != 1
        or not re.fullmatch(r"@[0-9]+", result["window_ids"][0])
        or not re.fullmatch(r"%[0-9]+", result["pane_ids"][0])
    ):
        raise ValueError("installed workspace command omitted created identities")


class Cargo:
    """Run Cargo against fresh extracted sources under one bounded build timeout."""

    def __init__(self, directory: Path):
        self.target = directory / "target"
        self.env = {**os.environ, "CARGO_TARGET_DIR": str(self.target)}

    def run(self, cwd: Path, *arguments: str) -> bytes:
        return subprocess.run(
            ["cargo", *arguments],
            cwd=cwd,
            env=self.env,
            stdout=subprocess.PIPE,
            check=True,
            timeout=1200,
        ).stdout

    def tree(self, cwd: Path, *arguments: str) -> list[str]:
        return (
            self.run(
                cwd,
                "tree",
                "--color",
                "never",
                *arguments,
                "-e",
                "normal",
                "--prefix",
                "none",
                "--format",
                "{p}",
            )
            .decode()
            .splitlines()
        )

    def features(self, cwd: Path) -> list[str]:
        return (
            self.run(
                cwd,
                "tree",
                "--locked",
                "--color",
                "never",
                "-e",
                "normal",
                "--prefix",
                "none",
                "--format",
                "{p}\t{f}",
            )
            .decode()
            .splitlines()
        )


def stage_archives(
    cargo: Cargo, directory: Path, output: Path, source_revision: str
) -> tuple[dict[str, Path], dict[str, str], list[dict]]:
    """Extract every publishable archive and retain its original digest."""
    packages = [item for name in PACKAGES for item in ("-p", name)]
    cargo.run(ROOT, "package", "--locked", "--no-verify", *packages)
    staged = {}
    versions = {}
    archives = []
    destination = output / "archives"
    destination.mkdir()
    for package in PACKAGES:
        candidates = [
            path
            for path in (cargo.target / "package").glob(f"{package}-*.crate")
            if path.name[len(package) + 1].isdigit()
        ]
        if len(candidates) != 1:
            raise ValueError(f"expected one fresh {package} archive, found {len(candidates)}")
        [archive] = candidates
        with tarfile.open(archive) as tar:
            tar.extractall(directory, filter="data")
        staged[package] = (directory / archive.stem).resolve()
        manifest = tomllib.loads((staged[package] / "Cargo.toml").read_text())
        vcs = json.loads((staged[package] / ".cargo_vcs_info.json").read_bytes())
        if vcs["git"]["sha1"] != source_revision or vcs["git"].get("dirty"):
            raise ValueError(f"{archive.name} does not match the clean source revision")
        if manifest["package"]["name"] != package:
            raise ValueError(f"{archive.name} contains another package")
        versions[package] = manifest["package"]["version"]
        archives.append(
            {"package": package, "version": versions[package], "sha256": sha256(archive)}
        )
        (destination / archive.name).write_bytes(archive.read_bytes())
    for source in staged.values():
        check_sibling_pins(tomllib.loads((source / "Cargo.toml").read_text()), versions)
    return staged, versions, archives


def record(recorder: Path, output: Path, command: Sequence[str], profile: str) -> dict:
    """Run the shared recorder unchanged and preserve its complete artefacts."""
    subprocess.run(
        [
            sys.executable,
            str(recorder),
            "--output-dir",
            str(output),
            *([profile] if profile else []),
            "--timeout",
            "120",
            "--",
            *command,
        ],
        check=True,
        timeout=140,
    )
    return json.loads((output / "result.json").read_bytes())


def recorder_identity(recorder: Path) -> dict[str, str]:
    """Require the exact committed shared recorder selected by the CI job."""
    revision = subprocess.check_output(
        ["git", "-C", str(recorder.parent), "rev-parse", "HEAD"], text=True
    ).strip()
    dirty = subprocess.check_output(
        ["git", "-C", str(recorder.parent), "status", "--porcelain", "--", str(recorder)], text=True
    )
    if revision != RECORDER_REVISION or dirty:
        raise ValueError(f"recorder must be clean at libtmux/docs {RECORDER_REVISION}")
    return {
        "repository": "https://github.com/libtmux/docs",
        "revision": revision,
        "sha256": sha256(recorder),
    }


def registry_patches(staged: Mapping[str, Path]) -> str:
    return "[patch.crates-io]\n" + "".join(
        f"{name} = {{ path = {json.dumps(str(path))} }}\n" for name, path in staged.items()
    )


def readme_program(crate: Path, client: str) -> str:
    """Copy a complete reader from its staged crate README."""
    blocks = re.findall(
        rf"<!-- {re.escape(client)}-example -->\s*```rust,no_run\n(.*?)\n```\s*"
        rf"<!-- /{re.escape(client)}-example -->",
        (crate / "README.md").read_text(),
        re.DOTALL,
    )
    if len(blocks) != 1:
        raise ValueError(f"expected one complete {client} README program")
    return (
        "\n".join(
            line for line in blocks[0].splitlines() if line != "#" and not line.startswith("# ")
        )
        + "\n"
    )


def prepare_guide_readers(
    project: Path, staged: Mapping[str, Path]
) -> dict[str, tuple[Path, dict]]:
    """Copy each archived guide into a standalone binary target."""
    crate = staged["libtmux-tokio"]
    examples = crate / "examples"
    entries = json.loads((examples / "api/manifest.json").read_bytes())["examples"]
    binaries = project / "src/bin"
    binaries.mkdir(parents=True)
    manifest = (examples / "api/consumer.toml").read_text()
    path = tomllib.loads(manifest)["dependencies"]["libtmux-tokio"]["path"]
    # Keep the guide's feature selection and supporting dependencies. Its
    # source checkout is supplied here by the unpacked crate.
    manifest = manifest.replace(json.dumps(path), json.dumps(str(crate)))
    if tomllib.loads(manifest)["dependencies"]["libtmux-tokio"]["path"] != str(crate):
        raise ValueError("the guide manifest did not resolve the staged Tokio crate")
    (project / "Cargo.toml").write_text(manifest + "\n" + registry_patches(staged))
    programs = {}
    for entry in entries:
        name = entry["cargoExample"]
        if not re.fullmatch(r"api_[a-z_]+", name) or name in programs:
            raise ValueError("duplicate or invalid guide binary name")
        if entry["sourceFile"] != f"crates/libtmux-tokio/examples/{name}.rs":
            raise ValueError("guide source disagrees with its Cargo example")
        source = binaries / f"{name}.rs"
        source.write_bytes((examples / source.name).read_bytes())
        programs[name] = (source, entry)
    return programs


def run_guide_examples(
    cargo: Cargo, directory: Path, staged: dict[str, Path], recorder: Path, output: Path
) -> list[dict]:
    """Run the published Tokio guide programs with their reader's dependencies."""
    project = directory / "guide-readers"
    programs = prepare_guide_readers(project, staged)
    check_dependency_tree(cargo.tree(project), staged, expect_tokio=True)
    cargo.run(project, "build", "--locked", "--quiet", "--bins")
    records = []
    for name, (source, entry) in programs.items():
        target = output / "guide-readers" / name
        expected = (entry["expectedOutput"] + "\n").encode()
        observed = record(recorder, target, [str(cargo.target / "debug" / name)], "")
        check_record(observed, target, expected, require_capture=False)
        records.append(
            {
                "program": entry["id"],
                "programSha256": sha256(source),
                "stdoutHex": observed["stdoutHex"],
                "cleanup": observed["cleanup"],
            }
        )
    return records


def build_fixture(
    cargo: Cargo,
    directory: Path,
    staged: Mapping[str, Path],
    versions: Mapping[str, str],
    extras: str,
) -> Path:
    """Build the external test fixture in a separate Cargo feature graph."""
    project = directory / "fixture-only"
    (project / "src").mkdir(parents=True)
    (project / "Cargo.toml").write_text(
        '[package]\nname = "installed-fixture"\nversion = "0.0.0"\nedition = "2024"\n'
        "[dependencies]\n"
        f'libtmux = {{ version = "={versions["libtmux"]}", default-features = false, features = ["tokio", "test-support"] }}\n'
        f"{extras}\n{registry_patches(staged)}"
    )
    (project / "src/main.rs").write_text(FIXTURE_PROGRAM)
    check_dependency_tree(cargo.tree(project), staged, expect_tokio=True)
    cargo.run(project, "build", "--locked", "--quiet")
    return cargo.target / "debug/installed-fixture"


def consume(
    cargo: Cargo,
    directory: Path,
    staged: dict[str, Path],
    versions: dict[str, str],
    recorder: Path,
    output: Path,
) -> tuple[list[dict], dict, Path]:
    """Build and execute pure, blocking, Tokio and mixed external programs."""
    patches = registry_patches(staged)
    facade = staged["libtmux"]
    blocking = readme_program(facade, "blocking")
    mixed = tomllib.loads((facade / "examples/api/consumer.toml").read_text())["dependencies"]
    extras = "\n".join(
        f"{name} = {{ version = {json.dumps(spec['version'])}, features = {json.dumps(spec['features'])} }}"
        for name, spec in mixed.items()
        if name != "libtmux"
    )
    lanes = [
        (
            "core",
            "libtmux-core",
            [],
            'fn main() { let _ = libtmux_core::NewSession::new("work"); }\n',
        ),
        ("blocking", "libtmux", [], blocking),
        ("blocking-control", "libtmux", ["blocking", "control-mode"], blocking),
        ("tokio", "libtmux-tokio", [], readme_program(staged["libtmux-tokio"], "tokio")),
        (
            "mixed",
            "libtmux",
            mixed["libtmux"]["features"],
            (facade / "examples/api_mixed.rs").read_text(),
        ),
    ]
    consumers = []
    mixed_record = {}
    fixture_binary = build_fixture(cargo, directory, staged, versions, extras)
    for lane, package, features, source in lanes:
        project = directory / f"reader-{lane}"
        (project / "src").mkdir(parents=True)
        (project / "Cargo.toml").write_text(
            f'[package]\nname = "reader-{lane}"\nversion = "0.0.0"\nedition = "2024"\n'
            f'[dependencies]\n{package} = {{ version = "={versions[package]}", default-features = {str(lane in {"blocking", "tokio"}).lower()}, features = {json.dumps(features)} }}\n'
            f"{extras if lane in {'tokio', 'mixed'} else ''}\n{patches}"
        )
        (project / "src/main.rs").write_text(source)
        selected = check_dependency_tree(
            cargo.tree(project), staged, expect_tokio=lane in {"tokio", "mixed"}
        )
        reader_features = check_reader_features(cargo.features(project))
        cargo.run(project, "build", "--locked", "--quiet")
        binary = cargo.target / f"debug/reader-{lane}"
        expected = (
            b"created and cleaned one session\n"
            if lane.startswith("blocking")
            else b"mixed runtime\n"
            if lane == "mixed"
            else b"installed-keeper\n"
            if lane == "tokio"
            else b""
        )
        if lane == "core":
            actual = subprocess.run(
                [str(binary)], capture_output=True, check=True, timeout=30
            ).stdout
            if actual != expected:
                raise ValueError("pure consumer emitted unexpected stdout")
        else:
            if lane in {"tokio", "mixed"}:
                driver = directory / f"driver-{lane}"
                command = [
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "--drive-reader",
                    "--reader-binary",
                    str(binary),
                    "--fixture-binary",
                    str(fixture_binary),
                    "--output-dir",
                    str(driver),
                ]
                observed = record(
                    recorder, output / lane, command, "--require-capture" if lane == "mixed" else ""
                )
            else:
                FIXTURES.mkdir(mode=0o700, exist_ok=True)
                with tempfile.TemporaryDirectory(
                    prefix="installed-reader-", dir=FIXTURES
                ) as fixture:
                    observed = record(recorder, output / lane, [str(binary), f"{fixture}/s"], "")
            check_record(observed, output / lane, expected, require_capture=lane == "mixed")
            if lane == "mixed":
                failures = []
                for scenario in (
                    "name-conflict",
                    "creation-followup",
                    "read-failure",
                    "read-and-cleanup-failure",
                ):
                    driver = directory / f"driver-{lane}-{scenario}"
                    target = output / "mixed-failures" / scenario
                    failure = record(
                        recorder,
                        target,
                        [
                            sys.executable,
                            str(Path(__file__).resolve()),
                            "--drive-reader",
                            "--reader-binary",
                            str(binary),
                            "--fixture-binary",
                            str(fixture_binary),
                            "--reader-case",
                            scenario,
                            "--output-dir",
                            str(driver),
                        ],
                        "",
                    )
                    check_record(failure, target, b"", require_capture=False)
                    failures.append(
                        {
                            **json.loads((driver / "reader.json").read_bytes()),
                            "cleanup": failure["cleanup"],
                        }
                    )
                mixed_record = {
                    "programSha256": sha256(project / "src/main.rs"),
                    "stdoutHex": observed["stdoutHex"],
                    "native": observed["native"],
                    "cleanup": observed["cleanup"],
                    "profile": "raw-stdout-native-capture",
                    "fixtureOutsideReader": True,
                    "normalNativeFeatures": reader_features,
                    "fixtureProgramSha256": hashlib.sha256(FIXTURE_PROGRAM.encode()).hexdigest(),
                    "runCommandSha256": hashlib.sha256(
                        json.loads((facade / "examples/api/manifest.json").read_bytes())[
                            "runCommand"
                        ].encode()
                    ).hexdigest(),
                    "failureCases": failures,
                }
        consumers.append(
            {
                "name": lane,
                "normalNativeDependencies": selected,
                "normalNativeFeatures": reader_features,
                "programSha256": sha256(project / "src/main.rs"),
                "stdoutHex": expected.hex(),
            }
        )
    return consumers, mixed_record, fixture_binary


def verify_external_pins(shipped: Mapping, resolved: Mapping, staged: Mapping[str, Path]) -> None:
    """Rebinding sibling archives must preserve every external version and checksum."""

    def external(lock: Mapping) -> set[tuple]:
        return {
            (package["name"], package["version"], package.get("source"), package.get("checksum"))
            for package in lock["package"]
            if package["name"] not in staged
        }

    if external(shipped) != external(resolved):
        raise ValueError("staged application changed a dependency pinned in the shipped Cargo.lock")


def install_apps(cargo: Cargo, directory: Path, staged: dict[str, Path]) -> tuple[Path, list[dict]]:
    """Install the extracted commands without changing their shipped manifests."""
    config = directory / "staged-registry.toml"
    config.write_text(registry_patches(staged))
    installed = directory / "installed"
    records = []
    for package in APPLICATIONS:
        path = staged[package]
        arguments = ("--config", str(config))
        lockfile = path / "Cargo.lock"
        shipped = tomllib.loads(lockfile.read_text())
        selected = check_dependency_tree(cargo.tree(path, *arguments), staged, expect_tokio=True)
        verify_external_pins(shipped, tomllib.loads(lockfile.read_text()), staged)
        cargo.run(
            path,
            "install",
            "--path",
            str(path),
            "--root",
            str(installed),
            "--debug",
            "--locked",
            *arguments,
        )
        binary = installed / "bin" / package
        version = tomllib.loads((path / "Cargo.toml").read_text())["package"]["version"]
        reported = subprocess.run(
            [str(binary), "--version"], capture_output=True, check=True, timeout=10
        )
        if reported.stdout != f"{package} {version}\n".encode() or reported.stderr:
            raise ValueError(f"installed {package} did not report its staged version on stdio")
        records.append(
            {
                "package": package,
                "version": version,
                "versionStdoutHex": reported.stdout.hex(),
                "binarySha256": sha256(binary),
                "normalNativeDependencies": selected,
            }
        )
    return installed / "bin", records


class JsonLines:
    """Read bounded newline records from child stdout under one absolute timeout."""

    def __init__(self, stream: IO[bytes]):
        self.stream = stream
        self.pending = bytearray()
        self.raw = bytearray()
        self.deadline = time.monotonic() + 30

    def line(self) -> bytes:
        while b"\n" not in self.pending:
            with selectors.DefaultSelector() as selector:
                selector.register(self.stream, selectors.EVENT_READ)
                if not selector.select(max(0, self.deadline - time.monotonic())):
                    raise TimeoutError("installed command stopped answering on stdout")
            block = os.read(self.stream.fileno(), 8192)
            if not block:
                raise ValueError("installed command closed stdout before its response")
            if len(self.raw) + len(block) > STREAM_LIMIT:
                raise ValueError("installed command exceeded the stdio proof byte limit")
            self.pending.extend(block)
            self.raw.extend(block)
        end = self.pending.index(b"\n") + 1
        line = bytes(self.pending[:end])
        del self.pending[:end]
        return line

    def finish(self) -> bytes:
        """Retain every stdout byte through EOF after the request channel closes."""
        tail = bytearray(self.pending)
        self.pending.clear()
        deadline = time.monotonic() + 10
        while True:
            with selectors.DefaultSelector() as selector:
                selector.register(self.stream, selectors.EVENT_READ)
                if not selector.select(max(0, deadline - time.monotonic())):
                    raise TimeoutError("installed command did not close stdout")
            block = os.read(self.stream.fileno(), 8192)
            if not block:
                if tail and not tail.endswith(b"\n"):
                    raise ValueError("installed command ended with an incomplete stdio record")
                return bytes(tail)
            if len(self.raw) + len(block) > STREAM_LIMIT:
                raise ValueError("installed command exceeded the stdio proof byte limit")
            self.raw.extend(block)
            tail.extend(block)


def close_child(process: subprocess.Popen) -> None:
    """Close stdio, then stop a child which does not finish within its allowance."""
    if process.stdin:
        with contextlib.suppress(BrokenPipeError, OSError):
            process.stdin.close()
    try:
        status = process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
        raise
    if status:
        raise ValueError(f"installed command exited with status {status}")


def drive_reader(reader: Path, fixture_binary: Path, output: Path, scenario: str) -> None:
    """Borrow an external fixture and check the reader preserved its prior sessions."""
    output.mkdir()
    with (output / "fixture.stderr").open("wb") as errors:
        host = subprocess.Popen(
            [str(fixture_binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors
        )
        try:
            assert host.stdout is not None
            stream = JsonLines(host.stdout)
            socket = stream.line().decode().strip()
            pane = stream.line().decode().strip()
            if not Path(socket).resolve().is_relative_to(FIXTURES.resolve()) or not re.fullmatch(
                r"%[0-9]+", pane
            ):
                raise ValueError("fixture did not report a private endpoint and exact pane")

            def tmux(*arguments: str) -> bytes:
                return subprocess.run(
                    ["tmux", "-N", "-S", socket, *arguments],
                    capture_output=True,
                    check=True,
                    timeout=10,
                ).stdout

            if scenario == "name-conflict":
                tmux("new-session", "-d", "-s", "work")
            if scenario == "read-failure":
                tmux("set-hook", "-g", "after-list-panes", "select-pane -t %4294967295")
            if scenario == "read-and-cleanup-failure":
                tmux(
                    "set-hook",
                    "-g",
                    "after-list-panes",
                    "kill-session -t work ; select-pane -t %4294967295",
                )
            if scenario == "creation-followup":
                tmux("set-hook", "-g", "after-new-session", "select-pane -t %4294967295")
            projection = "#{pid},#{start_time},#{session_id},#{session_name},#{session_windows}"
            before = tmux("list-sessions", "-F", projection)
            result = subprocess.run(
                [str(reader), socket], capture_output=True, check=False, timeout=30
            )
            after = tmux("list-sessions", "-F", projection)
            (output / "reader.stdout").write_bytes(result.stdout)
            (output / "reader.stderr").write_bytes(result.stderr)
            if len(result.stdout) + len(result.stderr) > STREAM_LIMIT:
                raise ValueError("reader stdio exceeded its proof limit")
            if scenario == "success" and result.returncode == 0:
                sys.stdout.buffer.write(result.stdout)
                sys.stdout.buffer.flush()
            if before != after:
                raise ValueError("reader changed or leaked a session outside its returned receipt")
            if (result.returncode == 0) != (scenario == "success"):
                raise ValueError("reader did not report the expected operation outcome")
            if scenario == "creation-followup" and not (
                b"CreationFollowup" in result.stderr and b"receipt: Some(" in result.stderr
            ):
                raise ValueError("reader omitted its verified partial creation receipt")
            if scenario == "read-and-cleanup-failure" and not all(
                field in result.stderr
                for field in (b"OperationAndCleanup", b"primary:", b"cleanup:")
            ):
                raise ValueError("reader did not retain operation and cleanup failures together")
            if scenario != "success" and result.stdout:
                raise ValueError("a failed reader exposed pane bytes")
            sys.stderr.buffer.write(result.stderr)
            sys.stderr.buffer.flush()
            (output / "reader.json").write_text(
                json.dumps(
                    {
                        "scenario": scenario,
                        "readerStatus": result.returncode,
                        "stdoutHex": result.stdout.hex(),
                        "stderrSha256": hashlib.sha256(result.stderr).hexdigest(),
                        "beforeHex": before.hex(),
                        "afterHex": after.hex(),
                        "borrowedSessionsPreserved": True,
                    },
                    indent=2,
                )
                + "\n"
            )
        except Exception as failure:
            try:
                close_child(host)
            except (OSError, subprocess.SubprocessError, ValueError) as cleanup:
                raise ExceptionGroup(
                    "reader and fixture cleanup failed", [failure, cleanup]
                ) from None
            raise
        else:
            close_child(host)


def drive_applications(binaries: Path, fixture_binary: Path, output: Path) -> None:
    """Exercise both installed commands on a library-owned daemon inside the recorder."""
    output.mkdir()
    env = {
        **os.environ,
        "SHELL": "/bin/sh",
        "LIBTMUX_TOOLSETS": "inspect",
        "LIBTMUX_TMUX_CONFIG": "/dev/null",
        "NO_COLOR": "1",
    }
    for name in (
        "TMUX",
        "TMUX_PANE",
        "LIBTMUX_SOCKET",
        "LIBTMUX_SOCKET_PATH",
        "LIBTMUX_TOOLS",
        "LIBTMUX_EXCLUDE_TOOLS",
        "LIBTMUX_ENVIRONMENT_VALUES",
    ):
        env.pop(name, None)
    env["XDG_CONFIG_HOME"] = str(output / "config")
    with (output / "fixture.stderr").open("wb") as errors:
        host = subprocess.Popen(
            [str(fixture_binary)],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=errors,
        )
        try:
            assert host.stdout is not None
            stream = JsonLines(host.stdout)
            socket = stream.line().decode().strip()
            pane = stream.line().decode().strip()
            if not Path(socket).resolve().is_relative_to(FIXTURES.resolve()) or not re.fullmatch(
                r"%[0-9]+", pane
            ):
                raise ValueError("fixture did not report a private endpoint and exact pane")
            fixture_control = []

            def fixture_command(command: str, expected: bytes) -> None:
                assert host.stdin is not None
                encoded = command.encode() + b"\n"
                host.stdin.write(encoded)
                host.stdin.flush()
                answer = stream.line()
                fixture_control.append({"stdinHex": encoded.hex(), "stdoutHex": answer.hex()})
                if answer != expected:
                    raise ValueError("fixture did not confirm its exact link operation")

            document = output / "workspace.json"
            document.write_text(
                json.dumps(
                    {
                        "session_name": "installed-workspace",
                        "windows": [{"window_name": "proof", "panes": ["blank"]}],
                    }
                )
            )
            workspace = subprocess.run(
                [
                    str(binaries / "tmux-workspace"),
                    "load",
                    "--json",
                    "-d",
                    "-S",
                    socket,
                    str(document),
                ],
                env=env,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                check=True,
                timeout=30,
            )
            if len(workspace.stdout) + len(workspace.stderr) > STREAM_LIMIT:
                raise ValueError("workspace stdio exceeded its proof limit")
            summary = json.loads(workspace.stdout)
            check_workspace(summary)
            transcript = []
            with (output / "mcp.stderr").open("wb") as mcp_errors:
                mcp = subprocess.Popen(
                    [str(binaries / "tmux-mcp"), "-S", socket],
                    env={**env, "LIBTMUX_TOOLS": "select_window"},
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=mcp_errors,
                )
                try:
                    assert mcp.stdout is not None and mcp.stdin is not None
                    stdin = mcp.stdin
                    protocol = JsonLines(mcp.stdout)
                    input_bytes = bytearray()
                    sequence = 0

                    def send(message: dict) -> None:
                        encoded = json.dumps(message).encode() + b"\n"
                        stdin.write(encoded)
                        stdin.flush()
                        input_bytes.extend(encoded)
                        transcript.append({"direction": "stdin", "message": message})

                    def exchange(method: str, params: dict) -> dict:
                        nonlocal sequence
                        sequence += 1
                        message = {
                            "jsonrpc": "2.0",
                            "id": sequence,
                            "method": method,
                            "params": params,
                        }
                        send(message)
                        while True:
                            line = protocol.line()
                            answer = json.loads(line)
                            transcript.append({"direction": "stdout", "message": answer})
                            if answer.get("jsonrpc") != "2.0":
                                raise ValueError("MCP stdout contains non-protocol data")
                            if answer.get("id") == sequence:
                                return answer

                    def request(method: str, params: dict) -> dict:
                        answer = exchange(method, params)
                        if "error" in answer:
                            raise ValueError(f"MCP refused {method}: {answer['error']}")
                        return answer["result"]

                    request(
                        "initialize",
                        {
                            "protocolVersion": "2025-06-18",
                            "capabilities": {},
                            "clientInfo": {"name": "installed-archive-proof", "version": "1"},
                        },
                    )
                    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
                    tools = request("tools/list", {})
                    names = {tool["name"] for tool in tools["tools"]}
                    if (
                        not {
                            "list_sessions",
                            "list_windows",
                            "capture_pane",
                            "select_window",
                        }.issubset(names)
                        or any(name.startswith("kill_") for name in names)
                        or any(
                            tool["name"] != "select_window"
                            and tool.get("_meta", {})
                            .get("com.git-pull.libtmux-mcp/capability", {})
                            .get("toolset")
                            != "inspect"
                            for tool in tools["tools"]
                        )
                    ):
                        raise ValueError(
                            "installed MCP exposed more than inspect and select_window"
                        )
                    uri = "tmux://capabilities"
                    resources = request("resources/list", {})["resources"]
                    if uri not in {item["uri"] for item in resources}:
                        raise ValueError("installed MCP omitted its capabilities resource")
                    [content] = request("resources/read", {"uri": uri})["contents"]
                    capabilities = json.loads(content["text"])
                    if content["uri"] != uri or set(capabilities["effectiveTools"]) != names:
                        raise ValueError("MCP capability report disagrees with tools/list")
                    refusal = exchange(
                        "tools/call",
                        {"name": "kill_session", "arguments": {"session": "$4294967295"}},
                    )
                    evidence = refusal.get("error", {}).get("data", {})
                    if (
                        evidence.get("kind") != "invalid_input"
                        or evidence.get("retryable") is not False
                    ):
                        raise ValueError(
                            "installed MCP omitted its scoped refusal and retry evidence"
                        )
                    sessions = request("tools/call", {"name": "list_sessions", "arguments": {}})
                    names = {item["name"] for item in sessions["structuredContent"]["sessions"]}
                    if names != {"installed-keeper", "installed-workspace"}:
                        raise ValueError(
                            "MCP did not inspect the workspace created by the installed CLI"
                        )

                    def window_rows() -> list[dict]:
                        answer = request("tools/call", {"name": "list_windows", "arguments": {}})
                        if answer.get("isError"):
                            raise ValueError("installed MCP could not list its exact placements")
                        rows = answer["structuredContent"]["windows"]
                        for row in rows:
                            check_window_link(row)
                        return rows

                    workspace_id = summary["results"][0]["session_id"]
                    keeper_id = next(
                        row["id"]
                        for row in sessions["structuredContent"]["sessions"]
                        if row["name"] == "installed-keeper"
                    )
                    fixture_command(f"link {workspace_id} 7", b"linked\n")
                    linked = window_rows()
                    [intended] = [
                        row
                        for row in linked
                        if row["session_id"] == workspace_id and row["index"] == 7
                    ]
                    saved = intended["link"]
                    if not any(
                        row["session_id"] == keeper_id and row["id"] == saved["window_id"]
                        for row in linked
                    ):
                        raise ValueError(
                            "installed MCP did not observe both links to the fixture window"
                        )
                    selected = request(
                        "tools/call", {"name": "select_window", "arguments": {"link": saved}}
                    )
                    if (
                        selected.get("isError")
                        or selected["structuredContent"]["windows"][0]["link"] != saved
                    ):
                        raise ValueError("installed MCP did not select the exact live listed link")
                    fixture_command("unlink", b"unlinked\n")
                    before_refusal = window_rows()
                    refused_link = request(
                        "tools/call", {"name": "select_window", "arguments": {"link": saved}}
                    )
                    withdrawn_link = check_withdrawn_link(
                        saved, before_refusal, window_rows(), refused_link
                    )
                    captured = request(
                        "tools/call", {"name": "capture_pane", "arguments": {"pane": pane}}
                    )
                    if captured.get("isError"):
                        raise ValueError("installed MCP capture returned a tool error")
                    screen = capture_bytes(captured["structuredContent"], pane)
                    if b"installed app pane\n" not in screen:
                        raise ValueError("installed MCP capture omitted the fixture greeting")
                    stdin.close()
                    for line in protocol.finish().splitlines():
                        answer = json.loads(line)
                        if answer.get("jsonrpc") != "2.0":
                            raise ValueError("MCP shutdown stdout contains non-protocol data")
                        transcript.append({"direction": "stdout", "message": answer})
                finally:
                    close_child(mcp)
            if (output / "mcp.stderr").stat().st_size > STREAM_LIMIT:
                raise ValueError("MCP stderr exceeded its proof limit")
            result = {
                "socket": socket,
                "pane": pane,
                "captureHex": screen.hex(),
                "withdrawnLink": withdrawn_link,
                "workspace": {
                    "stdinHex": "",
                    "stdoutHex": workspace.stdout.hex(),
                    "stderrHex": workspace.stderr.hex(),
                    "status": workspace.returncode,
                    "summary": summary,
                },
                "mcp": {
                    "stdinHex": input_bytes.hex(),
                    "stdoutHex": bytes(protocol.raw).hex(),
                    "stderrHex": (output / "mcp.stderr").read_bytes().hex(),
                    "status": mcp.returncode,
                    "transcript": transcript,
                },
            }
        finally:
            close_child(host)
        if stream.finish():
            raise ValueError("fixture printed unexpected stdout after its control replies")
        result["fixture"] = {"control": fixture_control, "stdoutHex": bytes(stream.raw).hex()}
        (output / "stdio.json").write_text(json.dumps(result, indent=2) + "\n")
    sys.stdout.write(json.dumps(result, sort_keys=True) + "\n")


def check_consumers(directory: Path, recorder: Path, output: Path) -> None:
    """Write the complete archive, consumer, installed-app and capture provenance."""
    recorder_provenance = recorder_identity(recorder)
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(
        subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT)
    )
    if dirty:
        raise ValueError("archive verification requires a clean source revision")
    output.mkdir(parents=True, exist_ok=False)
    (directory / "rust-toolchain.toml").write_bytes((ROOT / "rust-toolchain.toml").read_bytes())
    cargo = Cargo(directory)
    staged, versions, archives = stage_archives(cargo, directory, output, revision)
    consumers, mixed, fixture_binary = consume(cargo, directory, staged, versions, recorder, output)
    mixed["recorderSha256"] = recorder_provenance["sha256"]
    guides = run_guide_examples(cargo, directory, staged, recorder, output)
    binaries, apps = install_apps(cargo, directory, staged)
    app_output = output / "application-stdio"
    driver_output = directory / "application-driver"
    observed = record(
        recorder,
        app_output,
        [
            sys.executable,
            str(Path(__file__).resolve()),
            "--drive-applications",
            "--installed-bin",
            str(binaries),
            "--fixture-binary",
            str(fixture_binary),
            "--output-dir",
            str(driver_output),
        ],
        "--record-captures",
    )
    stdio = json.loads((driver_output / "stdio.json").read_bytes())
    raw = (json.dumps(stdio, sort_keys=True) + "\n").encode()
    check_record(observed, app_output, raw, require_capture=False)
    captures = [
        json.loads(line) for line in (app_output / "native-captures.jsonl").read_text().splitlines()
    ]
    witness = runpy.run_path(str(recorder))["native_capture_witness"](
        bytes.fromhex(stdio["captureHex"]), captures
    )
    (output / "application-stdio.json").write_text(json.dumps(stdio, indent=2) + "\n")
    if (
        subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        != revision
    ):
        raise ValueError("source revision changed while the archive proof was running")
    dirty |= bool(
        subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT)
    )
    if dirty:
        raise ValueError("source files changed while the archive proof was running")
    report = {
        "schemaVersion": 1,
        "sourceRevision": revision,
        "sourceDirty": dirty,
        "archives": archives,
        "recorder": recorder_provenance,
        "consumers": consumers,
        "mixed": mixed,
        "guideExamples": guides,
        "applications": {
            "installed": apps,
            "stdio": stdio,
            "paneByteWitness": witness,
            "cleanup": observed["cleanup"],
            "fixtureProgramSha256": hashlib.sha256(FIXTURE_PROGRAM.encode()).hexdigest(),
            "driverSha256": sha256(Path(__file__).resolve()),
            "profile": "json-stdio-and-separate-native-capture",
        },
        "passed": True,
    }
    (output / "proof.json").write_text(json.dumps(report, indent=2) + "\n")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--recorder", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--drive-applications", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--installed-bin", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--fixture-binary", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--drive-reader", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--reader-binary", type=Path, help=argparse.SUPPRESS)
    parser.add_argument(
        "--reader-case",
        default="success",
        choices=[
            "success",
            "name-conflict",
            "creation-followup",
            "read-failure",
            "read-and-cleanup-failure",
        ],
        help=argparse.SUPPRESS,
    )
    args = parser.parse_args(argv)
    if args.drive_reader:
        if args.reader_binary is None or args.fixture_binary is None:
            parser.error("the reader driver requires its binary and external fixture")
        drive_reader(
            args.reader_binary.resolve(),
            args.fixture_binary.resolve(),
            args.output_dir.resolve(),
            args.reader_case,
        )
    elif args.drive_applications:
        if args.installed_bin is None or args.fixture_binary is None:
            parser.error("the app driver requires installed binaries and its fixture")
        drive_applications(
            args.installed_bin.resolve(), args.fixture_binary.resolve(), args.output_dir.resolve()
        )
    else:
        if args.recorder is None:
            parser.error("--recorder must name the pinned libtmux/docs bare recorder")
        with tempfile.TemporaryDirectory(prefix="libtmux-readers-") as temporary:
            check_consumers(Path(temporary), args.recorder.resolve(), args.output_dir.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
