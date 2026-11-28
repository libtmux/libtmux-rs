#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# ///
"""Run external Cargo consumers against the Rust archives prepared for release."""

from __future__ import annotations

import json
import os
import subprocess
import tarfile
import tempfile
import tomllib
from pathlib import Path

LIBRARIES = ("libtmux-core", "libtmux-blocking", "libtmux-tokio", "libtmux-macros", "libtmux")
ROOT = Path(__file__).resolve().parents[2]


def check_consumers(directory: Path) -> None:
    """Check dependency identity and execute a program in each feature configuration."""
    env = {**os.environ, "CARGO_TARGET_DIR": str(directory / "target")}

    def cargo(cwd: Path, *arguments: str) -> bytes:
        return subprocess.run(
            ["cargo", *arguments], cwd=cwd, env=env, stdout=subprocess.PIPE, check=True
        ).stdout

    packages = [item for name in LIBRARIES for item in ("-p", name)]
    cargo(ROOT, "package", "--locked", "--allow-dirty", "--no-verify", *packages)
    archives = directory / "target" / "package"
    dependencies: dict[str, Path] = {}
    for name in LIBRARIES:
        [archive] = [p for p in archives.glob(f"{name}-*.crate") if p.name[len(name) + 1].isdigit()]
        with tarfile.open(archive) as tar:
            tar.extractall(directory, filter="data")
        dependencies[name] = directory / archive.stem

    versions = {}
    for name, path in dependencies.items():
        manifest = tomllib.loads((path / "Cargo.toml").read_text())
        versions[name] = manifest["package"]["version"]
    for path in dependencies.values():
        manifest = tomllib.loads((path / "Cargo.toml").read_text())
        for field in ("dependencies", "dev-dependencies", "build-dependencies"):
            for alias, specification in manifest.get(field, {}).items():
                package = specification.get("package", alias)
                if package in versions and specification["version"] != f"={versions[package]}":
                    raise RuntimeError(f"{path.name} does not pin {package} exactly")

    patches = "\n".join(f'{name} = {{ path = "{path}" }}' for name, path in dependencies.items())
    facade = dependencies["libtmux"]
    mixed = tomllib.loads((facade / "examples/api/consumer.toml").read_text())["dependencies"]
    extras = "\n".join(
        f'{name} = {{ version = "{spec["version"]}", features = {json.dumps(spec["features"])} }}'
        for name, spec in mixed.items()
        if name != "libtmux"
    )
    configurations = [
        ("core", "libtmux-core", [], 'let _ = libtmux_core::NewSession::new("work");'),
        (
            "blocking",
            "libtmux",
            [],
            'let _ = libtmux::blocking::Server::at("/example/socket").unwrap();',
        ),
        ("tokio", "libtmux", ["tokio"], "let _ = libtmux::tokio::Server::builder();"),
        ("mixed", "libtmux", mixed["libtmux"]["features"], ""),
    ]
    for lane, name, features, body in configurations:
        project = directory / f"reader-{lane}"
        (project / "src").mkdir(parents=True)
        defaults = "true" if lane == "blocking" else "false"
        (project / "Cargo.toml").write_text(
            f'[package]\nname = "reader-{lane}"\nversion = "0.0.0"\nedition = "2024"\n'
            f'[dependencies]\n{name} = {{ version = "={versions[name]}", default-features = {defaults}, '
            f"features = {json.dumps(features)} }}\n"
            f"{extras if lane == 'mixed' else ''}\n[patch.crates-io]\n{patches}\n"
        )
        source = (
            (facade / "examples/api_mixed.rs").read_text()
            if lane == "mixed"
            else f"fn main() {{ {body} }}\n"
        )
        (project / "src/main.rs").write_text(source)
        tree = set(
            cargo(project, "tree", "-e", "normal", "--prefix", "none", "--format", "{p}")
            .decode()
            .splitlines()
        )
        selected = {line.removesuffix(" (*)") for line in tree if line.startswith("libtmux-core v")}
        if len(selected) != 1 or str(dependencies["libtmux-core"]) not in next(iter(selected)):
            raise RuntimeError(f"{lane} did not resolve one staged core: {selected}")
        has_tokio = any(line.startswith("tokio v") for line in tree)
        if has_tokio != (lane in {"tokio", "mixed"}):
            raise RuntimeError(f"unexpected Tokio dependency in {lane}: {has_tokio}")
        actual = cargo(project, "run", "--locked", "--quiet")
        if actual != (b"mixed runtime\n" if lane == "mixed" else b""):
            raise RuntimeError(f"unexpected stdout for {lane} ({len(actual)} bytes)")
        print(f"{lane} consumer passed")


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="libtmux-readers-") as temporary:
        check_consumers(Path(temporary))
