#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Package a built command for a GitHub release, in the layout `cargo binstall` fetches.

    package_binary.py PACKAGE TARGET [VERSION]

Run it from the workspace root after `cargo build --release --target TARGET`. It
writes `<package>-<version>-<target>.tar.gz`, which holds the directory
`<package>-<version>-<target>/` with the command, the crate's README and its
two licences, and appends `name=<package>-<version>-<target>` to $GITHUB_OUTPUT.
An empty VERSION reads the crate's version from `cargo metadata`.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
from collections.abc import Callable, Mapping, Sequence

DOCUMENTS = ("README.md", "LICENSE-APACHE", "LICENSE-MIT")


class PackageError(Exception):
    """The command cannot be packaged."""


def crate_version(metadata: str, package: str) -> str:
    """The version of the workspace member called `package`.

    >>> crate_version('{"packages": [{"name": "a", "version": "1.2.3"}]}', "a")
    '1.2.3'
    """
    for member in json.loads(metadata)["packages"]:
        if member["name"] == package:
            return str(member["version"])
    raise PackageError(f"no workspace member is called {package}")


def read_version(package: str) -> str:
    """Ask cargo for the version of `package`."""
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise PackageError(f"cargo metadata failed: {result.stderr.strip()}")
    return crate_version(result.stdout, package)


def build_archive(package: str, target: str, version: str, root: pathlib.Path) -> str:
    """Lay out and compress the archive under `root`, and return its name without `.tar.gz`."""
    name = f"{package}-{version}-{target}"
    command = root / "target" / target / "release" / package
    crate = root / "crates" / package
    directory = root / name
    try:
        directory.mkdir()
        shutil.copy(command, directory)
        for document in DOCUMENTS:
            shutil.copy(crate / document, directory)
    except OSError as error:
        raise PackageError(f"cannot lay out {name}: {error}") from error
    result = subprocess.run(
        ["tar", "-czf", f"{name}.tar.gz", name],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise PackageError(f"tar failed: {result.stderr.strip()}")
    return name


def main(
    argv: Sequence[str],
    environ: Mapping[str, str],
    version_of: Callable[[str], str] = read_version,
    root: pathlib.Path | None = None,
) -> int:
    if len(argv) not in (2, 3):
        sys.stderr.write("usage: package_binary.py PACKAGE TARGET [VERSION]\n")
        return 2
    package, target = argv[0], argv[1]
    try:
        version = (argv[2] if len(argv) == 3 else "") or version_of(package)
        name = build_archive(package, target, version, root or pathlib.Path.cwd())
    except PackageError as error:
        sys.stderr.write(f"::error::package-binary: {error}\n")
        return 1
    destination = environ.get("GITHUB_OUTPUT")
    if destination:
        with open(destination, "a", encoding="utf-8") as output:
            output.write(f"name={name}\n")
    sys.stdout.write(f"{name}.tar.gz\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], os.environ))
