//! Client selection contract tests.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;

use mcp_swap::catalog::{Paths, known_clients, select_client_names, select_clients};

const CANONICAL: [&str; 8] = [
    "claude", "codex", "cursor", "gemini", "grok", "agy", "opencode", "pi",
];

#[test]
fn every_selector_permutation_has_canonical_order() {
    let mut permutation = CANONICAL;
    let mut counters = [0_usize; 8];
    let mut count = 0_usize;

    assert_canonical(&permutation);
    count += 1;

    let mut index = 1;
    while index < permutation.len() {
        if counters[index] < index {
            let other = if index % 2 == 0 { 0 } else { counters[index] };
            permutation.swap(other, index);
            assert_canonical(&permutation);
            count += 1;
            counters[index] += 1;
            index = 1;
        } else {
            counters[index] = 0;
            index += 1;
        }
    }

    assert_eq!(count, 40_320);
}

#[test]
fn antigravity_alias_is_normalized_before_deduplication() {
    let selected = select_client_names(&["pi", "antigravity", "agy", "claude"])
        .expect("known client selectors");

    assert_eq!(selected, ["claude", "agy", "pi"]);
}

#[test]
fn client_paths_are_derived_from_isolated_roots() {
    let paths = Paths::from_roots("/home/test", "/config", "/state").expect("absolute roots");
    let clients = known_clients(&paths);

    assert_eq!(clients.len(), 8);
    assert_eq!(clients[0].config_path, Path::new("/home/test/.claude.json"));
    assert_eq!(
        clients[5].config_path,
        Path::new("/home/test/.gemini/config/mcp_config.json")
    );
    assert_eq!(
        clients[6].config_path,
        Path::new("/config/opencode/opencode.jsonc")
    );
    assert_eq!(
        clients[7].config_path,
        Path::new("/home/test/.pi/agent/mcp.json")
    );
    assert_eq!(
        paths.lock_file(),
        Path::new("/state/libtmux-mcp-dev/swap/state.lock")
    );
    assert_eq!(
        paths.state_dir(),
        Path::new("/state/libtmux-mcp-dev/swap/rust")
    );
    assert_eq!(
        paths.state_file(),
        Path::new("/state/libtmux-mcp-dev/swap/rust/state.json")
    );
}

#[test]
fn selected_clients_use_catalog_order_and_alias_deduplication() {
    let paths = Paths::from_roots("/home/test", "/config", "/state").expect("absolute roots");
    let clients = known_clients(&paths);

    let selected =
        select_clients(&clients, &["pi", "antigravity", "agy", "claude"]).expect("known selectors");

    assert_eq!(
        selected
            .iter()
            .map(|client| client.name.as_str())
            .collect::<Vec<_>>(),
        ["claude", "agy", "pi"]
    );
}

fn assert_canonical(permutation: &[&str]) {
    let selected = select_client_names(permutation).expect("known client selectors");
    assert_eq!(selected, CANONICAL);
}
