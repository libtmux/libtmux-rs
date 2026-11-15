"""Each way a tarball can go wrong is reported, and a sound one is not."""

from __future__ import annotations

from pathlib import Path

import check_package_contents as check

VERSIONS = {
    "libtmux": "0.1.2",
    "libtmux-macros": "0.1.2",
    "tmux-mcp": "0.2.0",
    "tmux-workspace": "0.1.2",
}


def tree(root: Path, files: dict[str, str]) -> list[str]:
    for name, text in files.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")
    return sorted(files)


def test_a_document_naming_the_current_version_passes(tmp_path: Path) -> None:
    listing = tree(tmp_path, {"README.md": 'libtmux = "0.1.2"\ntmux-mcp = "0.2.0"\n'})
    assert check.check_versions(tmp_path, "libtmux", listing, VERSIONS) == []


def test_a_stale_version_names_the_document_and_the_crate(tmp_path: Path) -> None:
    listing = tree(tmp_path, {"README.md": "cargo add libtmux@0.1.1\n"})
    (problem,) = check.check_versions(tmp_path, "libtmux", listing, VERSIONS)
    assert problem == (
        "libtmux ships README.md, which tells the reader to depend on libtmux 0.1.1, not 0.1.2"
    )


def test_each_crate_is_held_to_its_own_version(tmp_path: Path) -> None:
    text = 'libtmux = "0.1.2"\ncargo install tmux-mcp \\\n  --version 0.1.2\n'
    listing = tree(tmp_path, {"README.md": text})
    (problem,) = check.check_versions(tmp_path, "tmux-mcp", listing, VERSIONS)
    assert "tmux-mcp 0.1.2, not 0.2.0" in problem


def test_a_link_to_a_file_that_ships_passes(tmp_path: Path) -> None:
    listing = tree(tmp_path, {"README.md": "[d](docs/design.md#why)\n", "docs/design.md": "x\n"})
    assert check.check_links(tmp_path, "libtmux", listing) == []


def test_a_link_to_a_file_left_out_of_the_package_is_named(tmp_path: Path) -> None:
    tree(tmp_path, {"README.md": "[d](docs/design.md)\n", "docs/design.md": "x\n"})
    (problem,) = check.check_links(tmp_path, "libtmux", ["README.md"])
    assert problem == (
        "libtmux ships README.md, which links docs/design.md -> docs/design.md, which it does not ship"
    )


def test_a_link_is_resolved_from_its_own_directory(tmp_path: Path) -> None:
    listing = tree(tmp_path, {"docs/a.md": "[up](../README.md)\n", "README.md": "x\n"})
    assert check.check_links(tmp_path, "libtmux", listing) == []


def test_urls_anchors_and_intra_doc_paths_are_not_files(tmp_path: Path) -> None:
    text = "[a](https://example.com) [b](#top) [c](crate::command) [d](http://x)\n"
    listing = tree(tmp_path, {"README.md": text})
    assert check.check_links(tmp_path, "libtmux", listing) == []


def test_a_required_file_missing_from_the_package_is_named(tmp_path: Path) -> None:
    tree(tmp_path, {"CHANGELOG.md": "x\n", "examples/a.rs": "x\n", "examples/b.rs": "x\n"})
    problems = check.check_required(
        tmp_path, "libtmux", ["CHANGELOG.md", "examples/a.rs"], ["CHANGELOG.md", "examples/*.rs"]
    )
    assert problems == ["libtmux package is missing examples/b.rs"]


def test_a_required_literal_absent_from_the_tree_is_named(tmp_path: Path) -> None:
    tree(tmp_path, {"CHANGELOG.md": "x\n"})
    problems = check.check_required(
        tmp_path, "libtmux", ["CHANGELOG.md"], ["CHANGELOG.md", "README.md"]
    )
    assert problems == ["libtmux: README.md is missing from the working tree"]


def test_patterns_that_match_nothing_do_not_make_the_check_vacuous(tmp_path: Path) -> None:
    problems = check.check_required(tmp_path, "libtmux", [], ["examples/*.rs"])
    assert problems == ["libtmux: expected packaged files are missing from the working tree"]


def test_a_shipped_sandbox_test_is_named() -> None:
    listing = ["src/lib.rs", "tests/sandbox_runner.rs", "tests/sandbox_canary.rs"]
    (problem,) = check.check_sandbox_tests(listing)
    assert "tests/sandbox_runner.rs" in problem
    assert check.check_sandbox_tests(["src/lib.rs", "tests/server.rs"]) == []


def test_the_required_lists_name_files_this_tree_has() -> None:
    for crate, directory in check.CRATES.items():
        present, absent = check.required_files(check.ROOT / directory, check.REQUIRED[crate])
        assert present, crate
        assert absent == [], crate
