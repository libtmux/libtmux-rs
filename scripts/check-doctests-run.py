#!/usr/bin/env python3
"""Fail when a doctest does not run the example it shows.

rustdoc supplies a doctest's `fn main` only when the block does not define
one, so a body that is nothing but a hidden `async fn example` compiles that
function and then runs an empty main. Every assertion inside it is dead, and
nothing says so: it renders like any other example and `cargo test --doc`
counts it among its passes.

Asking which shapes fail to run is the wrong question, and the first version
of this script asked it. A rule that lists the ways a body can be built and
never driven -- an uncalled function, an async block nobody awaits, a closure
bound and never called -- is a blacklist, and a blacklist has a next hole. It
had four, and two of them were the shapes its own error message pushed a
reader towards after telling them not to use the third.

So this asks the opposite question: is the line the reader sees reached? A
scope entered only through something never used is not reached, whatever that
something is, and a construct nobody here has thought of defaults to
unreached rather than to fine.

Two ways a block fails. Some line a reader sees is never reached, or there is
nothing to see at all -- a body hidden in its entirety renders as an empty
example and proves nothing to anyone.

`no_run`, `ignore` and `compile_fail` are exempt. Each declares what it is;
the offence is a block that reads as executable and is not.
"""

from __future__ import annotations

import pathlib
import re
import sys

NOT_RUST = {"console", "text", "toml", "yaml", "json", "sh", "bash", "diff", "md"}
DECLARED = ("no_run", "ignore", "compile_fail")

RUST_FENCE = re.compile(r"^\s*(///|//!)\s*```(.*)$")
RUST_STRIP = re.compile(r"^\s*(///|//!)\s?")
INCLUDED = re.compile(r'include_str!\("([^"]+)"\)')

COMMENT = re.compile(r"//.*$")
STRING = re.compile(r'"(?:[^"\\]|\\.)*"')
CHARLIT = re.compile(r"'(?:[^'\\]|\\.)'")
FN_DEF = re.compile(r"\b(?:pub\s+)?(?:async\s+)?fn\s+(\w+)")
MEMBER_OF = re.compile(r"^(pub\s+)?(unsafe\s+)?(impl|trait)\b")
# A body bound to a name runs only if something later uses the name.
BOUND = re.compile(
    r"\blet\s+(?:mut\s+)?(\w+)\s*(?::[^=]+)?=\s*"
    r"(?:async\s*(?:move\s*)?\{|(?:async\s*)?(?:move\s*)?\|)"
)
# One handed straight to something else is presumed driven by it.
AS_ARGUMENT = re.compile(r"[(,]\s*(?:async\s*(?:move\s*)?\{|(?:async\s*)?(?:move\s*)?\|[^|]*\|)")

NOISE = {"{", "}", "};", ");", "});", "})?;", ")?;"}


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

    Listing them would leave the next one uncovered and silent, which is how
    the examples gate came to miss a whole crate.
    """
    found = set()
    for root in roots:
        for source in pathlib.Path(root).rglob("*.rs"):
            for match in INCLUDED.finditer(source.read_text(errors="replace")):
                target = (source.parent / match.group(1)).resolve()
                if target.suffix == ".md" and target.exists():
                    found.add(target)
    return found


def _clean(line: str) -> str:
    """Strip what must not contribute braces or identifiers."""
    return COMMENT.sub("", CHARLIT.sub("''", STRING.sub('""', line)))


def _text(line: str) -> str:
    """A line as the compiler sees it, hidden or not."""
    stripped = line.strip()
    return stripped[1:].strip() if stripped.startswith("#") else stripped


def reached(body: str) -> tuple[int, int]:
    """Return how many of the lines a reader sees are reached, and how many there are."""
    raw = body.splitlines()
    lines = [_clean(line) for line in raw]

    # Which scope openings are gated, and on what name.
    gate_of: dict[int, str] = {}
    member = depth = 0
    for index, line in enumerate(lines):
        text = _text(line)
        opening = text.count("{")
        if MEMBER_OF.match(text) and opening:
            member = member or depth + 1
        definition = FN_DEF.search(text)
        bound = BOUND.search(text)
        if definition and opening:
            # A method reaches its caller through the trait rather than by
            # name, so gating one on its name appearing marks every trait impl
            # in a doctest dead. Implementing the trait is what such a block
            # demonstrates, and compiling it is what checks it.
            if not member:
                gate_of[index] = definition.group(1)
        elif bound and not AS_ARGUMENT.search(text):
            gate_of[index] = bound.group(1)
        depth += opening - text.count("}")
        if member and depth < member:
            member = 0

    def walk(used: set[str]) -> list[bool]:
        stack = [True]
        marks = []
        for index, line in enumerate(lines):
            text = _text(line)
            marks.append(stack[-1])
            opening = text.count("{")
            if opening:
                gate = gate_of.get(index)
                # rustdoc calls `main`; anything else needs a user.
                entered = stack[-1] and (gate is None or gate == "main" or gate in used)
                stack.extend([entered] * opening)
            for _ in range(min(text.count("}"), len(stack) - 1)):
                stack.pop()
        return marks

    # A name counts as used only where the use itself is reached, so a call
    # made from code that never runs does not revive what it calls.
    names = set(gate_of.values())
    used: set[str] = set()
    for _ in range(len(names) + 2):
        marks = walk(used)
        found = {
            name
            for name in names
            for index, line in enumerate(lines)
            if marks[index]
            and gate_of.get(index) != name
            and re.search(r"\b" + re.escape(name) + r"\b", line)
        }
        if found == used:
            break
        used = found

    marks = walk(used)
    seen = total = 0
    for index, line in enumerate(raw):
        if line.strip().startswith("#"):
            continue
        text = _clean(line).strip()
        if not text or text in NOISE:
            continue
        total += 1
        seen += marks[index]
    return seen, total


def main(roots: list[str]) -> int:
    unreached: list[str] = []
    invisible: list[str] = []

    sources = [(path, rust_blocks) for root in roots for path in sorted(pathlib.Path(root).rglob("*.rs"))]
    sources += [(path, markdown_blocks) for path in sorted(included_markdown(roots))]

    for path, reader in sources:
        if "/target/" in str(path):
            continue
        for line, attribute, body in reader(path):
            if attribute.split(",")[0].strip() in NOT_RUST:
                continue
            if any(word in attribute for word in DECLARED):
                continue
            seen, total = reached(body)
            try:
                shown = path.relative_to(pathlib.Path.cwd())
            except ValueError:
                shown = path
            if not total:
                invisible.append(f"{shown}:{line}")
            elif seen < total:
                unreached.append(f"{shown}:{line}  ({total - seen} of {total} lines never reached)")

    if not unreached and not invisible:
        print(f"every doctest runs what it shows ({len(sources)} files scanned)")
        return 0

    for line in unreached + invisible:
        print(line, file=sys.stderr)

    if unreached:
        print(
            f"\n{len(unreached)} doctest(s) show a reader lines that never run. The "
            "scope holding them is entered only through something nothing uses -- a "
            "function nobody calls, an async block nobody awaits, a closure nobody "
            "invokes -- so rustdoc compiles the body and the assertions inside it "
            "never happen.\n\n"
            "Call the work from the top level, or from a `fn main` the block defines. "
            "Mark it `no_run` if it genuinely cannot run here, which says so to the "
            "next reader instead of leaving them to find out.",
            file=sys.stderr,
        )
    if invisible:
        print(
            f"\n{len(invisible)} doctest(s) hide their whole body, so the example "
            "renders empty. Whatever it proves, it proves it to nobody: leave at "
            "least the lines a reader came for visible.",
            file=sys.stderr,
        )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:] or ["crates"]))
