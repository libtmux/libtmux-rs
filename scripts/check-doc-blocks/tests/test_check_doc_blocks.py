"""Each split a doc comment can suffer is reported, and clean source is not."""

from __future__ import annotations

from pathlib import Path

import check_doc_blocks
import pytest


def report(tmp_path: Path, source: str) -> list[str]:
    path = tmp_path / "lib.rs"
    path.write_text(source, encoding="utf-8")
    return check_doc_blocks.offenders(path)


def test_clean_source_passes(tmp_path: Path) -> None:
    source = (
        "/// Returns the name.\n"
        "///\n"
        "/// The second paragraph may say more. It may have two sentences.\n"
        "#[must_use]\n"
        "pub fn name() {}\n"
        "\n"
        "/// `tmux` is a proper noun and a code span opens a summary.\n"
        "pub fn other() {}\n"
    )
    assert report(tmp_path, source) == []


@pytest.mark.parametrize(
    ("source", "message"),
    [
        ("/// and then the rest of a sentence.\npub fn a() {}\n", "opens mid-sentence"),
        ("#[must_use]\n/// Returns it.\npub fn a() {}\n", "sits below an attribute"),
        (
            "/// Returns the value.\n/// Also does more.\npub fn a() {}\n",
            "two sentences in one summary",
        ),
    ],
)
def test_each_kind_of_split_is_reported(tmp_path: Path, source: str, message: str) -> None:
    (problem,) = report(tmp_path, source)
    assert message in problem


def test_main_fails_on_a_split_and_passes_without_one(tmp_path: Path) -> None:
    clean = tmp_path / "clean.rs"
    clean.write_text("/// Fine.\npub fn a() {}\n", encoding="utf-8")
    assert check_doc_blocks.main([str(clean)]) == 0
    split = tmp_path / "split.rs"
    split.write_text("/// and a fragment.\npub fn a() {}\n", encoding="utf-8")
    assert check_doc_blocks.main([str(split)]) == 1
