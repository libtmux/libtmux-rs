//! The workspace files the READMEs show are workspaces the library reads.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use tmux_workspace::Workspace;

/// The body of every fenced `yaml` block in a Markdown document.
fn yaml_blocks(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut block: Option<String> = None;
    for line in markdown.lines() {
        match (&mut block, line.trim_start()) {
            (None, fence) if fence.starts_with("```yaml") => block = Some(String::new()),
            (Some(_), "```") => found.extend(block.take()),
            (Some(text), _) => {
                text.push_str(line);
                text.push('\n');
            }
            (None, _) => {}
        }
    }
    found
}

/// Whether the library reads a block as a workspace the way the command does:
/// an unrecognised key is refused, not recorded.
fn loads(source: &str) -> Result<usize, String> {
    Workspace::from_yaml_strict(source)
        .map(|workspace| workspace.windows.len())
        .map_err(|error| error.to_string())
}

#[test]
fn every_workspace_in_the_readmes_loads() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut documents = vec![(
        "README.md",
        std::fs::read_to_string(manifest.join("README.md")),
    )];
    // The repository's front page ships in no crate, so it is read only where
    // the repository is.
    if manifest.join("../../Cargo.toml").is_file() {
        documents.push((
            "../../README.md",
            std::fs::read_to_string(manifest.join("../../README.md")),
        ));
    }
    let mut failures = Vec::new();
    for (name, markdown) in documents {
        let blocks = yaml_blocks(&markdown.unwrap());
        assert!(!blocks.is_empty(), "{name} shows no workspace");
        for block in blocks {
            match loads(&block) {
                Ok(windows) => assert!(windows > 0, "{name} shows a workspace with no windows"),
                Err(error) => failures.push(format!("{name}: {error}\n{block}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_readme_check_finds_blocks_and_refuses_what_the_library_refuses() {
    let document = "```yaml\nsession_name: dev\nwindows:\n  - window_name: a\n    panes: [vim]\n```\n\
                    prose\n```console\n$ tmux-workspace ls\n```\n\
                    ```yaml\nsession_name: two\n```\n";
    let blocks = yaml_blocks(document);
    assert_eq!(blocks.len(), 2);
    assert_eq!(loads(&blocks[0]), Ok(1));
    // A workspace needs `windows`, and a key the library does not know is
    // refused rather than ignored.
    assert!(loads(&blocks[1]).is_err());
    assert!(loads("session_name: dev\nwindows: []\nno_such_key: 1\n").is_err());
}
