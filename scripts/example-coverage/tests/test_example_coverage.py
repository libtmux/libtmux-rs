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
