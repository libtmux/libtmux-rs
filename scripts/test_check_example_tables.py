#!/usr/bin/env python3
"""Tests for `check-example-tables.py`.

The checker runs a cargo example, so what is tested here is everything either
side of that: finding the marked blocks, splitting a row into columns, and
deciding which differences are drift and which are the clock.
"""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

SPEC = importlib.util.spec_from_file_location(
    "check_example_tables",
    pathlib.Path(__file__).with_name("check-example-tables.py"),
)
assert SPEC is not None and SPEC.loader is not None
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)

HEADER = "mode                     dispatches      wall  attribution  query output"
ROW = "async/folded             3                  9ms  merged       2 panes, 2 windows"


class BlockTests(unittest.TestCase):
    def test_a_marked_fence_is_found_with_its_example_and_line(self) -> None:
        text = "intro\n\n<!-- example-output: matrix --all-features -->\n\n```text\nbody\n```\n"
        self.assertEqual(
            checker.blocks(text),
            [(3, "matrix --all-features", "body\n")],
        )

    def test_an_unmarked_fence_is_left_alone(self) -> None:
        self.assertEqual(checker.blocks("```text\nnot claimed by anything\n```\n"), [])

    def test_a_marker_with_no_block_is_drift_rather_than_silence(self) -> None:
        with self.assertRaises(checker.Drift):
            checker.blocks("<!-- example-output: matrix -->\n\njust prose\n")

    def test_a_marker_does_not_claim_a_block_further_down_the_page(self) -> None:
        # The block this marker named was deleted. Searching forward would
        # adopt the next one and check the wrong output against it.
        with self.assertRaises(checker.Drift):
            checker.blocks(
                "<!-- example-output: matrix -->\n\nprose\n\n```text\nsomeone else's\n```\n"
            )


class CellTests(unittest.TestCase):
    def test_single_spaces_inside_a_cell_do_not_split_it(self) -> None:
        self.assertEqual(
            checker.cells(ROW),
            ["async/folded", "3", "9ms", "merged", "2 panes, 2 windows"],
        )

    def test_two_timings_agree_however_they_read(self) -> None:
        self.assertTrue(checker.comparable("9ms", "1483ms"))
        self.assertTrue(checker.comparable("1.5s", "12us"))

    def test_a_count_only_agrees_with_itself(self) -> None:
        self.assertFalse(checker.comparable("3", "6"))
        self.assertFalse(checker.comparable("merged", "per-command"))
        self.assertFalse(checker.comparable("9ms", "9"))


class CompareTests(unittest.TestCase):
    def documented(self, row: str = ROW) -> str:
        return f"{HEADER}\n{row}\n"

    def test_a_clock_that_moved_is_not_drift(self) -> None:
        produced = self.documented(ROW.replace("9ms", "23ms"))
        self.assertEqual(checker.compare(self.documented(), produced, "matrix", 1), [])

    def test_a_changed_count_is_reported_with_both_lines(self) -> None:
        produced = self.documented(ROW.replace("3   ", "6   "))
        problems = checker.compare(self.documented(), produced, "matrix", 10)
        self.assertEqual(len(problems), 1)
        self.assertIn("README:11", problems[0])
        self.assertIn("where the block shows", problems[0])

    def test_a_line_the_example_gained_is_reported(self) -> None:
        produced = self.documented() + "dispatches ranged 3..6\n"
        problems = checker.compare(self.documented(), produced, "matrix", 1)
        self.assertTrue(any("printed 3 lines" in problem for problem in problems))

    def test_an_empty_block_is_drift(self) -> None:
        problems = checker.compare("\n", self.documented(), "matrix", 4)
        self.assertEqual(len(problems), 1)
        self.assertIn("empty", problems[0])


if __name__ == "__main__":
    unittest.main()
