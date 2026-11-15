"""Each break of a Contents list is reported, and a sound one is not."""

from __future__ import annotations

from pathlib import Path

import check_contents

SOUND = """\
# Title

## Contents

- [One](#one)
- [Two words](#two-words)
  - [Inner](#inner)

## One

text

## Two words

### Inner

```text
## Not a heading
```
"""


def write(tmp_path: Path, text: str) -> Path:
    path = tmp_path / "doc.md"
    path.write_text(text, encoding="utf-8")
    return path


def test_a_matching_list_passes(tmp_path: Path) -> None:
    assert check_contents.check(write(tmp_path, SOUND)) == []


def test_a_section_missing_from_the_list_is_named(tmp_path: Path) -> None:
    text = SOUND.replace("- [One](#one)\n", "")
    (problem,) = check_contents.check(write(tmp_path, text))
    assert "missing ['One']" in problem


def test_a_stale_entry_is_named(tmp_path: Path) -> None:
    text = SOUND.replace("- [One](#one)\n", "- [One](#one)\n- [Gone](#gone)\n")
    (problem,) = check_contents.check(write(tmp_path, text))
    assert "stale ['Gone']" in problem


def test_a_link_to_the_wrong_anchor_is_named(tmp_path: Path) -> None:
    text = SOUND.replace("(#two-words)", "(#two)")
    (problem,) = check_contents.check(write(tmp_path, text))
    assert "links #two, but the section's anchor is #two-words" in problem


def test_nested_entries_must_match_the_subsections(tmp_path: Path) -> None:
    text = SOUND.replace("[Inner](#inner)", "[Other](#other)")
    problems = check_contents.check(write(tmp_path, text))
    assert any("does not match its ### subsections" in problem for problem in problems)


def test_a_document_without_a_list_is_refused(tmp_path: Path) -> None:
    (problem,) = check_contents.check(write(tmp_path, "# T\n\n## One\n"))
    assert "no `## Contents` list" in problem


def test_main_exits_nonzero_on_a_break(tmp_path: Path) -> None:
    bad = write(tmp_path, SOUND.replace("(#one)", "(#uno)"))
    assert check_contents.main([str(bad)]) == 1
    assert check_contents.main([str(write(tmp_path, SOUND))]) == 0
