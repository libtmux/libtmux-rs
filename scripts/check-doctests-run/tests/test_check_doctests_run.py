"""A doctest that shows lines it never runs is reported, and one that runs them is not."""

from __future__ import annotations

from pathlib import Path

import check_doctests_run
import pytest


def scan(tmp_path: Path, block: str, fence: str = "") -> int:
    lines = [f"/// ```{fence}", *(f"/// {line}" for line in block.splitlines()), "/// ```"]
    (tmp_path / "lib.rs").write_text("\n".join(lines) + "\npub fn item() {}\n", encoding="utf-8")
    return check_doctests_run.main([str(tmp_path)])


def test_a_block_that_runs_what_it_shows_passes(tmp_path: Path) -> None:
    assert scan(tmp_path, "let x = 1 + 1;\nassert_eq!(x, 2);") == 0


def test_a_function_nobody_calls_is_reported(tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
    block = "# async fn example() {\nlet x = 1;\nassert_eq!(x, 1);\n# }"
    assert scan(tmp_path, block) == 1
    assert "never reached" in capsys.readouterr().err


def test_calling_the_function_makes_it_run(tmp_path: Path) -> None:
    block = "fn example() {\n    assert_eq!(1, 1);\n}\nexample();"
    assert scan(tmp_path, block) == 0


def test_a_block_of_definitions_only_is_inert(
    tmp_path: Path, capsys: pytest.CaptureFixture
) -> None:
    assert scan(tmp_path, "struct A;\nimpl A {\n    fn f(&self) {}\n}") == 1
    assert "execute no statement" in capsys.readouterr().err


def test_a_fully_hidden_block_is_invisible(tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
    assert scan(tmp_path, "# let x = 1;\n# assert_eq!(x, 1);") == 1
    assert "hide their whole body" in capsys.readouterr().err


@pytest.mark.parametrize("fence", ["no_run", "ignore", "compile_fail", "text"])
def test_a_declared_block_is_exempt(tmp_path: Path, fence: str) -> None:
    assert scan(tmp_path, "# fn never() {}\n# let x = 1;", fence) == 0
