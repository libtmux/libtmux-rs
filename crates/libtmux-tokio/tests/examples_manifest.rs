//! Every shipped example has its own `[[example]]` block.
//!
//! `cargo` auto-discovers a file under `examples/` with no explicit block, so
//! one is easy to add without ever declaring it: `inspect.rs` did, and
//! `README.md`'s "six programs" (seven declared blocks, eight files) is what
//! that gap left behind. A file with no block still runs -- this is
//! about the block being the one place a later feature gate belongs, not
//! about anything failing today.

use std::collections::BTreeSet;
use std::path::Path;

#[test]
fn every_example_file_has_its_own_cargo_toml_block() {
    let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|error| panic!("{} reads: {error}", manifest_path.display()));

    let mut declared = BTreeSet::new();
    let mut lines = manifest.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "[[example]]" {
            continue;
        }
        let name = lines
            .by_ref()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .and_then(|line| line.strip_prefix("name"))
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .map_or_else(
                || panic!("an [[example]] block with no name in {manifest_path:?}"),
                |rest| rest.trim().trim_matches('"').to_owned(),
            );
        declared.insert(name);
    }

    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let on_disk: BTreeSet<String> = std::fs::read_dir(&examples_dir)
        .unwrap_or_else(|error| panic!("{} reads: {error}", examples_dir.display()))
        .map(|entry| entry.expect("directory entry reads"))
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rs"))
        .map(|entry| {
            entry
                .path()
                .file_stem()
                .expect("a .rs file has a stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    assert_eq!(
        on_disk, declared,
        "every file under examples/ needs its own [[example]] block in Cargo.toml, even with no \
         required-features, so a later feature gate is never silently missing one",
    );
}
