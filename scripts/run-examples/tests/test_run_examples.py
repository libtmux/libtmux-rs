"""The example gate fails when its table drifts from the workspace, and reads its rows right."""

from __future__ import annotations

from pathlib import Path

import pytest
import run_examples


def test_a_row_is_seven_fields_and_a_short_row_is_padded() -> None:
    (row,) = run_examples.parse_table("libtmux | scratch | test-support | | | hello from tmux\n")
    assert (row.crate, row.example, row.features) == ("libtmux", "scratch", "test-support")
    assert (row.arguments, row.driver, row.expect, row.needs) == ("", "", "hello from tmux", "")


def test_comments_and_blank_lines_are_dropped() -> None:
    assert run_examples.parse_table("\n  # a note\n\n") == []


def test_the_needs_column_is_a_list_of_words() -> None:
    (row,) = run_examples.parse_table("libtmux | find | query | sh | | in window | pane control\n")
    assert row.arguments.split() == ["sh"]
    assert row.needs_word("pane")
    assert row.needs_word("control")
    assert not row.needs_word("pan")


def test_an_example_with_no_row_and_a_row_with_no_example_are_both_named() -> None:
    unlisted, missing = run_examples.compare(["a|renamed"], ["a|original", "b|new"])
    assert unlisted == ["a|original", "b|new"]
    assert missing == ["a|renamed"]


def test_the_table_names_every_example_the_workspace_ships() -> None:
    rows = run_examples.parse_table(run_examples.TABLE)
    unlisted, missing = run_examples.compare(
        (row.key for row in rows), run_examples.workspace_examples()
    )
    assert (unlisted, missing) == ([], [])


@pytest.mark.parametrize(
    ("version", "qualified"),
    [
        ("3.6b", False),
        ("3.7-rc1", False),
        ("next-3.7", False),
        ("3.7", True),
        ("3.7b", True),
        ("3.8", True),
        ("next-3.9", True),
        ("garbage", False),
    ],
)
def test_control_examples_need_tmux_3_7_or_later(version: str, qualified: bool) -> None:
    assert run_examples.control_qualified(version) is qualified


def test_only_a_pane_example_is_handed_the_fixture_server(tmp_path: Path) -> None:
    fixture = run_examples.Fixture(tmp_path)
    fixture.tmux_value = "/s,1,0"
    pane, plain = run_examples.parse_table(
        "libtmux | inspect | | | | examples | pane\nlibtmux | scratch | | | | hello\n"
    )
    assert run_examples.run_environment(pane, fixture)["LIBTMUX_SANDBOX_TMUX"] == "/s,1,0"
    assert "LIBTMUX_SANDBOX_TMUX" not in run_examples.run_environment(plain, fixture)


def test_a_cargo_example_command_names_its_crate_features_and_arguments() -> None:
    (row,) = run_examples.parse_table("libtmux | find | query | sh | | in window | pane\n")
    command = run_examples.cargo_run(row)
    assert command[:3] == ["cargo", "run", "--quiet"]
    assert command[command.index("--manifest-path") + 1] == "crates/libtmux/Cargo.toml"
    assert command[command.index("--features") + 1] == "query"
    assert command[-2:] == ["--", "sh"]
