#!/usr/bin/env python3
"""Compare the format catalog against tmux's own `format_table[]`.

The catalog is hand-maintained, so a format tmux gained -- or one nobody ever
added -- is invisible: nothing fails, callers just cannot ask for the field.
This records every name tmux publishes and what this crate does about it, the
way `public-api.txt` records the public surface.

`--check` reads the recorded ledger and the catalog and fails when they
disagree. Regenerating needs a tmux checkout; checking does not.
"""

from __future__ import annotations

import pathlib
import re
import sys

CATALOG = pathlib.Path("crates/libtmux/src/formats.rs")
LEDGER = pathlib.Path("crates/libtmux/docs/format-coverage.txt")

# What a callback reads decides whether a listing can ever answer it.
SCOPES = {
    "ft->m": "mouse",
    "ft->pb": "buffer",
    "ft->wp": "pane",
    "ft->wl": "winlink",
    "ft->w": "window",
    "ft->s": "session",
    "ft->c": "client",
}

REVIEWED_EXCLUSIONS = {
    "loop_last_flag": "excluded: only set inside a #{L:} format loop",
}


def catalogued() -> set[str]:
    """Format names this crate can request."""
    text = CATALOG.read_text()
    return set(re.findall(r'\(\s*[A-Z_0-9]+,\s*[a-z_0-9]+,\s*"([a-z_0-9]+)"', text))


def published(source: pathlib.Path) -> dict[str, str]:
    """Format names tmux publishes, mapped to the scope each one needs.

    Two registrations, because tmux has two. `format_table[]` is the static
    set every listing can answer; `format_add` and `cmdq_add_format` attach a
    name to one command's or one mode's tree, so those only exist in that
    context and no listing carries them.
    """
    text = (source / "format.c").read_text()

    scopes = {}
    for name in sorted(set(re.findall(r'\{ "([a-z_0-9]+)", ', text))):
        body = re.search(rf"format_cb_{name}\(struct format_tree \*ft.*?\n\}}", text, re.S)
        if not body:
            scopes[name] = "unknown"
            continue
        needed = [label for member, label in SCOPES.items() if member in body.group(0)]
        scopes[name] = needed[0] if needed else "global"

    for path in sorted(source.glob("*.c")):
        body = path.read_text()
        for name in re.findall(r'(?:format_add|format_add_cb|cmdq_add_format)\([a-z_>.-]+, "([a-z_0-9]+)"', body):
            scopes.setdefault(name, f"context: {path.stem}")

    return scopes


def read_ledger() -> dict[str, str]:
    entries = {}
    for line in LEDGER.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        name, _, status = line.partition(" ")
        entries[name] = status.strip()
    return entries


def classify(name: str, scope: str, have: set[str]) -> str:
    """Classify one upstream format against the listing catalog."""
    if name in have:
        return "catalogued"
    if name in REVIEWED_EXCLUSIONS:
        return REVIEWED_EXCLUSIONS[name]
    if scope.startswith("context: "):
        return f"excluded: only inside {scope.removeprefix('context: ')}"
    if scope == "mouse":
        return "excluded: needs a mouse event"
    if scope == "buffer":
        return "excluded: needs a paste buffer, which is not a hierarchy row"
    if scope == "unknown":
        return "excluded: format-internal"
    return f"missing: {scope}"


def expected_ledger(source: pathlib.Path) -> dict[str, str]:
    """Build the ledger implied by one tmux source tree."""
    have = catalogued()
    return {
        name: classify(name, scope, have)
        for name, scope in published(source).items()
    }


def check() -> int:
    have = catalogued()
    ledger = read_ledger()

    problems = []
    for name in sorted(have - set(ledger)):
        problems.append(f"{name}: in the catalog, absent from the ledger")
    for name, status in sorted(ledger.items()):
        if status == "catalogued" and name not in have:
            problems.append(f"{name}: recorded as catalogued, absent from the catalog")
        if status.startswith("missing") and name in have:
            problems.append(f"{name}: recorded as missing, now in the catalog")

    if problems:
        for line in problems:
            print(line, file=sys.stderr)
        print(
            f"\n{len(problems)} disagreement(s). Rerun "
            "`just format-coverage <tmux source>` and commit the result.",
            file=sys.stderr,
        )
        return 1

    counts: dict[str, int] = {}
    for status in ledger.values():
        counts[status.split(":")[0]] = counts.get(status.split(":")[0], 0) + 1
    summary = ", ".join(f"{count} {status}" for status, count in sorted(counts.items()))
    print(f"format coverage unchanged: {summary}")
    return 0


def check_source(source: pathlib.Path) -> int:
    """Compare the checked ledger with one pinned tmux source tree."""
    recorded = read_ledger()
    expected = expected_ledger(source)
    problems = []
    for name in sorted(set(recorded) | set(expected)):
        if name not in recorded:
            problems.append(f"{name}: published by tmux, absent from the ledger")
        elif name not in expected:
            problems.append(f"{name}: recorded in the ledger, absent from tmux")
        elif recorded[name] != expected[name]:
            problems.append(
                f"{name}: recorded as {recorded[name]!r}, expected {expected[name]!r}"
            )

    if problems:
        for line in problems:
            print(line, file=sys.stderr)
        print(
            f"\n{len(problems)} upstream disagreement(s). Rerun "
            "`just format-coverage <tmux source>` and commit the result.",
            file=sys.stderr,
        )
        return 1

    print(f"format coverage matches {len(expected)} upstream formats")
    return 0


def generate(source: pathlib.Path) -> int:
    expected = expected_ledger(source)

    lines = [
        "# Every format name tmux publishes, and what this crate does about it.",
        "#",
        "# Regenerate with `just format-coverage <path to a tmux checkout>`.",
        "# A `missing` entry is a field a listing could carry and does not; an",
        "# `excluded` entry names why no listing can ever answer it.",
        "#",
        "# `catalogued` says the crate can decode the field, not that any listing asks",
        "# for it. For which names each listing actually requests, see",
        "# `list-profiles.txt`.",
        "",
    ]
    for name in sorted(expected):
        lines.append(f"{name} {expected[name]}")

    LEDGER.write_text("\n".join(lines) + "\n")
    print(f"recorded {len(expected)} formats")
    return 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["--check"]:
        raise SystemExit(check())
    if sys.argv[1:2] == ["--check-source"] and len(sys.argv) == 3:
        raise SystemExit(check_source(pathlib.Path(sys.argv[2])))
    if len(sys.argv) != 2:
        print(
            "usage: format-coverage.py (--check | --check-source <tmux source> | <tmux source>)",
            file=sys.stderr,
        )
        raise SystemExit(2)
    raise SystemExit(generate(pathlib.Path(sys.argv[1])))
