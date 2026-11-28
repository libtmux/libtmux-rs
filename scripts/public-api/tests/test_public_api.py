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


def test_records_external_reexports_even_when_the_definition_is_not_in_the_index(
    tmp_path: pathlib.Path,
) -> None:
    doc = fixture()
    doc["index"]["0"]["inner"]["module"]["items"].append(20)
    doc["index"]["20"] = {
        "id": 20,
        "visibility": "public",
        "inner": {"use": {"id": 21, "name": "NewSession", "is_glob": False}},
    }
    doc["paths"]["21"] = {
        "crate_id": 2,
        "path": ["libtmux_core", "new_session", "NewSession"],
        "kind": "struct",
    }
    source = tmp_path / "rustdoc.json"
    source.write_text(json.dumps(doc), encoding="utf-8")
    result = run(str(source))
    assert result.returncode == 0, result.stdout + result.stderr
    assert (
        "struct libtmux::NewSession = libtmux_core::new_session::NewSession"
        in result.stdout.splitlines()
    )


def test_records_external_globs_and_module_aliases(tmp_path: pathlib.Path) -> None:
    doc = fixture()
    for identifier, source_name, exported, glob in (
        (20, "libtmux_core", "libtmux_core", True),
        (22, "libtmux_tokio", "tokio", False),
    ):
        doc["index"]["0"]["inner"]["module"]["items"].append(identifier)
        doc["index"][str(identifier)] = {
            "id": identifier,
            "visibility": "public",
            "inner": {"use": {"id": identifier + 1, "name": exported, "is_glob": glob}},
        }
        doc["paths"][str(identifier + 1)] = {
            "crate_id": 2,
            "path": [source_name],
            "kind": "module",
        }
    source = tmp_path / "rustdoc.json"
    source.write_text(json.dumps(doc), encoding="utf-8")
    result = run(str(source))
    assert result.returncode == 0, result.stdout + result.stderr
    assert "module libtmux::tokio = libtmux_tokio" in result.stdout.splitlines()
    assert "use libtmux::* = libtmux_core::*" in result.stdout.splitlines()


def test_hidden_json_records_visible_alias_and_signature_types_without_helpers(
    tmp_path: pathlib.Path,
) -> None:
    doc = fixture()
    doc["index"]["0"]["inner"]["module"]["items"] = [20, 21]
    doc["index"]["20"] = {
        "id": 20,
        "name": "implementation",
        "visibility": "public",
        "attrs": [{"other": "#[doc(hidden)]"}],
        "inner": {"module": {"items": [1, 22, 23]}},
    }
    doc["index"]["21"] = {
        "id": 21,
        "visibility": "public",
        "inner": {"use": {"id": 1, "name": "Visible", "is_glob": False}},
    }
    for identifier, name in [(22, "HiddenOnlyHelper"), (23, "ReturnValue")]:
        doc["index"][str(identifier)] = {
            "id": identifier,
            "name": name,
            "visibility": "public",
            "inner": {
                "struct": {"kind": "unit", "generics": {"params": [], "where_predicates": []}}
            },
        }
        doc["paths"][str(identifier)] = {
            "crate_id": 0,
            "kind": "struct",
            "path": ["libtmux", "implementation", name],
        }
    doc["index"]["4"]["inner"]["function"]["sig"]["output"] = {
        "resolved_path": {"id": 23, "path": "ReturnValue", "args": None},
    }
    doc["index"]["24"] = {
        **doc["index"]["4"],
        "id": 24,
        "name": "internal_method",
        "attrs": ["#[doc(hidden)]"],
    }
    doc["index"]["3"]["inner"]["impl"]["items"].append(24)
    doc["index"]["25"] = {
        "id": 25,
        "inner": {
            "impl": {
                "for": {
                    "resolved_path": {
                        "id": 22,
                        "path": "HiddenOnlyHelper",
                        "args": {
                            "angle_bracketed": {
                                "args": [{"type": {"resolved_path": {"id": 1, "path": "Widget"}}}],
                                "constraints": [],
                            }
                        },
                    }
                },
                "trait": {"id": 9, "path": "From"},
                "items": [],
                "is_synthetic": False,
                "blanket_impl": None,
            }
        },
    }
    source = tmp_path / "hidden.json"
    source.write_text(json.dumps(doc), encoding="utf-8")
    result = run(str(source))
    assert result.returncode == 0, result.stdout + result.stderr
    assert "struct libtmux::Visible" in result.stdout.splitlines()
    assert "struct libtmux::implementation::ReturnValue" in result.stdout.splitlines()
    assert (
        "function libtmux::Visible::get: fn(&self) -> libtmux::implementation::ReturnValue"
        in result.stdout.splitlines()
    )
    assert "HiddenOnlyHelper" not in result.stdout
    assert "internal_method" not in result.stdout
    assert "module libtmux::implementation" not in result.stdout.splitlines()
