from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("public-api.py")
COVERAGE = SCRIPT.with_name("example-coverage.py")


def fixture() -> dict:
    return {
        "root": 0,
        "paths": {
            "0": {"crate_id": 0, "path": ["libtmux"], "kind": "module"},
            "1": {
                "crate_id": 0,
                "path": ["libtmux", "private", "Widget"],
                "kind": "struct",
            },
            "9": {
                "crate_id": 2,
                "path": ["core", "convert", "From"],
                "kind": "trait",
            },
        },
        "index": {
            "0": {
                "id": 0,
                "name": "libtmux",
                "visibility": "public",
                "inner": {"module": {"items": [1]}},
            },
            "1": {
                "id": 1,
                "name": "Widget",
                "visibility": "public",
                "inner": {
                    "struct": {
                        "kind": {"plain": {"fields": [2]}},
                        "generics": {"params": [], "where_predicates": []},
                        "impls": [3, 5],
                    }
                },
            },
            "2": {
                "id": 2,
                "name": "value",
                "visibility": "public",
                "inner": {"struct_field": {"primitive": "u64"}},
            },
            "3": {
                "id": 3,
                "name": None,
                "visibility": "default",
                "inner": {
                    "impl": {
                        "for": {
                            "resolved_path": {
                                "path": "Widget",
                                "id": 1,
                                "args": None,
                            }
                        },
                        "trait": None,
                        "items": [4],
                        "is_synthetic": False,
                        "blanket_impl": None,
                    }
                },
            },
            "4": {
                "id": 4,
                "name": "get",
                "visibility": "public",
                "inner": {
                    "function": {
                        "sig": {
                            "inputs": [
                                [
                                    "self",
                                    {
                                        "borrowed_ref": {
                                            "lifetime": None,
                                            "is_mutable": False,
                                            "type": {"generic": "Self"},
                                        }
                                    },
                                ]
                            ],
                            "output": {"primitive": "u64"},
                            "is_c_variadic": False,
                        },
                        "generics": {"params": [], "where_predicates": []},
                        "header": {
                            "is_const": False,
                            "is_unsafe": False,
                            "is_async": False,
                            "abi": "Rust",
                        },
                    }
                },
            },
            "5": {
                "id": 5,
                "name": None,
                "visibility": "default",
                "inner": {
                    "impl": {
                        "for": {
                            "resolved_path": {
                                "path": "Widget",
                                "id": 1,
                                "args": None,
                            }
                        },
                        "trait": {
                            "path": "From",
                            "id": 9,
                            "args": {
                                "angle_bracketed": {
                                    "args": [
                                        {
                                            "type": {
                                                "borrowed_ref": {
                                                    "lifetime": None,
                                                    "is_mutable": False,
                                                    "type": {"primitive": "str"},
                                                }
                                            }
                                        }
                                    ],
                                    "constraints": [],
                                }
                            },
                        },
                        "items": [],
                        "is_negative": False,
                        "is_synthetic": False,
                        "blanket_impl": None,
                    }
                },
            },
        },
    }


class PublicApiTest(unittest.TestCase):
    def render_adapter(self, dependency: dict, *, exported: bool = True,
                       coverage: bool = False) -> subprocess.CompletedProcess:
        adapter = {
            "root": 0,
            "paths": {
                "0": {"crate_id": 0, "path": ["adapter"], "kind": "module"},
                "2": {"crate_id": 1, "path": ["libtmux", "private", "Widget"], "kind": "struct"},
            },
            "index": {
                "0": {"id": 0, "name": "adapter", "visibility": "public",
                      "inner": {"module": {"items": [1] if exported else []}}},
                "1": {"id": 1, "name": None, "visibility": "public",
                      "inner": {"use": {"source": "libtmux::Widget", "name": "Gadget",
                                        "id": 2, "is_glob": False}}},
            },
        }
        with tempfile.TemporaryDirectory() as directory:
            source = pathlib.Path(directory) / "adapter.json"
            peer = pathlib.Path(directory) / "libtmux.json"
            source.write_text(json.dumps(adapter), encoding="utf-8")
            peer.write_text(json.dumps(dependency), encoding="utf-8")
            command = [sys.executable, str(COVERAGE if coverage else SCRIPT), str(source),
                       "--dependency-json", str(peer)]
            if coverage:
                command.append("--require-all")
            return subprocess.run(command, check=False, capture_output=True, text=True)

    def test_workspace_alias_includes_members_and_impls(self) -> None:
        result = self.render_adapter(fixture())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "function adapter::Gadget::get: fn(&self) -> u64",
            "impl From<&str> for adapter::Gadget",
            "struct adapter::Gadget",
            "struct_field adapter::Gadget::value: u64",
        ])

    def test_dependency_signature_mutation_changes_inventory(self) -> None:
        original = self.render_adapter(fixture())
        changed = fixture()
        changed["index"]["4"]["inner"]["function"]["sig"]["output"] = {"primitive": "bool"}
        result = self.render_adapter(changed)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual(result.stdout, original.stdout)
        self.assertIn("function adapter::Gadget::get: fn(&self) -> bool", result.stdout)

    def test_removed_export_removes_dependency_surface(self) -> None:
        result = self.render_adapter(fixture(), exported=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_dependency_example_is_required(self) -> None:
        dependency = fixture()
        dependency["index"]["1"]["docs"] = "```rust\nlet value = 1;\n```"
        present = self.render_adapter(dependency, coverage=True)
        self.assertEqual(present.returncode, 0, present.stderr)
        self.assertIn("1/1 (100%)", present.stdout)
        dependency["index"]["1"]["docs"] = "An undocumented example."
        missing = self.render_adapter(dependency, coverage=True)
        self.assertEqual(missing.returncode, 1, missing.stderr)
        self.assertIn("0/1 (0%)", missing.stdout)

    def test_records_signatures_and_explicit_trait_impls(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = pathlib.Path(directory) / "rustdoc.json"
            source.write_text(json.dumps(fixture()), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(SCRIPT), str(source)],
                check=False,
                capture_output=True,
                text=True,
            )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            [
                "function libtmux::Widget::get: fn(&self) -> u64",
                "impl From<&str> for libtmux::Widget",
                "struct libtmux::Widget",
                "struct_field libtmux::Widget::value: u64",
            ],
        )


if __name__ == "__main__":
    unittest.main()
