from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("public-api.py")


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
