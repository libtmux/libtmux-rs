"""Tests for `check_example_tables.py`.

The checker runs a cargo example, so what is tested here is everything either
side of that: finding the marked blocks, splitting a row into columns, and
deciding which differences are drift and which are the clock.
"""

from __future__ import annotations

import check_example_tables as checker
import pytest

HEADER = "mode                     dispatches      wall  attribution  query output"
ROW = "async/folded             3                  9ms  merged       2 panes, 2 windows"


def documented(row: str = ROW) -> str:
    return f"{HEADER}\n{row}\n"


def test_a_marked_fence_is_found_with_its_example_and_line() -> None:
    text = "intro\n\n<!-- example-output: matrix --all-features -->\n\n```text\nbody\n```\n"
    assert checker.blocks(text) == [(3, "matrix --all-features", "body\n")]


def test_an_unmarked_fence_is_left_alone() -> None:
    assert checker.blocks("```text\nnot claimed by anything\n```\n") == []


def test_a_marker_with_no_block_is_drift_rather_than_silence() -> None:
    with pytest.raises(checker.Drift):
        checker.blocks("<!-- example-output: matrix -->\n\njust prose\n")


def test_a_marker_does_not_claim_a_block_further_down_the_page() -> None:
    # The block this marker named was deleted. Searching forward would
    # adopt the next one and check the wrong output against it.
    with pytest.raises(checker.Drift):
        checker.blocks("<!-- example-output: matrix -->\n\nprose\n\n```text\nsomeone else's\n```\n")


def test_single_spaces_inside_a_cell_do_not_split_it() -> None:
    assert checker.cells(ROW) == ["async/folded", "3", "9ms", "merged", "2 panes, 2 windows"]


def test_two_timings_agree_however_they_read() -> None:
    assert checker.comparable("9ms", "1483ms")
    assert checker.comparable("1.5s", "12us")


def test_a_count_only_agrees_with_itself() -> None:
    assert not checker.comparable("3", "6")
    assert not checker.comparable("merged", "per-command")
    assert not checker.comparable("9ms", "9")


def test_a_clock_that_moved_is_not_drift() -> None:
    produced = documented(ROW.replace("9ms", "23ms"))
    assert checker.compare(documented(), produced, "matrix", 1) == []


def test_a_changed_count_is_reported_with_both_lines() -> None:
    produced = documented(ROW.replace("3   ", "6   "))
    problems = checker.compare(documented(), produced, "matrix", 10)
    assert len(problems) == 1
    assert "README:11" in problems[0]
    assert "where the block shows" in problems[0]


def test_a_line_the_example_gained_is_reported() -> None:
    produced = documented() + "dispatches ranged 3..6\n"
    problems = checker.compare(documented(), produced, "matrix", 1)
    assert any("printed 3 lines" in problem for problem in problems)


def test_an_empty_block_is_drift() -> None:
    problems = checker.compare("\n", documented(), "matrix", 4)
    assert len(problems) == 1
    assert "empty" in problems[0]
