#!/usr/bin/env python3
"""Print the public API surface of a crate, one record per line.

Built from rustdoc's JSON rather than a separate tool, so the only thing it
needs is the nightly the fuzz targets already require. The output is sorted and
stable enough to diff: item signatures and non-blanket trait implementations
change when a caller-visible contract changes.

It is not a semver oracle. It says what moved, which is what
`cargo-semver-checks` cannot say while every version is a prerelease and it
skips every lint.
"""

from __future__ import annotations

import json
import sys

# Item kinds that are part of the surface. Modules only contribute paths;
# trait implementation headers are recorded separately.
KINDS = {
    "struct", "enum", "trait", "function", "type_alias", "constant",
    "macro", "proc_macro", "struct_field", "variant", "assoc_const",
    "assoc_type", "primitive", "union",
}


def lookup(mapping: dict, identifier):
    """Read a rustdoc map whose integer IDs are serialized as strings."""
    return mapping.get(str(identifier)) or mapping.get(identifier)


def render_constant(value) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        return value.get("expr") or value.get("value") or "_"
    return str(value)


def render_path(value: dict, paths: dict, reachable: dict) -> str:
    identifier = value.get("id")
    name = reachable.get(str(identifier))
    if name is None:
        summary = lookup(paths, identifier) or {}
        recorded = summary.get("path") or []
        raw = value.get("path")
        if raw and raw.startswith(("$crate::", "_serde::")) and recorded:
            name = recorded[-1]
        else:
            name = raw or ("::".join(recorded) if recorded else "_")
    return name + render_args(value.get("args"), paths, reachable)


def render_arg(value: dict, paths: dict, reachable: dict) -> str:
    if "type" in value:
        return render_type(value["type"], paths, reachable)
    if "lifetime" in value:
        return value["lifetime"]
    if "const" in value:
        return render_constant(value["const"])
    if "infer" in value:
        return "_"
    raise ValueError(f"unsupported rustdoc generic argument: {value!r}")


def render_term(value: dict, paths: dict, reachable: dict) -> str:
    if "type" in value:
        return render_type(value["type"], paths, reachable)
    if "constant" in value:
        return render_constant(value["constant"])
    raise ValueError(f"unsupported rustdoc term: {value!r}")


def render_constraint(value: dict, paths: dict, reachable: dict) -> str:
    name = value["name"] + render_args(value.get("args"), paths, reachable)
    binding = value.get("binding") or {}
    if "equality" in binding:
        return f"{name} = {render_term(binding['equality'], paths, reachable)}"
    if "constraint" in binding:
        bounds = render_bounds(binding["constraint"], paths, reachable)
        return f"{name}: {bounds}"
    return name


def render_args(value, paths: dict, reachable: dict) -> str:
    if not value:
        return ""
    if "angle_bracketed" in value:
        inside = value["angle_bracketed"]
        parts = [render_arg(arg, paths, reachable) for arg in inside.get("args") or []]
        parts.extend(
            render_constraint(constraint, paths, reachable)
            for constraint in inside.get("constraints") or []
        )
        return f"<{', '.join(parts)}>" if parts else ""
    if "parenthesized" in value:
        inside = value["parenthesized"]
        inputs = ", ".join(
            render_type(item, paths, reachable) for item in inside.get("inputs") or []
        )
        output = inside.get("output")
        suffix = (
            f" -> {render_type(output, paths, reachable)}" if output is not None else ""
        )
        return f"({inputs}){suffix}"
    raise ValueError(f"unsupported rustdoc generic arguments: {value!r}")


def render_bound(value: dict, paths: dict, reachable: dict) -> str:
    if "trait_bound" in value:
        bound = value["trait_bound"]
        prefix = {
            "none": "",
            "maybe": "?",
            "maybe_const": "~const ",
        }.get(bound.get("modifier"), f"{bound.get('modifier')} ")
        generic = render_generic_params(
            bound.get("generic_params") or [], paths, reachable
        )
        higher_ranked = f"for{generic} " if generic else ""
        return higher_ranked + prefix + render_path(bound["trait"], paths, reachable)
    if "outlives" in value:
        return value["outlives"]
    if "use" in value:
        captured = value["use"]
        if isinstance(captured, list):
            captured = ", ".join(captured)
        return f"use<{captured}>"
    raise ValueError(f"unsupported rustdoc bound: {value!r}")


def render_bounds(values: list, paths: dict, reachable: dict) -> str:
    return " + ".join(render_bound(value, paths, reachable) for value in values)


def render_type(value: dict, paths: dict, reachable: dict) -> str:
    if "resolved_path" in value:
        return render_path(value["resolved_path"], paths, reachable)
    if "generic" in value:
        return value["generic"]
    if "primitive" in value:
        return value["primitive"]
    if "tuple" in value:
        parts = [render_type(item, paths, reachable) for item in value["tuple"]]
        comma = "," if len(parts) == 1 else ""
        return f"({', '.join(parts)}{comma})"
    if "slice" in value:
        return f"[{render_type(value['slice'], paths, reachable)}]"
    if "array" in value:
        array = value["array"]
        return (
            f"[{render_type(array['type'], paths, reachable)}; "
            f"{render_constant(array['len'])}]"
        )
    if "borrowed_ref" in value:
        reference = value["borrowed_ref"]
        lifetime = f"{reference['lifetime']} " if reference.get("lifetime") else ""
        mutable = "mut " if reference.get("is_mutable") else ""
        held = render_type(reference["type"], paths, reachable)
        return f"&{lifetime}{mutable}{held}"
    if "raw_pointer" in value:
        pointer = value["raw_pointer"]
        mutable = "mut" if pointer.get("is_mutable") else "const"
        return f"*{mutable} {render_type(pointer['type'], paths, reachable)}"
    if "impl_trait" in value:
        return f"impl {render_bounds(value['impl_trait'], paths, reachable)}"
    if "dyn_trait" in value:
        dynamic = value["dyn_trait"]
        bounds = []
        for trait in dynamic.get("traits") or []:
            generic = render_generic_params(
                trait.get("generic_params") or [], paths, reachable
            )
            higher_ranked = f"for{generic} " if generic else ""
            bounds.append(higher_ranked + render_path(trait["trait"], paths, reachable))
        if dynamic.get("lifetime"):
            bounds.append(dynamic["lifetime"])
        return f"dyn {' + '.join(bounds)}"
    if "function_pointer" in value:
        pointer = value["function_pointer"]
        generic = render_generic_params(
            pointer.get("generic_params") or [], paths, reachable
        )
        return render_function(
            pointer["sig"], pointer["header"], {"params": [], "where_predicates": []},
            paths, reachable, generic_override=generic,
        )
    if "qualified_path" in value:
        qualified = value["qualified_path"]
        held = render_type(qualified["self_type"], paths, reachable)
        trait = qualified.get("trait")
        prefix = f"<{held} as {render_path(trait, paths, reachable)}>" if trait else held
        return (
            f"{prefix}::{qualified['name']}"
            f"{render_args(qualified.get('args'), paths, reachable)}"
        )
    if "pat" in value:
        return render_type(value["pat"]["type"], paths, reachable)
    if "infer" in value:
        return "_"
    raise ValueError(f"unsupported rustdoc type: {value!r}")


def render_generic_param(value: dict, paths: dict, reachable: dict) -> str | None:
    name = value["name"]
    kind = value.get("kind") or {}
    if "type" in kind:
        detail = kind["type"]
        if detail.get("is_synthetic"):
            return None
        bounds = render_bounds(detail.get("bounds") or [], paths, reachable)
        suffix = f": {bounds}" if bounds else ""
        if detail.get("default") is not None:
            suffix += f" = {render_type(detail['default'], paths, reachable)}"
        return name + suffix
    if "lifetime" in kind:
        outlives = kind["lifetime"].get("outlives") or []
        return name + (f": {' + '.join(outlives)}" if outlives else "")
    if "const" in kind:
        detail = kind["const"]
        rendered = f"const {name}: {render_type(detail['type'], paths, reachable)}"
        if detail.get("default") is not None:
            rendered += f" = {render_constant(detail['default'])}"
        return rendered
    raise ValueError(f"unsupported rustdoc generic parameter: {value!r}")


def render_generic_params(values: list, paths: dict, reachable: dict) -> str:
    rendered = [render_generic_param(value, paths, reachable) for value in values]
    rendered = [value for value in rendered if value is not None]
    return f"<{', '.join(rendered)}>" if rendered else ""


def render_generics(value: dict, paths: dict, reachable: dict) -> tuple[str, str]:
    parameters = render_generic_params(value.get("params") or [], paths, reachable)
    predicates = []
    for wrapped in value.get("where_predicates") or []:
        if "bound_predicate" in wrapped:
            predicate = wrapped["bound_predicate"]
            generic = render_generic_params(
                predicate.get("generic_params") or [], paths, reachable
            )
            higher_ranked = f"for{generic} " if generic else ""
            bounds = render_bounds(predicate.get("bounds") or [], paths, reachable)
            predicates.append(
                f"{higher_ranked}{render_type(predicate['type'], paths, reachable)}: {bounds}"
            )
        elif "lifetime_predicate" in wrapped:
            predicate = wrapped["lifetime_predicate"]
            predicates.append(
                f"{predicate['lifetime']}: {' + '.join(predicate.get('outlives') or [])}"
            )
        elif "eq_predicate" in wrapped:
            predicate = wrapped["eq_predicate"]
            predicates.append(
                f"{render_term(predicate['lhs'], paths, reachable)} = "
                f"{render_term(predicate['rhs'], paths, reachable)}"
            )
        else:
            raise ValueError(f"unsupported rustdoc where predicate: {wrapped!r}")
    where = f" where {', '.join(predicates)}" if predicates else ""
    return parameters, where


def render_abi(value) -> str:
    if value == "Rust":
        return ""
    if isinstance(value, str):
        return f'extern "{value}" '
    if isinstance(value, dict):
        name, detail = next(iter(value.items()))
        unwind = "-unwind" if isinstance(detail, dict) and detail.get("unwind") else ""
        return f'extern "{name}{unwind}" '
    raise ValueError(f"unsupported rustdoc ABI: {value!r}")


def render_input(name: str, value: dict, paths: dict, reachable: dict) -> str:
    if name == "self":
        if value == {"generic": "Self"}:
            return "self"
        reference = value.get("borrowed_ref")
        if reference and reference.get("type") == {"generic": "Self"}:
            lifetime = f"{reference['lifetime']} " if reference.get("lifetime") else ""
            mutable = "mut " if reference.get("is_mutable") else ""
            return f"&{lifetime}{mutable}self"
    return f"{name}: {render_type(value, paths, reachable)}"


def render_function(
    signature: dict,
    header: dict,
    generics: dict,
    paths: dict,
    reachable: dict,
    *,
    generic_override: str | None = None,
) -> str:
    parameters, where = render_generics(generics, paths, reachable)
    if generic_override is not None:
        parameters = generic_override
    qualifiers = ""
    if header.get("is_const"):
        qualifiers += "const "
    if header.get("is_async"):
        qualifiers += "async "
    if header.get("is_unsafe"):
        qualifiers += "unsafe "
    qualifiers += render_abi(header.get("abi", "Rust"))
    inputs = [
        render_input(name, value, paths, reachable)
        for name, value in signature.get("inputs") or []
    ]
    if signature.get("is_c_variadic"):
        inputs.append("...")
    output = signature.get("output")
    result = f"{qualifiers}fn{parameters}({', '.join(inputs)})"
    if output is not None:
        result += f" -> {render_type(output, paths, reachable)}"
    return result + where


def item_suffix(kind: str, value, paths: dict, reachable: dict) -> str:
    if kind == "function":
        return ": " + render_function(
            value["sig"], value["header"], value["generics"], paths, reachable
        )
    if kind == "struct_field":
        return f": {render_type(value, paths, reachable)}"
    if kind in ("constant", "assoc_const"):
        return f": {render_type(value['type'], paths, reachable)}"
    if kind == "type_alias":
        parameters, where = render_generics(value["generics"], paths, reachable)
        return f"{parameters} = {render_type(value['type'], paths, reachable)}{where}"
    if kind == "assoc_type":
        parameters, where = render_generics(value["generics"], paths, reachable)
        bounds = render_bounds(value.get("bounds") or [], paths, reachable)
        suffix = parameters + (f": {bounds}" if bounds else "")
        if value.get("type") is not None:
            suffix += f" = {render_type(value['type'], paths, reachable)}"
        return suffix + where
    if kind in ("struct", "enum", "union", "trait"):
        parameters, where = render_generics(value["generics"], paths, reachable)
        bounds = render_bounds(value.get("bounds") or [], paths, reachable)
        suffix = parameters + (f": {bounds}" if bounds else "") + where
        if kind == "trait":
            flags = [name for name in ("unsafe", "auto") if value.get(f"is_{name}")]
            if flags:
                suffix += f" [{' '.join(flags)}]"
        return suffix
    return ""


def type_mentions_reachable(value: dict, reachable: dict) -> bool:
    resolved = value.get("resolved_path")
    if resolved and str(resolved.get("id")) in reachable:
        return True
    return any(
        type_mentions_reachable(child, reachable)
        for child in value.values()
        if isinstance(child, dict)
    ) or any(
        type_mentions_reachable(child, reachable)
        for children in value.values()
        if isinstance(children, list)
        for child in children
        if isinstance(child, dict)
    )


def explicit_impls(index: dict, paths: dict, reachable: dict) -> set[str]:
    lines = set()
    for item in index.values():
        implementation = (item.get("inner") or {}).get("impl")
        if not implementation:
            continue
        trait = implementation.get("trait")
        if (
            not trait
            or implementation.get("is_synthetic")
            or implementation.get("blanket_impl") is not None
            or trait.get("path") == "StructuralPartialEq"
        ):
            continue
        target = implementation.get("for") or {}
        if not (
            str(trait.get("id")) in reachable
            or type_mentions_reachable(target, reachable)
        ):
            continue
        parameters, where = render_generics(
            implementation.get("generics") or {"params": [], "where_predicates": []},
            paths,
            reachable,
        )
        negative = "!" if implementation.get("is_negative") else ""
        lines.add(
            f"impl{parameters} {negative}{render_path(trait, paths, reachable)} for "
            f"{render_type(target, paths, reachable)}{where}"
        )
    return lines


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


def parents(index: dict, paths: dict, reachable: dict) -> tuple[dict, set]:
    """Map each child item to its owner and find trait-impl duplicates.

    Methods, fields, and variants have no standalone path in rustdoc's output,
    so an unqualified `sessions` says nothing about which handle it belongs to
    and a move between types would not show in a diff.
    """
    owner: dict = {}
    implemented: set = set()

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
            if isinstance(kind, dict):
                plain = kind.get("plain") or {}
                fields = (plain.get("fields") or []) + (kind.get("tuple") or [])
            else:
                fields = []
            for child in fields:
                if child is not None:
                    owner[str(child)] = here

        if "enum" in inner:
            here = name_of(item.get("id"))
            for child in inner["enum"].get("variants") or []:
                owner[str(child)] = here
                # A struct-like variant's fields are items in their own right
                # and nothing else attributes them, so without this they record
                # as a bare `kind` or `target`: names that collide with every
                # other field spelled the same and leave a variant's shape free
                # to change under a diff that reports nothing.
                variant = index.get(str(child)) or index.get(child) or {}
                shape = (variant.get("inner") or {}).get("variant") or {}
                kind = shape.get("kind")
                if isinstance(kind, dict):
                    plain = kind.get("struct") or {}
                    fields = (plain.get("fields") or []) + (kind.get("tuple") or [])
                else:
                    fields = []
                held = variant.get("name")
                for field in fields:
                    if field is not None:
                        owner[str(field)] = f"{here}::{held}" if here and held else held

        if "trait" in inner:
            here = name_of(item.get("id"))
            for child in inner["trait"].get("items") or []:
                owner[str(child)] = here

        if "impl" in inner:
            target = inner["impl"].get("for") or {}
            resolved = target.get("resolved_path") or {}
            here = name_of(resolved.get("id")) or resolved.get("path")
            trait = inner["impl"].get("trait") or {}
            # The trait declares its members; its implementation header is
            # recorded separately. Only inherent items need an owner here.
            if trait:
                implemented.update(str(child) for child in inner["impl"].get("items") or [])
                continue
            for child in inner["impl"].get("items") or []:
                owner[str(child)] = here

    return owner, implemented


def main(path: str) -> int:
    with open(path, encoding="utf-8") as handle:
        doc = json.load(handle)

    index = doc["index"]
    paths = doc["paths"]
    root = doc["root"]
    summary = paths.get(str(root)) or paths.get(root) or {}
    crate = (summary.get("path") or ["crate"])[0]
    reachable = public_paths(index, root, crate)
    owner, implemented = parents(index, paths, reachable)
    lines = set()

    for item in index.values():
        if item.get("visibility") not in ("public", "default"):
            continue
        inner = item.get("inner") or {}
        kind = next(iter(inner), None)
        if kind not in KINDS:
            continue

        identifier = item.get("id")
        if str(identifier) in implemented:
            continue
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

        lines.add(f"{kind} {name}{item_suffix(kind, inner[kind], paths, reachable)}")

    lines.update(explicit_impls(index, paths, reachable))

    for line in sorted(lines):
        print(line)
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print("usage: public-api.py <rustdoc.json>", file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1]))
