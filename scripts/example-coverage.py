#!/usr/bin/env python3
"""Report which crate-root types have a runnable example, and which do not.

The bar this repository sets is that a caller can land on any type's page and
see it used. Measuring it matters because the gap is otherwise invisible: a
type with well-documented *methods* and no example of its own still leaves
someone who arrived from a search with nothing to copy.

Scoped to what `use libtmux::X` reaches. A private helper's page is not where
anyone lands, and a trivial accessor inherits the example on its type.
"""

from __future__ import annotations

import json
import sys

TYPES = {"struct", "enum", "trait", "type_alias"}


def exported_ids(doc: dict) -> set[str]:
    """Ids reachable as `libtmux::X`, following the root's re-exports."""
    index = doc["index"]
    root = doc["root"]
    item = index.get(str(root)) or index.get(root)
    if not item:
        return set()

    found: set[str] = set()
    for child in (item.get("inner") or {}).get("module", {}).get("items") or []:
        entry = index.get(str(child)) or index.get(child)
        if not entry:
            continue
        inner = entry.get("inner") or {}
        if "use" in inner:
            target = inner["use"].get("id")
            if target is not None:
                found.add(str(target))
        else:
            found.add(str(child))
    return found


def main(path: str) -> int:
    with open(path, encoding="utf-8") as handle:
        doc = json.load(handle)

    index = doc["index"]
    have: list[str] = []
    missing: list[str] = []

    for identifier in exported_ids(doc):
        item = index.get(identifier)
        if not item:
            continue
        kind = next(iter(item.get("inner") or {}), None)
        if kind not in TYPES:
            continue
        label = f"{kind} {item.get('name') or '?'}"
        (have if "```" in (item.get("docs") or "") else missing).append(label)

    total = len(have) + len(missing)
    percent = (100 * len(have) / total) if total else 0.0
    print(f"crate-root types with a runnable example: {len(have)}/{total} ({percent:.0f}%)")
    if missing:
        print()
        for label in sorted(missing):
            print(f"  {label}")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: example-coverage.py <rustdoc.json>", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1]))
