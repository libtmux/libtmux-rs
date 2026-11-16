"""Tests for the changelog check.

The first test runs the check on this repository. Every other test breaks a
copy of it one way and expects the check to say so, because a check that cannot
fail is not checking anything.
"""

from __future__ import annotations

import pathlib
import shutil

import check_changelogs
import pytest

ROOT = pathlib.Path(__file__).resolve().parents[3]

CRATES = ("libtmux", "libtmux-macros", "tmux-mcp", "tmux-workspace")


@pytest.fixture
def root(tmp_path: pathlib.Path) -> pathlib.Path:
    """Copy the files the check reads."""
    shutil.copy(ROOT / "CHANGELOG.md", tmp_path / "CHANGELOG.md")
    for crate in CRATES:
        target = tmp_path / "crates" / crate
        target.mkdir(parents=True)
        for name in ("Cargo.toml", "README.md", "CHANGELOG.md"):
            shutil.copy(ROOT / "crates" / crate / name, target / name)
    return tmp_path


def edit(root: pathlib.Path, crate: str, name: str, old: str, new: str) -> None:
    """Replace `old` with `new` in a copied file, which must contain it."""
    path = root / "crates" / crate / name
    text = path.read_text()
    assert old in text
    path.write_text(text.replace(old, new, 1))


def problems(root: pathlib.Path) -> list[str]:
    """Return what the check says about the copy."""
    return check_changelogs.check(root)


def test_the_repository_keeps_a_changelog_in_every_crate(root: pathlib.Path) -> None:
    """The crates in this checkout pass, and all four are found."""
    assert sorted(check_changelogs.published_crates(ROOT)) == sorted(CRATES)
    assert check_changelogs.check(ROOT) == []
    assert problems(root) == []


def test_a_crate_without_a_changelog_is_flagged(root: pathlib.Path) -> None:
    """Deleting a crate's changelog names the crate."""
    (root / "crates/tmux-mcp/CHANGELOG.md").unlink()
    assert problems(root) == ["crates/tmux-mcp/CHANGELOG.md is missing; tmux-mcp is published"]


def test_another_crates_changelog_is_flagged(root: pathlib.Path) -> None:
    """A changelog pasted over another crate's fails where it was pasted."""
    shutil.copy(ROOT / "crates/libtmux/CHANGELOG.md", root / "crates/tmux-mcp/CHANGELOG.md")
    assert problems(root) == [
        'crates/tmux-mcp/CHANGELOG.md does not open with "Notable changes to `tmux-mcp`."'
    ]


def test_a_changelog_without_an_unreleased_section_is_flagged(root: pathlib.Path) -> None:
    """The next release needs a section to land under."""
    edit(root, "libtmux", "CHANGELOG.md", "\n## Unreleased\n", "\n## Next\n")
    assert problems(root) == ['crates/libtmux/CHANGELOG.md has no "## Unreleased" section']


def test_an_unreleased_heading_in_a_fence_does_not_count(root: pathlib.Path) -> None:
    """Only a heading outside a code block is a section."""
    edit(
        root, "libtmux", "CHANGELOG.md", "\n## Unreleased\n", "\n```markdown\n## Unreleased\n```\n"
    )
    assert problems(root) == ['crates/libtmux/CHANGELOG.md has no "## Unreleased" section']


def test_a_crate_that_stops_including_its_changelog_is_flagged(root: pathlib.Path) -> None:
    """`cargo package` ships what `include` lists, so the file has to be there."""
    edit(root, "tmux-workspace", "Cargo.toml", '    "/CHANGELOG.md",\n', "")
    assert problems(root) == [
        "crates/tmux-workspace/Cargo.toml does not include /CHANGELOG.md, so tmux-workspace does not ship it"
    ]


def test_a_readme_that_does_not_link_its_changelog_is_flagged(root: pathlib.Path) -> None:
    """A reader of the crate's page has to be able to find the history."""
    edit(root, "libtmux-macros", "README.md", "[`CHANGELOG.md`](CHANGELOG.md)", "`CHANGELOG.md`")
    assert problems(root) == ["crates/libtmux-macros/README.md does not link CHANGELOG.md"]


def test_a_root_changelog_without_an_unreleased_section_is_flagged(root: pathlib.Path) -> None:
    """The root file keeps the placeholder the next release lands under."""
    assert problems(root) == []
    text = (root / "CHANGELOG.md").read_text().replace("\n## Unreleased\n", "\n## Next\n", 1)
    (root / "CHANGELOG.md").write_text(text)
    assert problems(root) == ['CHANGELOG.md is missing or has no "## Unreleased" section']


def test_a_tree_with_no_published_crate_is_flagged(root: pathlib.Path) -> None:
    """A check that finds nothing to check says so rather than passing."""
    shutil.rmtree(root / "crates")
    assert problems(root) == ["crates/: no published crate found"]


def test_a_crate_marked_unpublished_needs_no_changelog(root: pathlib.Path) -> None:
    """A private tool is not held to the rule."""
    edit(root, "libtmux-macros", "Cargo.toml", "[package]\n", "[package]\npublish = false\n")
    (root / "crates/libtmux-macros/CHANGELOG.md").unlink()
    assert problems(root) == []
