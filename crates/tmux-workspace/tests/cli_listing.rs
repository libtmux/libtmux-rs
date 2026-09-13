//! Human listing structure and machine record preservation.
#![cfg(feature = "cli")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

struct Listing {
    root: tempfile::TempDir,
    _namespace: tempfile::TempDir,
}

impl Listing {
    fn new() -> Self {
        std::fs::create_dir_all("/tmp/libtmux-rs-test").unwrap();
        let namespace = tempfile::tempdir_in("/tmp/libtmux-rs-test").unwrap();
        let physical = namespace.path().join("physical");
        std::fs::create_dir(&physical).unwrap();
        #[cfg(unix)]
        let parent = {
            let alias = namespace.path().join("alias");
            std::os::unix::fs::symlink("physical", &alias).unwrap();
            alias
        };
        #[cfg(not(unix))]
        let parent = physical;
        let root = tempfile::tempdir_in(parent).unwrap();
        let fixture = Self {
            root,
            _namespace: namespace,
        };
        for file in [
            "project/child/.tmuxp.json",
            "project/.tmuxp.json",
            "global/alpha.json",
            "global/beta.json",
            "xdg/tmuxp/inactive.json",
            ".tmuxp/legacy.json",
        ] {
            fixture.write(file);
        }
        fixture
    }

    fn write(&self, file: &str) {
        let path = self.root.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            json!({"session_name":"listed-session", "windows":[{"panes":[]}]}).to_string(),
        )
        .unwrap();
    }

    fn run(&self, arguments: &[&str]) -> String {
        let root = self.root.path().canonicalize().unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_tmux-workspace"))
            .args(arguments)
            .current_dir(root.join("project/child"))
            .env_clear()
            .env("HOME", &root)
            .env("TMUXP_CONFIGDIR", root.join("global"))
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .env("PATH", "/nonexistent")
            .env("TERM", "dumb")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    }
}

#[test]
fn tree_groups_discovered_workspaces_by_directory() {
    let fixture = Listing::new();
    let tree = fixture.run(&["--color", "never", "ls", "--tree"]);
    let headings = ["~/project/child", "~/project", "~/global"];
    let mut previous = 0;
    for (index, heading) in headings.iter().enumerate() {
        let position = tree
            .lines()
            .position(|line| line.trim_end() == *heading)
            .unwrap_or_else(|| panic!("missing directory group {heading:?}: {tree:?}"));
        assert!(index == 0 || position > previous, "{tree:?}");
        previous = position;
    }
    let rows: Vec<_> = tree.lines().filter(|line| line.contains(".json")).collect();
    assert_eq!(rows.len(), 4, "{tree:?}");
    for (row, path) in rows.iter().zip([
        "~/project/child/.tmuxp.json",
        "~/project/.tmuxp.json",
        "~/global/alpha.json",
        "~/global/beta.json",
    ]) {
        assert!(row.ends_with(path), "{tree:?}");
        assert!(row.contains("──"), "workspace lacks a tree branch: {row:?}");
    }
    assert!(!tree.contains("inactive.json") && !tree.contains("legacy.json"));
}

#[test]
fn tree_preserves_full_content_and_machine_records() {
    let fixture = Listing::new();
    for mode in ["--json", "--ndjson"] {
        for full in [false, true] {
            let mut args = vec![mode, "ls"];
            if full {
                args.push("--full");
            }
            let flat = fixture.run(&args);
            args.push("--tree");
            assert_eq!(fixture.run(&args), flat);
            let records: Vec<Value> = if mode == "--json" {
                let document: Value = serde_json::from_str(&flat).unwrap();
                assert_eq!(
                    document["global_workspace_dirs"].as_array().unwrap().len(),
                    3
                );
                document["workspaces"].as_array().unwrap().clone()
            } else {
                flat.lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect()
            };
            assert_eq!(records.len(), 4);
            for record in records {
                assert_eq!(record.get("config").is_some(), full);
                if full {
                    assert_eq!(record["config"]["session_name"], "listed-session");
                }
            }
        }
    }
    for tree in [false, true] {
        let mut args = vec!["--color", "never", "ls", "--full"];
        if tree {
            args.push("--tree");
        }
        assert_eq!(
            fixture
                .run(&args)
                .matches("session_name: listed-session")
                .count(),
            4
        );
    }
}

#[cfg(unix)]
#[test]
fn human_listing_escapes_controls_and_preserves_unicode_names() {
    let fixture = Listing::new();
    let name = "安全\u{1b}]8;;bad\u{7}\r\n\t\u{9b}31m";
    fixture.write(&format!("global/{name}.json"));
    for tree in [false, true] {
        for full in [false, true] {
            let mut args = vec!["--color", "never", "ls"];
            if tree {
                args.push("--tree");
            }
            if full {
                args.push("--full");
            }
            let human = fixture.run(&args);
            assert!(human.contains("安全"), "{human:?}");
            assert!(
                human.chars().all(|c| c == '\n' || !c.is_control()),
                "untrusted terminal controls in listing: {human:?}"
            );
            assert_eq!(
                human.lines().filter(|line| line.contains("安全")).count(),
                1
            );
            assert!(
                human.contains("\\u{1b}") && human.contains("\\u{9b}"),
                "{human:?}"
            );
        }
    }
    let machine: Value = serde_json::from_str(&fixture.run(&["--json", "ls", "--tree"])).unwrap();
    let record = machine["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap();
    assert_eq!(
        record["path"],
        Path::new("~/global")
            .join(format!("{name}.json"))
            .to_str()
            .unwrap()
    );
}
