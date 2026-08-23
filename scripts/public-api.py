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


def public_paths(index: dict, root, crate: str) -> dict:
    """Map each item to the path a caller can actually name it by.

    rustdoc reports where an item was defined, which is not where it can be
    reached: this crate keeps its modules private and re-exports from the root,
    so `libtmux::limits::DispatchLimits` is what rustdoc says and
    `libtmux::DispatchLimits` is what compiles. A ledger that prints the first
    is a ledger a reviewer cannot paste.

    Breadth-first, so the first path found for an item is a shortest one, which
    is the one a caller would write.
    """
    best: dict = {}
    frontier = [(root, crate)]
    visited = set()

    while frontier:
        following = []
        for module_id, prefix in frontier:
            if module_id in visited:
                continue
            visited.add(module_id)
            item = index.get(str(module_id)) or index.get(module_id)
            module = (item or {}).get("inner", {}).get("module")
            if module is None:
                continue

            for child_id in module.get("items") or []:
                child = index.get(str(child_id)) or index.get(child_id)
                if not child or child.get("visibility") not in ("public", "default"):
                    continue
                inner = child.get("inner") or {}
                kind = next(iter(inner), None)

                if kind == "use":
                    used = inner["use"]
                    target = used.get("id")
                    if target is None:
                        continue
                    # A glob puts the module's contents at this path rather
                    # than the module itself.
                    if used.get("is_glob"):
                        following.append((target, prefix))
                        continue
                    here = f"{prefix}::{used.get('name')}"
                    best.setdefault(str(target), here)
                    aliased = index.get(str(target)) or index.get(target)
                    if aliased and "module" in (aliased.get("inner") or {}):
                        following.append((target, here))
                    continue

                name = child.get("name")
                if not name:
                    continue
                here = f"{prefix}::{name}"
                best.setdefault(str(child_id), here)
                if kind == "module":
                    following.append((child_id, here))

        frontier = following

    return best


def parents(index: dict, paths: dict, reachable: dict) -> dict:
    """Map each child item to the type that owns it.

    Methods, fields, and variants have no standalone path in rustdoc's output,
    so an unqualified `sessions` says nothing about which handle it belongs to
    and a move between types would not show in a diff.
    """
    owner: dict = {}

    def name_of(identifier) -> str | None:
        reached = reachable.get(str(identifier))
        if reached:
            return reached
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
    root = doc["root"]
    summary = paths.get(str(root)) or paths.get(root) or {}
    crate = (summary.get("path") or ["crate"])[0]
    reachable = public_paths(index, root, crate)
    owner = parents(index, paths, reachable)
    lines = set()

    for item in index.values():
        if item.get("visibility") not in ("public", "default"):
            continue
        inner = item.get("inner") or {}
        kind = next(iter(inner), None)
        if kind not in KINDS:
            continue

        identifier = item.get("id")
        reached = reachable.get(str(identifier))
        held_by = owner.get(str(identifier))
        summary = paths.get(str(identifier)) or paths.get(identifier)
        # The owner is consulted before rustdoc's own path because rustdoc
        # carries a path for a method too, and it is the one under the private
        # module the type was defined in rather than the one it is reached by.
        if reached:
            name = reached
        elif held_by and item.get("name"):
            name = f"{held_by}::{item['name']}"
        elif summary and summary.get("path"):
            name = "::".join(summary["path"])
        elif item.get("name"):
            name = item["name"]
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
