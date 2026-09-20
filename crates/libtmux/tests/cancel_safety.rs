//! Methods whose dropped future leaves something to know keep saying so.

// Helpers outside a test function are not covered by clippy.toml's in-test
// exemptions, and this file has them.
#![allow(clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every public `async fn` that owes a `# Cancel safety` section, as
/// (file, impl type, method).
///
/// Each one holds something, can half-happen, keeps running after a drop, or
/// is a wait or stream read a caller races in `select!`. `.github/WRITING.md`
/// says when a method belongs here.
const MUST_CARRY: &[(&str, &str, &str)] = &[
    ("src/control.rs", "ControlEvents", "next_event"),
    ("src/control.rs", "ControlMode", "attach"),
    ("src/control.rs", "ControlMode", "attach_with_limits"),
    ("src/control.rs", "ControlMode", "next_event"),
    ("src/control.rs", "ControlMode", "send"),
    ("src/control.rs", "ControlSender", "send"),
    ("src/control.rs", "ControlSender", "watch_only"),
    ("src/control.rs", "PaneOutput", "next_chunk"),
    ("src/control.rs", "PaneOutput", "snapshot"),
    ("src/pane.rs", "Pane", "send_line"),
    ("src/pane/observe.rs", "Pane", "stream_output"),
    ("src/pane/observe.rs", "Pane", "stream_output_with_limits"),
    ("src/pane/observe.rs", "Pane", "wait_for_quiet"),
    ("src/pane/observe.rs", "Pane", "wait_for_text"),
    ("src/pane/observe.rs", "Pane", "wait_until"),
    ("src/plan/run.rs", "Plan", "run"),
    ("src/plan/run.rs", "Plan", "run_over_control_mode"),
    ("src/server.rs", "Server", "run_shell"),
    ("src/server.rs", "Server", "with_session"),
    ("src/server/channels.rs", "Server", "lock_channel"),
    ("src/server/channels.rs", "Server", "wait_for_channel"),
    ("src/server/channels.rs", "Server", "with_channel_lock"),
    ("src/server/settings.rs", "Server", "set_hooks"),
    ("src/session.rs", "Session", "with_window"),
    ("src/session/settings.rs", "Session", "set_hooks"),
    ("src/test.rs", "TestServer", "shutdown"),
    ("src/test.rs", "TestServerBuilder", "start"),
    ("src/window.rs", "Window", "with_pane"),
    ("src/window/settings.rs", "Window", "set_hooks"),
];

const HEADING: &str = "/// # Cancel safety";

#[test]
fn listed_methods_keep_their_cancel_safety_section() {
    let listed: BTreeSet<(String, String, String)> = MUST_CARRY
        .iter()
        .map(|(file, owner, method)| {
            (
                (*file).to_owned(),
                (*owner).to_owned(),
                (*method).to_owned(),
            )
        })
        .collect();

    let mut found = BTreeSet::new();
    let mut carrying = BTreeSet::new();
    for path in walk(Path::new("src")) {
        let file = path.to_string_lossy().replace('\\', "/");
        let source = std::fs::read_to_string(&path).expect("a readable source file");
        for (owner, method, has_section) in public_async_fns(&source) {
            let key = (file.clone(), owner, method);
            if has_section {
                carrying.insert(key.clone());
            }
            found.insert(key);
        }
    }

    let mut problems = Vec::new();
    for key in &listed {
        let (file, owner, method) = key;
        if !found.contains(key) {
            problems.push(format!(
                "{file}: `{owner}::{method}` is listed but not found"
            ));
        } else if !carrying.contains(key) {
            problems.push(format!(
                "{file}: `{owner}::{method}` lost its `# Cancel safety` section"
            ));
        }
    }
    for (file, owner, method) in carrying.difference(&listed) {
        problems.push(format!(
            "{file}: `{owner}::{method}` has a `# Cancel safety` section but is not in \
             MUST_CARRY, so losing it would go unnoticed"
        ));
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Each `pub async fn` in `source` as (impl type, method, has the heading).
fn public_async_fns(source: &str) -> Vec<(String, String, bool)> {
    let mut found = Vec::new();
    let mut owner = String::new();
    let mut in_doc = false;
    let mut has_section = false;
    let mut in_attribute = false;

    for line in source.lines() {
        if line.starts_with("impl ") || line.starts_with("impl<") {
            owner = impl_type(line);
        } else if line == "}" {
            owner.clear();
        }
        let code = line.trim_start();
        if in_attribute {
            in_attribute = !code.ends_with(']');
            continue;
        }
        if code.starts_with("///") {
            if !in_doc {
                has_section = false;
            }
            in_doc = true;
            has_section |= code.trim_end() == HEADING;
            continue;
        }
        if in_doc && code.starts_with("#[") {
            in_attribute = !code.ends_with(']');
            continue;
        }
        if let Some(rest) = code.strip_prefix("pub async fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            found.push((owner.clone(), name, in_doc && has_section));
        }
        in_doc = false;
    }
    found
}

/// The type an `impl` line at column zero is for.
fn impl_type(line: &str) -> String {
    let mut rest = line.trim_start_matches("impl").trim_start();
    if rest.starts_with('<') {
        let mut depth = 0;
        let end = rest
            .char_indices()
            .find(|(_, c)| {
                match c {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .map_or(rest.len(), |(index, _)| index + 1);
        rest = rest[end..].trim_start();
    }
    if let Some((_, target)) = rest.split_once(" for ") {
        rest = target;
    }
    rest.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

fn walk(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let entries = std::fs::read_dir(directory).expect("a readable directory");
    for entry in entries {
        let path = entry.expect("a readable entry").path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
    found
}
