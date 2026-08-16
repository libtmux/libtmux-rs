#!/usr/bin/env python3
"""Print the public API surface of a crate, one item per line.

Built from rustdoc's JSON rather than a separate tool, so the only thing it
needs is the nightly the fuzz targets already require. The output is sorted and
stable enough to diff: a line appearing or disappearing is a change to what a
caller can reach.

It is not a semver oracle. It says what moved, which is what
`cargo-semver-checks` cannot say while every version is a prerelease and it
skips every lint.
"""

from __future__ import annotations

import json
import sys

# Item kinds that are part of the surface. A module is a path component rather
# than a thing to call, and an impl is reported through the items inside it.
KINDS = {
    "struct", "enum", "trait", "function", "type_alias", "constant",
    "macro", "proc_macro", "struct_field", "variant", "assoc_const",
    "assoc_type", "primitive", "union",
}


def parents(index: dict, paths: dict) -> dict:
    """Map each child item to the type that owns it.

    Methods, fields, and variants have no standalone path in rustdoc's output,
    so an unqualified `sessions` says nothing about which handle it belongs to
    and a move between types would not show in a diff.
    """
    owner: dict = {}

    def name_of(identifier) -> str | None:
        summary = paths.get(str(identifier)) or paths.get(identifier)
        if summary and summary.get("path"):
            return "::".join(summary["path"])
        item = index.get(str(identifier)) or index.get(identifier)
        return item.get("name") if item else None

    for item in index.values():
        inner = item.get("inner") or {}

        if "struct" in inner:
            here = name_of(item.get("id"))
            kind = inner["struct"].get("kind")
            # A unit struct reports its kind as a bare string rather than a
            # map, so it has no fields to attribute.
            plain = kind.get("plain") if isinstance(kind, dict) else None
            fields = (plain or {}).get("fields") or []
            for child in fields:
                owner[str(child)] = here

        if "enum" in inner:
            here = name_of(item.get("id"))
            for child in inner["enum"].get("variants") or []:
                owner[str(child)] = here

        if "trait" in inner:
            here = name_of(item.get("id"))
            for child in inner["trait"].get("items") or []:
                owner[str(child)] = here

        if "impl" in inner:
            target = inner["impl"].get("for") or {}
            resolved = target.get("resolved_path") or {}
            here = name_of(resolved.get("id")) or resolved.get("path")
            trait = inner["impl"].get("trait") or {}
            # A trait implementation adds no new surface: the trait already
            # declared it. Only inherent items are listed.
            if trait:
                continue
            for child in inner["impl"].get("items") or []:
                owner[str(child)] = here

    return owner


def main(path: str) -> int:
    with open(path, encoding="utf-8") as handle:
        doc = json.load(handle)

    index = doc["index"]
    paths = doc["paths"]
    owner = parents(index, paths)
    lines = set()

    for item in index.values():
        if item.get("visibility") not in ("public", "default"):
            continue
        inner = item.get("inner") or {}
        kind = next(iter(inner), None)
        if kind not in KINDS:
            continue

        identifier = item.get("id")
        summary = paths.get(str(identifier)) or paths.get(identifier)
        if summary and summary.get("path"):
            name = "::".join(summary["path"])
        elif item.get("name"):
            held_by = owner.get(str(identifier))
            name = f"{held_by}::{item['name']}" if held_by else item["name"]
        else:
            continue

        lines.add(f"{kind} {name}")

    for line in sorted(lines):
        print(line)
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: public-api.py <rustdoc.json>", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1]))
