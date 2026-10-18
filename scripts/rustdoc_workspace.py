"""Resolve workspace re-exports using the declaring crate's rustdoc JSON."""

from __future__ import annotations

import json
from pathlib import Path


def crate_name(doc: dict) -> str:
    return doc["index"][str(doc["root"])]["name"]


def load(path: str, dependency_paths: list[str]) -> dict:
    primary = json.loads(Path(path).read_text())
    dependencies = [json.loads(Path(item).read_text()) for item in dependency_paths]
    return resolve(primary, dependencies)


def resolve(primary: dict, dependencies: list[dict]) -> dict:
    """Import declarations reachable through explicit workspace re-exports.

    IDs are namespaced before joining documents. Referenced signature types
    retain path metadata; only exported declarations and their owned members
    enter the combined index. Unrelated dependency APIs stay out of it.
    """
    documents = {crate_name(doc): doc for doc in [*dependencies, primary]}
    definitions = {}
    for name, doc in documents.items():
        local_crate = doc["index"][str(doc["root"])].get("crate_id", 0)
        definitions[name] = {
            tuple(item["path"]): str(identifier)
            for identifier, item in doc["paths"].items()
            if item["crate_id"] == local_crate and str(identifier) in doc["index"]
        }

    def locate(name, identifier):
        identifier = str(identifier)
        doc = documents[name]
        if identifier in doc["index"]:
            return name, identifier
        summary = doc["paths"].get(identifier)
        if summary:
            target_path = tuple(summary["path"])
            dependency = target_path[0]
            declared = definitions.get(dependency, {}).get(target_path)
            if declared is not None:
                return dependency, declared
        return name, identifier

    def key(name, identifier):
        owner, local = locate(name, identifier)
        return f"{owner}:{local}"

    id_lists = {"items", "impls", "implementations", "variants", "fields"}

    def rewrite(value, name, field=None):
        if value is None:
            return None
        if field == "id":
            return key(name, value)
        if field == "links":
            return {label: key(name, identifier) for label, identifier in value.items()}
        if isinstance(value, dict):
            return {child: rewrite(item, name, child) for child, item in value.items()}
        if isinstance(value, list):
            if field in id_lists or (field == "tuple" and all(item is None or isinstance(item, int) for item in value)):
                return [key(name, item) if item is not None else None for item in value]
            return [rewrite(item, name) for item in value]
        return value

    index = {}
    paths = {}
    for name, doc in documents.items():
        for identifier, item in doc["paths"].items():
            paths.setdefault(key(name, identifier), item)

    primary_name = crate_name(primary)
    pending = [(primary_name, identifier) for identifier in primary["index"]]
    while pending:
        name, identifier = locate(*pending.pop())
        identity = key(name, identifier)
        if identity in index:
            continue
        item = documents[name]["index"].get(str(identifier))
        if item is None:
            continue
        index[identity] = rewrite(item, name)
        inner = item.get("inner") or {}
        if "use" in inner:
            target = inner["use"].get("id")
            if target is not None:
                pending.append((name, target))
        if name == primary_name:
            continue

        def owned(value, field=None):
            if isinstance(value, dict):
                for child, entry in value.items():
                    owned(entry, child)
            elif isinstance(value, list):
                if field in id_lists or (field == "tuple" and all(entry is None or isinstance(entry, int) for entry in value)):
                    pending.extend((name, entry) for entry in value if entry is not None)
                else:
                    for entry in value:
                        owned(entry)

        owned(inner)

    return {**primary, "root": key(primary_name, primary["root"]), "index": index, "paths": paths}
