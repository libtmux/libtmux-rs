#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Read a crate release out of its tag, and attach the packaged crate to the release.

    release_crate.py target TAG
    release_crate.py attach TAG PACKAGE VERSION CRATE_FILE

A release tag names one crate, `<crate>@v<version>`. `target` splits the tag at
its last `@v`, refuses it unless the workspace has a member of that name with
that version, and appends `package`, `version` and `crate_file`
(`target/package/<package>-<version>.crate`) to $GITHUB_OUTPUT. A tag is an
assertion about the tree, and a tag naming a version the manifest does not
carry would otherwise publish whatever the manifest says.

`attach` uploads the crate to the GitHub release of the tag, and creates the
release first when it does not exist. It is idempotent, because a dispatch
exists to retry a publish that failed partway, and a release made by the first
attempt must not stop the second. A version with a `-` suffix is a semver
prerelease; a final version gets an ordinary release. It needs `gh`, GH_TOKEN
and GITHUB_REPOSITORY.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass

Run = Callable[..., subprocess.CompletedProcess[str]]


class ReleaseError(Exception):
    """The tag or the release cannot be used."""


@dataclass(frozen=True)
class Target:
    package: str
    version: str

    @property
    def crate_file(self) -> str:
        return f"target/package/{self.package}-{self.version}.crate"


def parse_tag(tag: str) -> Target:
    """Split `<crate>@v<version>` at its last `@v`.

    >>> parse_tag("libtmux@v0.1.0")
    Target(package='libtmux', version='0.1.0')
    """
    package, separator, version = tag.rpartition("@v")
    if not separator:
        raise ReleaseError(f"tag {tag} is not <crate>@v<version>")
    return Target(package, version)


def check_manifest(target: Target, metadata: str) -> None:
    """Refuse a tag whose crate is not a workspace member at the tagged version."""
    versions = [
        member["version"]
        for member in json.loads(metadata)["packages"]
        if member["name"] == target.package
    ]
    if not versions:
        raise ReleaseError(f"no workspace member is called {target.package}")
    if versions[0] != target.version:
        raise ReleaseError(
            f"{target.package} is {versions[0]} in the manifest, but the tag says {target.version}"
        )


def release_commands(
    tag: str, target: Target, crate_file: str, repository: str, exists: bool
) -> list[str]:
    """The `gh release` command that attaches the crate, as an argument list."""
    if exists:
        return ["gh", "release", "upload", tag, crate_file, "--repo", repository, "--clobber"]
    notes = f"`{target.package}` {target.version}. See crates/{target.package}/CHANGELOG.md for what changed."
    flags: list[str] = []
    if "-" in target.version:
        notes += " This is a prerelease."
        flags.append("--prerelease")
    return [
        "gh",
        "release",
        "create",
        tag,
        "--repo",
        repository,
        "--title",
        f"{target.package} {target.version}",
        "--notes",
        notes,
        *flags,
        "--verify-tag",
        crate_file,
    ]


def resolve(tag: str, run: Run) -> Target:
    target = parse_tag(tag)
    result = run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise ReleaseError(f"cargo metadata failed: {result.stderr.strip()}")
    check_manifest(target, result.stdout)
    return target


def attach(args: Sequence[str], environ: Mapping[str, str], run: Run) -> int:
    tag, package, version, crate_file = args
    repository = environ.get("GITHUB_REPOSITORY", "")
    if not repository:
        raise ReleaseError("GITHUB_REPOSITORY is not set")
    exists = (
        run(
            ["gh", "release", "view", tag, "--repo", repository],
            capture_output=True,
            text=True,
            check=False,
        ).returncode
        == 0
    )
    command = release_commands(tag, Target(package, version), crate_file, repository, exists)
    return run(command, check=False).returncode


def main(argv: Sequence[str], environ: Mapping[str, str], run: Run = subprocess.run) -> int:
    try:
        if len(argv) == 2 and argv[0] == "target":
            target = resolve(argv[1], run)
            destination = environ.get("GITHUB_OUTPUT")
            lines = f"package={target.package}\nversion={target.version}\ncrate_file={target.crate_file}\n"
            if destination:
                with open(destination, "a", encoding="utf-8") as output:
                    output.write(lines)
            sys.stdout.write(lines)
            return 0
        if len(argv) == 5 and argv[0] == "attach":
            return attach(argv[1:], environ, run)
    except ReleaseError as error:
        sys.stderr.write(f"::error::release-crate: {error}\n")
        return 1
    sys.stderr.write(
        "usage: release_crate.py target TAG\n"
        "       release_crate.py attach TAG PACKAGE VERSION CRATE_FILE\n"
    )
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], os.environ))
