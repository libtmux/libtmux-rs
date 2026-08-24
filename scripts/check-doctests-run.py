#!/usr/bin/env python3
"""Fail when a doctest compiles but executes nothing.

rustdoc wraps a doctest's body in `fn main`, so a block whose whole body is a
hidden function definition compiles that function and then runs an empty main.
Every assertion inside it is dead. Nothing says so: the block has no marker
distinguishing it from one that runs, `cargo test --doc` counts it among its
passes, and a reader sees the same rendered example either way.

This is the defect `just examples` exists for, one level down. There, an
example was compiled by `--all-targets` and never run; here, a doctest is
compiled by rustdoc and never run. Both report success while covering nothing.

`no_run`, `ignore` and `compile_fail` are exempt: each declares what it is.
The offence is a block that reads as executable and is not.

A doctest is not required to *do* anything -- a block that only defines a type
to prove a derive expands is a compile test, and compiling is the point. What
fails here is narrower: a block that defines functions, calls none of them,
and has no statement of its own.
"""

from __future__ import annotations

import pathlib
import re
import sys

# A fence whose language is not Rust, or which says it will not run.
NOT_RUST = {"console", "text", "toml", "yaml", "json", "sh", "bash", "diff", "md"}
DECLARED = ("no_run", "ignore", "compile_fail")

RUST_FENCE = re.compile(r"^\s*(///|//!)\s*```(.*)$")
RUST_STRIP = re.compile(r"^\s*(///|//!)\s?")
INCLUDED = re.compile(r'include_str!\("([^"]+)"\)')

# A line that only declares something. Anything else is a statement, which
# means main does something and the block runs.
DECLARATION = re.compile(r"^(use |fn |async fn |pub |struct |enum |impl |trait |mod |type |const |static |\}|\{|#\[)")

# rustdoc's hidden-line marker, and string bodies, which would otherwise put
# stray braces into the depth count.
HIDDEN = re.compile(r"^\s*#\s?")
STRINGS = re.compile(r'"(?:[^"\\]|\\.)*"')


def rust_blocks(path: pathlib.Path):
    """Yield `(line, attribute, body)` for each fenced block in `///` or `//!`."""
    lines = path.read_text(errors="replace").splitlines()
    index = 0
    while index < len(lines):
        opened = RUST_FENCE.match(lines[index])
        if not opened:
            index += 1
            continue
        attribute, start, body = opened.group(2).strip(), index + 1, []
        index += 1
        while index < len(lines):
            closed = RUST_FENCE.match(lines[index])
            if closed and not closed.group(2).strip():
                break
            body.append(RUST_STRIP.sub("", lines[index]))
            index += 1
        index += 1
        yield start, attribute, "\n".join(body)


def markdown_blocks(path: pathlib.Path):
    """Yield `(line, attribute, body)` for each fenced block in a markdown file."""
    lines = path.read_text(errors="replace").splitlines()
    index = 0
    while index < len(lines):
        if not lines[index].startswith("```"):
            index += 1
            continue
        attribute, start, body = lines[index][3:].strip(), index + 1, []
        index += 1
        while index < len(lines) and not lines[index].startswith("```"):
            body.append(lines[index])
            index += 1
        index += 1
        yield start, attribute, "\n".join(body)


def included_markdown(roots: list[str]) -> set[pathlib.Path]:
    """Every markdown file rustdoc pulls in, found rather than listed.

    Naming them would leave a new one uncovered and silent, which is the shape
    of the bug this script is here to stop.
    """
    found = set()
    for root in roots:
        for source in pathlib.Path(root).rglob("*.rs"):
            for match in INCLUDED.finditer(source.read_text(errors="replace")):
                target = (source.parent / match.group(1)).resolve()
                if target.suffix == ".md" and target.exists():
                    found.add(target)
    return found


def never_runs(attribute: str, body: str) -> bool:
    """Whether this block compiles a definition and then executes nothing."""
    if attribute.split(",")[0].strip() in NOT_RUST:
        return False
    if any(word in attribute for word in DECLARED):
        return False

    defined = re.findall(r"\b(?:async\s+)?fn (\w+)", body)
    if not defined:
        return False

    # rustdoc only supplies a `main` when the block does not define one, so a
    # block that writes its own is the entry point and runs. That is the whole
    # difference between the two shapes: `fn main` is called by rustdoc, and
    # `async fn example` is called by nobody.
    if "main" in defined:
        return False

    for name in defined:
        without = re.sub(r"(?:async\s+)?fn " + name, "", body)
        if re.search(r"\b" + name + r"\s*\(", without):
            return False

    if any(word in body for word in ("tokio::runtime", "#[tokio::main]", "block_on", "tokio_test")):
        return False

    # Whether main does anything is a question about nesting, not about lines.
    # The statements that matter sit at depth zero; the ones inside the hidden
    # function are the very thing that never runs, and reading them as proof
    # that it does is how this check first passed a block it should have
    # failed.
    depth = 0
    for line in body.splitlines():
        text = HIDDEN.sub("", line).strip()
        if not text or text.startswith("//"):
            continue
        bare = STRINGS.sub("", text)
        if depth == 0 and not DECLARATION.match(text):
            return False
        depth += bare.count("{") - bare.count("}")
    return True


def main(roots: list[str]) -> int:
    found = []
    sources = [(path, rust_blocks) for root in roots for path in sorted(pathlib.Path(root).rglob("*.rs"))]
    sources += [(path, markdown_blocks) for path in sorted(included_markdown(roots))]

    for path, reader in sources:
        if "/target/" in str(path):
            continue
        for line, attribute, body in reader(path):
            if never_runs(attribute, body):
                try:
                    shown = path.relative_to(pathlib.Path.cwd())
                except ValueError:
                    shown = path
                found.append(f"{shown}:{line}")

    if not found:
        print(f"every doctest runs ({len(sources)} files scanned)")
        return 0

    for line in found:
        print(line, file=sys.stderr)
    print(
        f"\n{len(found)} doctest(s) compile a definition and run nothing. rustdoc "
        "puts the body in `fn main`, so a block that only defines a hidden "
        "function never reaches its own assertions.\n\n"
        "Give it a runtime and call the work -- `libtmux::test::TestServer` and "
        "a `block_on`, as the executing doctests do -- or mark it `no_run` if it "
        "genuinely cannot run here, which says so to the next reader.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:] or ["crates"]))
