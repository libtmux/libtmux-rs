//! Methods whose cancellation leaves something to know keep saying so.

// Helpers outside a test function are not covered by clippy.toml's in-test
// exemptions, and this file has them.
#![allow(clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Blocking effects, reads, and fixture cleanup whose contracts must stay
/// explicit, as (file, impl type, method). Async drop safety is covered in
/// the native client's corresponding gate.
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

fn required_heading(owner: &str, method: &str) -> &'static str {
    match (owner, method) {
        ("ControlEvents" | "ControlMode", "next_event") | ("PaneOutput", "next_chunk") => {
            "# Blocking"
        }
        ("TestServer", "shutdown") | ("TestServerBuilder", "start") => "# Cleanup",
        _ => "# Cancellation",
    }
}

#[test]
fn listed_methods_keep_their_cancel_safety_section() {
    let sources = walk(Path::new("src"))
        .into_iter()
        .map(|path| {
            let source = std::fs::read_to_string(&path).expect("a readable source file");
            (path.to_string_lossy().replace('\\', "/"), source)
        })
        .collect::<Vec<_>>();
    let problems = contract_problems(&sources, MUST_CARRY);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

fn contract_problems(sources: &[(String, String)], required: &[(&str, &str, &str)]) -> Vec<String> {
    let listed = required
        .iter()
        .map(|(file, owner, method)| {
            (
                (
                    (*file).to_owned(),
                    (*owner).to_owned(),
                    (*method).to_owned(),
                ),
                required_heading(owner, method),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut found = BTreeMap::new();
    let mut carrying = BTreeSet::new();
    for (file, source) in sources {
        for (owner, method, heading) in public_fns(source) {
            let key = (file.clone(), owner, method);
            if heading.is_some() {
                carrying.insert(key.clone());
            }
            found.insert(key, heading);
        }
    }
    let mut problems = Vec::new();
    for (key, expected) in &listed {
        let (file, owner, method) = key;
        match found.get(key) {
            None => problems.push(format!(
                "{file}: `{owner}::{method}` is listed but not found"
            )),
            Some(Some(actual)) if actual == expected => {}
            _ => problems.push(format!(
                "{file}: `{owner}::{method}` needs its `{expected}` contract"
            )),
        }
    }
    for (file, owner, method) in carrying.iter().filter(|key| !listed.contains_key(*key)) {
        problems.push(format!(
            "{file}: `{owner}::{method}` has an unlisted blocking contract"
        ));
    }
    problems
}

#[test]
fn removing_or_misclassifying_a_blocking_contract_fails() {
    let required = [("src/control.rs", "ControlMode", "send")];
    let source = "impl ControlMode {\n    /// # Cancellation\n    /// A queued command is refused.\n    pub fn send() {}\n}\n";
    let check =
        |source: String| contract_problems(&[("src/control.rs".to_owned(), source)], &required);
    assert!(check(source.to_owned()).is_empty());
    assert_eq!(
        check(source.replace("    /// # Cancellation\n", "")).len(),
        1
    );
    assert_eq!(
        check(source.replace("# Cancellation", "# Cancel safety")).len(),
        1
    );
    assert_eq!(
        check(source.replace("# Cancellation", "# Blocking")).len(),
        1
    );
    assert_eq!(
        check(source.replace("pub fn send", "pub fn renamed")).len(),
        2
    );
}

/// Each `pub fn` in `source` as (impl type, method, has the heading).
fn public_fns(source: &str) -> Vec<(String, String, Option<&'static str>)> {
    let mut found = Vec::new();
    let mut owner = String::new();
    let mut in_doc = false;
    let mut heading = None;
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
                heading = None;
            }
            in_doc = true;
            heading = match code.trim_end() {
                "/// # Cancellation" => Some("# Cancellation"),
                "/// # Blocking" => Some("# Blocking"),
                "/// # Cleanup" => Some("# Cleanup"),
                "/// # Cancel safety" => Some("# Cancel safety"),
                _ => heading,
            };
            continue;
        }
        if in_doc && code.starts_with("#[") {
            in_attribute = !code.ends_with(']');
            continue;
        }
        if let Some(rest) = code.strip_prefix("pub fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            found.push((owner.clone(), name, if in_doc { heading } else { None }));
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
