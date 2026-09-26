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

ENTRY = re.compile(r"^- \[(?P<title>[^\]]+)\]\(#(?P<anchor>[^)]+)\)$")


def anchor(title: str) -> str:
    """The id GitHub gives a heading: lowercase, punctuation dropped, spaces to hyphens."""
    text = title.strip().lower().replace("`", "")
    text = re.sub(r"[^\w\- ]", "", text)
    return text.replace(" ", "-")


def check(path: pathlib.Path) -> list[str]:
    lines = path.read_text(encoding="utf-8").splitlines()
    headings: list[str] = []
    listed: list[tuple[str, str]] = []
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
                headings.append(title)
            continue
        if in_contents and line.startswith("- "):
            match = ENTRY.match(line)
            if match is None:
                return [f"{path}: a Contents entry is not `- [Title](#anchor)`: {line}"]
            listed.append((match["title"], match["anchor"]))
    if not listed:
        return [f"{path}: no `## Contents` list"]
    problems = []
    titles = [title for title, _ in listed]
    if titles != headings:
        missing = [title for title in headings if title not in titles]
        stale = [title for title in titles if title not in headings]
        problems.append(
            f"{path}: Contents does not match the sections in order;"
            f" missing {missing}, stale {stale}"
        )
    problems += [
        f"{path}: [{title}] links #{target}, but the section's anchor is #{anchor(title)}"
        for title, target in listed
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
