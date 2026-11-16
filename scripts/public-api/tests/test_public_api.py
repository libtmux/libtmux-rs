from __future__ import annotations

import json
import pathlib
import subprocess
import sys

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "public_api.py"


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


def run(*arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *arguments],
        check=False,
        capture_output=True,
        text=True,
    )


EXPECTED = [
    "function libtmux::Widget::get: fn(&self) -> u64",
    "impl From<&str> for libtmux::Widget",
    "struct libtmux::Widget",
    "struct_field libtmux::Widget::value: u64",
]


def test_records_signatures_and_explicit_trait_impls(tmp_path: pathlib.Path) -> None:
    source = tmp_path / "rustdoc.json"
    source.write_text(json.dumps(fixture()), encoding="utf-8")
    result = run(str(source))
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout.splitlines() == EXPECTED


def test_check_passes_on_the_committed_surface(tmp_path: pathlib.Path) -> None:
    source = tmp_path / "rustdoc.json"
    source.write_text(json.dumps(fixture()), encoding="utf-8")
    committed = tmp_path / "public-api.txt"
    committed.write_text("\n".join(EXPECTED) + "\n", encoding="utf-8")
    result = run("--check", str(committed), str(source))
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout == "public API unchanged\n"


def test_check_fails_and_shows_the_difference_when_the_surface_moved(
    tmp_path: pathlib.Path,
) -> None:
    source = tmp_path / "rustdoc.json"
    source.write_text(json.dumps(fixture()), encoding="utf-8")
    committed = tmp_path / "public-api.txt"
    committed.write_text("\n".join(EXPECTED[:-1]) + "\n", encoding="utf-8")
    result = run("--check", str(committed), str(source))
    assert result.returncode == 1
    assert "+struct_field libtmux::Widget::value: u64" in result.stdout
    assert "Run `just api`" in result.stderr
