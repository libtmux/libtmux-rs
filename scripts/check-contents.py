#!/usr/bin/env python3
"""Fail when a document's Contents list and its sections disagree.

A long document opens with a `## Contents` list so a reader can find a section
without reading down to it. A list is a copy of the headings, and a copy
drifts: a section is added or renamed, the list keeps rendering, and the reader
is sent to an anchor that no longer exists. This compares the two, in order,
and checks each link against the anchor GitHub gives its heading.
"""

from __future__ import annotations

import pathlib
import re
import sys

ENTRY = re.compile(r"^(?P<indent>(  )?)- \[(?P<title>[^\]]+)\]\(#(?P<anchor>[^)]+)\)$")


def anchor(title: str) -> str:
    """The id GitHub gives a heading: lowercase, punctuation dropped, spaces to hyphens."""
    text = title.strip().lower().replace("`", "")
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


def check(path: pathlib.Path) -> list[str]:
    """Compare the Contents list with the sections it names.

    A top-level entry names a `##` section, in order. Entries indented under
    one name that section's `###` subsections, in order, when a section lists
    them at all: a long section can carry its own index without every short
    one having to.
    """
    lines = path.read_text(encoding="utf-8").splitlines()
    sections: list[tuple[str, list[str]]] = []
    listed: list[tuple[str, str, list[tuple[str, str]]]] = []
    fenced = False
    in_contents = False
    for line in lines:
        if line.startswith("```"):
            fenced = not fenced
            continue
        if fenced:
            continue
        if line.startswith("## "):
            title = line[3:].strip()
            in_contents = title == "Contents"
            if not in_contents:
                sections.append((title, []))
            continue
        if line.startswith("### ") and sections:
            sections[-1][1].append(line[4:].strip())
            continue
        if in_contents and line.lstrip().startswith("- "):
            match = ENTRY.match(line)
            if match is None:
                return [f"{path}: a Contents entry is not `- [Title](#anchor)`: {line}"]
            if match["indent"]:
                if not listed:
                    return [f"{path}: a nested Contents entry has no section above it: {line}"]
                listed[-1][2].append((match["title"], match["anchor"]))
            else:
                listed.append((match["title"], match["anchor"], []))
    if not listed:
        return [f"{path}: no `## Contents` list"]
    problems = []
    titles = [title for title, _, _ in listed]
    headings = [title for title, _ in sections]
    if titles != headings:
        missing = [title for title in headings if title not in titles]
        stale = [title for title in titles if title not in headings]
        problems.append(
            f"{path}: Contents does not match the sections in order;"
            f" missing {missing}, stale {stale}"
        )
    subsections = dict(sections)
    entries = [(title, target) for title, target, _ in listed]
    for title, _, nested in listed:
        entries += nested
        if nested and [sub for sub, _ in nested] != subsections.get(title, []):
            problems.append(
                f"{path}: Contents under [{title}] does not match its ### subsections in order;"
                f" listed {[sub for sub, _ in nested]}, found {subsections.get(title, [])}"
            )
    problems += [
        f"{path}: [{title}] links #{target}, but the section's anchor is #{anchor(title)}"
        for title, target in entries
        if target != anchor(title)
    ]
    return problems


def main(paths: list[str]) -> int:
    problems = [problem for path in paths for problem in check(pathlib.Path(path))]
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
