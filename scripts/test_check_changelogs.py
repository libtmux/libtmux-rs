"""Tests for the changelog check.

The first test runs the check on this repository. Every other test breaks a
copy of it one way and expects the check to say so, because a check that cannot
fail is not checking anything.
"""

from __future__ import annotations

import importlib.util
import pathlib
import shutil
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("check_changelogs", ROOT / "scripts" / "check-changelogs.py")
assert SPEC is not None and SPEC.loader is not None
check_changelogs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check_changelogs)

CRATES = ("libtmux", "libtmux-macros", "tmux-mcp", "tmux-workspace")


class CheckChangelogsTest(unittest.TestCase):
    """Break a copy of the crates and watch the check notice."""

    def setUp(self) -> None:
        """Copy the files the check reads."""
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        shutil.copy(ROOT / "CHANGELOG.md", self.root / "CHANGELOG.md")
        for crate in CRATES:
            target = self.root / "crates" / crate
            target.mkdir(parents=True)
            for name in ("Cargo.toml", "README.md", "CHANGELOG.md"):
                shutil.copy(ROOT / "crates" / crate / name, target / name)

    def edit(self, crate: str, name: str, old: str, new: str) -> None:
        """Replace `old` with `new` in a copied file, which must contain it."""
        path = self.root / "crates" / crate / name
        text = path.read_text()
        self.assertIn(old, text)
        path.write_text(text.replace(old, new, 1))

    def problems(self) -> list[str]:
        """Return what the check says about the copy."""
        return check_changelogs.check(self.root)

    def test_the_repository_keeps_a_changelog_in_every_crate(self) -> None:
        """The crates in this checkout pass, and all four are found."""
        self.assertEqual(sorted(check_changelogs.published_crates(ROOT)), sorted(CRATES))
        self.assertEqual(check_changelogs.check(ROOT), [])
        self.assertEqual(self.problems(), [])

    def test_a_crate_without_a_changelog_is_flagged(self) -> None:
        """Deleting a crate's changelog names the crate."""
        (self.root / "crates/tmux-mcp/CHANGELOG.md").unlink()
        self.assertEqual(
            self.problems(),
            ["crates/tmux-mcp/CHANGELOG.md is missing; tmux-mcp is published"],
        )

    def test_another_crates_changelog_is_flagged(self) -> None:
        """A changelog pasted over another crate's fails where it was pasted."""
        shutil.copy(ROOT / "crates/libtmux/CHANGELOG.md", self.root / "crates/tmux-mcp/CHANGELOG.md")
        self.assertEqual(
            self.problems(),
            ['crates/tmux-mcp/CHANGELOG.md does not open with "Notable changes to `tmux-mcp`."'],
        )

    def test_a_changelog_without_an_unreleased_section_is_flagged(self) -> None:
        """The next release needs a section to land under."""
        self.edit("libtmux", "CHANGELOG.md", "\n## Unreleased\n", "\n## Next\n")
        self.assertEqual(
            self.problems(),
            ['crates/libtmux/CHANGELOG.md has no "## Unreleased" section'],
        )

    def test_an_unreleased_heading_in_a_fence_does_not_count(self) -> None:
        """Only a heading outside a code block is a section."""
        self.edit("libtmux", "CHANGELOG.md", "\n## Unreleased\n", "\n```markdown\n## Unreleased\n```\n")
        self.assertEqual(
            self.problems(),
            ['crates/libtmux/CHANGELOG.md has no "## Unreleased" section'],
        )

    def test_a_crate_that_stops_including_its_changelog_is_flagged(self) -> None:
        """`cargo package` ships what `include` lists, so the file has to be there."""
        self.edit("tmux-workspace", "Cargo.toml", '    "/CHANGELOG.md",\n', "")
        self.assertEqual(
            self.problems(),
            ["crates/tmux-workspace/Cargo.toml does not include /CHANGELOG.md, so tmux-workspace does not ship it"],
        )

    def test_a_readme_that_does_not_link_its_changelog_is_flagged(self) -> None:
        """A reader of the crate's page has to be able to find the history."""
        self.edit("libtmux-macros", "README.md", "[`CHANGELOG.md`](CHANGELOG.md)", "`CHANGELOG.md`")
        self.assertEqual(
            self.problems(),
            ["crates/libtmux-macros/README.md does not link CHANGELOG.md"],
        )

    def test_a_root_changelog_without_an_unreleased_section_is_flagged(self) -> None:
        """The root file keeps the placeholder the next release lands under."""
        self.assertEqual(self.problems(), [])
        text = (self.root / "CHANGELOG.md").read_text().replace("\n## Unreleased\n", "\n## Next\n", 1)
        (self.root / "CHANGELOG.md").write_text(text)
        self.assertEqual(
            self.problems(),
            ['CHANGELOG.md is missing or has no "## Unreleased" section'],
        )

    def test_a_tree_with_no_published_crate_is_flagged(self) -> None:
        """A check that finds nothing to check says so rather than passing."""
        shutil.rmtree(self.root / "crates")
        self.assertEqual(self.problems(), ["crates/: no published crate found"])

    def test_a_crate_marked_unpublished_needs_no_changelog(self) -> None:
        """A private tool is not held to the rule."""
        self.edit("libtmux-macros", "Cargo.toml", "[package]\n", "[package]\npublish = false\n")
        (self.root / "crates/libtmux-macros/CHANGELOG.md").unlink()
        self.assertEqual(self.problems(), [])


if __name__ == "__main__":
    unittest.main()
