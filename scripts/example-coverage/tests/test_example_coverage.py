"""A crate-root type without an example is counted, and `--require-all` fails on it."""

from __future__ import annotations

import json
from pathlib import Path

import example_coverage
import pytest


def item(name: str, docs: str) -> dict:
    return {"name": name, "docs": docs, "inner": {"struct": {}}}


def write(tmp_path: Path, documented: str) -> Path:
    doc = {
        "root": 0,
        "index": {
            "0": {"inner": {"module": {"items": [1, 2]}}},
            "1": item("Server", "```\nlet x = 1;\n```"),
            "2": item("Pane", documented),
        },
    }
    path = tmp_path / "libtmux.json"
    path.write_text(json.dumps(doc), encoding="utf-8")
    return path


def test_the_count_names_the_type_without_an_example(
    tmp_path: Path, capsys: pytest.CaptureFixture
) -> None:
    assert example_coverage.main(str(write(tmp_path, "no example"))) == 0
    out = capsys.readouterr().out
    assert "1/2" in out
    assert "struct Pane" in out


def test_require_all_fails_when_a_type_has_no_example(tmp_path: Path) -> None:
    assert example_coverage.main_strict(str(write(tmp_path, "no example"))) == 1


def test_require_all_passes_when_every_type_has_one(tmp_path: Path) -> None:
    assert example_coverage.main_strict(str(write(tmp_path, "```\nlet y = 2;\n```"))) == 0


def test_hidden_json_keeps_visible_aliases_but_skips_hidden_types(
    tmp_path: Path, capsys: pytest.CaptureFixture
) -> None:
    doc = {
        "root": 0,
        "index": {
            "0": {"inner": {"module": {"items": [1, 2, 3]}}},
            "1": {"inner": {"use": {"id": 4}}},
            "2": {"inner": {"use": {"id": 5}}},
            "3": {
                "attrs": [{"other": "#[doc(hidden)]"}],
                "inner": {"module": {"items": [4, 6]}},
            },
            "4": item("VisibleSnapshot", "```\nlet y = 2;\n```"),
            "5": {**item("InternalReply", "no example"), "attrs": [{"other": "#[doc(hidden)]"}]},
            "6": item("HiddenOnlyHelper", "no example"),
        },
    }
    path = tmp_path / "hidden.json"
    path.write_text(json.dumps(doc), encoding="utf-8")
    assert example_coverage.main_strict(str(path)) == 0
    assert "1/1" in capsys.readouterr().out
    doc["index"]["4"]["docs"] = "no example"
    path.write_text(json.dumps(doc), encoding="utf-8")
    assert example_coverage.main_strict(str(path)) == 1
