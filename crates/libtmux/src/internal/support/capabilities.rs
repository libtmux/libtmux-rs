//! What a supported tmux daemon can do, release by release.
//!
//! One table names each optional capability, the first release that has it,
//! what libtmux does below it, and the public API that needs it. Operations
//! checked from a command's arguments read their floors and intervals from
//! this table, and the rest name the [`crate::since`] constant the entry
//! reuses, so the table and the refusals cannot disagree. The docs site reads
//! the same table through `capabilities.json` at the workspace root, which a
//! test keeps equal to it.
//!
//! A quirk is not a capability. A release-specific defect libtmux works
//! around is listed apart, as a workaround with the exact releases it covers.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "the manifest emitter reads every entry; refusals read only some"
    )
)]

use crate::since;
use crate::{ReleaseSuffix, ReleaseVersion};

/// What libtmux does where a capability is missing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Handling {
    /// Return a typed error before anything runs.
    Refuse,
    /// Take another path that gives a close answer.
    Fallback,
}

impl Handling {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::Fallback => "fallback",
        }
    }
}

/// A tmux command and the flags of it a capability depends on.
pub(crate) type Command = (&'static str, &'static [&'static str]);

/// A feature that exists from one release on.
pub(crate) struct Capability {
    /// Stable kebab-case id shared across the libtmux ports.
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) since: ReleaseVersion,
    pub(crate) handling: Handling,
    /// The public error a refusal returns.
    pub(crate) error: &'static str,
    /// Public API names, written as the generated API reference spells them.
    pub(crate) apis: &'static [&'static str],
    pub(crate) commands: &'static [Command],
}

/// A defect in exact releases that libtmux refuses.
pub(crate) struct Workaround {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) summary: &'static str,
    /// First affected release up to, not including, the first fixed one.
    pub(crate) broken_in: ReleaseVersion,
    pub(crate) fixed_in: ReleaseVersion,
    pub(crate) apis: &'static [&'static str],
    pub(crate) commands: &'static [Command],
}

/// Exactly tmux 3.7 gets `break-pane` and the characters in names wrong; 3.7a
/// fixes both.
const BREAK_BROKEN: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);
const BREAK_FIXED: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::PATCH_A);

const UNSUPPORTED: &str = "error.Error.UnsupportedCapability";

/// Dispatch of an arbitrary command: the checks run on every command a
/// caller builds, not on one named method.
const COMMAND_PATHS: &[&str] = &["command.Command", "plan.Plan.run"];

pub(crate) const CONTROL_MODE: Capability = Capability {
    id: "control-mode",
    name: "Control mode",
    summary: "Attach a library-managed control client and stream events.",
    since: since::CONTROL_ACQUISITION,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &[
        "control.ControlMode.attach",
        "control.ControlMode.attach_with_limits",
        "server.Server.over_control_mode",
        "plan.Plan.run_over_control_mode",
    ],
    commands: &[("attach-session", &[])],
};

pub(crate) const CONTROL_PANE_OFF: Capability = Capability {
    id: "control-pane-off",
    name: "Silence a pane in a control client",
    summary: "mute_pane uses refresh-client -A off; earlier releases pause the pane instead.",
    since: since::CONTROL_PANE_OFF,
    handling: Handling::Fallback,
    error: UNSUPPORTED,
    apis: &["control.ControlSender.mute_pane"],
    commands: &[("refresh-client", &["-A"])],
};

pub(crate) const BUFFER_DELETE_NAMED: Capability = Capability {
    id: "buffer-delete-named",
    name: "Named buffer deletion",
    summary: "Delete one paste buffer by name without risking another buffer.",
    since: since::NAMED_BUFFER_IDENTITY,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &["server.Server.delete_buffer"],
    commands: &[("delete-buffer", &["-b"])],
};

pub(crate) const BUFFER_RENAME: Capability = Capability {
    id: "buffer-rename",
    name: "Buffer rename",
    summary: "Rename a paste buffer without risking another buffer.",
    since: since::NAMED_BUFFER_IDENTITY,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &["server.Server.set_buffer"],
    commands: &[("set-buffer", &["-b", "-n"])],
};

pub(crate) const LAYOUT_WITHOUT_CHECKSUM: Capability = Capability {
    id: "layout-without-checksum",
    name: "Layouts without a checksum",
    summary: "Apply a layout string or a named layout that tmux validates before use.",
    since: ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL),
    handling: Handling::Refuse,
    error: "error.Error.InvalidLayout",
    apis: &["window.Window.select_layout"],
    commands: &[("select-layout", &[])],
};

pub(crate) const LAYOUT_MIRRORED_MAIN: Capability = Capability {
    id: "layout-mirrored-main",
    name: "Mirrored main layouts",
    summary: "Apply main-horizontal-mirrored and main-vertical-mirrored.",
    since: since::MIRRORED_LAYOUTS,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &[
        "window.Window.select_layout",
        "window.Layout.MainHorizontalMirrored",
        "window.Layout.MainVerticalMirrored",
    ],
    commands: &[("select-layout", &[])],
};

pub(crate) const LAYOUT_JSON: Capability = Capability {
    id: "layout-json",
    name: "JSON layout strings",
    summary: "Apply a layout saved in the JSON form tmux reports.",
    since: since::JSON_LAYOUTS,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &["window.Window.select_layout", "window.LayoutSpec.Saved"],
    commands: &[("select-layout", &[])],
};

pub(crate) const CAPTURE_TRIM_BLANK_CELLS: Capability = Capability {
    id: "capture-trim-blank-cells",
    name: "Capture without trailing blank cells",
    summary: "Stop a capture at the last used cell instead of the pane width.",
    since: since::CAPTURE_TRIM_BLANK_CELLS,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &[
        "pane.CaptureOptions.trim_blank_cells",
        "pane.Pane.capture_with",
    ],
    commands: &[("capture-pane", &["-T"])],
};

pub(crate) const CAPTURE_LINE_FLAGS: Capability = Capability {
    id: "capture-line-flags",
    name: "Capture with line flags",
    summary: "Read each captured row with its wrapped and cursor flags.",
    since: since::CAPTURE_LINE_FLAGS,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &["pane.Pane.capture_lines"],
    commands: &[("capture-pane", &["-F"])],
};

pub(crate) const LIST_KEYS_FORMAT: Capability = Capability {
    id: "list-keys-format",
    name: "Typed key bindings",
    summary: "Read key bindings through list-keys -F.",
    since: since::LIST_KEYS_FORMAT,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &["server.Server.typed_key_bindings"],
    commands: &[("list-keys", &["-F"])],
};

pub(crate) const PANE_BORDER_STYLE_PER_PANE: Capability = Capability {
    id: "pane-border-style-per-pane",
    name: "Per-pane border styles",
    summary: "Set pane-border-style and pane-active-border-style on one pane.",
    since: since::PANE_BORDER_STYLE_PER_PANE,
    handling: Handling::Refuse,
    error: UNSUPPORTED,
    apis: &["options.OptionSchema.accepts"],
    commands: &[("set-option", &["-p"])],
};

pub(crate) const CAPABILITIES: &[&Capability] = &[
    &CONTROL_MODE,
    &CONTROL_PANE_OFF,
    &BUFFER_DELETE_NAMED,
    &BUFFER_RENAME,
    &LAYOUT_WITHOUT_CHECKSUM,
    &LAYOUT_MIRRORED_MAIN,
    &LAYOUT_JSON,
    &CAPTURE_TRIM_BLANK_CELLS,
    &CAPTURE_LINE_FLAGS,
    &LIST_KEYS_FORMAT,
    &PANE_BORDER_STYLE_PER_PANE,
];

pub(crate) const BREAK_PANE_MISNAMES_WINDOWS: Workaround = Workaround {
    id: "break-pane-misnames-windows",
    name: "break-pane window naming",
    summary: "break-pane gets the new window wrong on exactly tmux 3.7; libtmux refuses it.",
    broken_in: BREAK_BROKEN,
    fixed_in: BREAK_FIXED,
    apis: &["pane.Pane.break_out"],
    commands: &[("break-pane", &[])],
};

pub(crate) const SPLIT_WINDOW_PERCENT_SIZE: Workaround = Workaround {
    id: "split-window-percent-size",
    name: "split-window -p",
    summary: "split-window -p is broken on tmux 3.4; use -l N%.",
    broken_in: ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL),
    fixed_in: ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL),
    apis: COMMAND_PATHS,
    commands: &[("split-window", &["-p"])],
};

pub(crate) const NAME_SEPARATORS_REJECTED: Workaround = Workaround {
    id: "name-separators-rejected",
    name: "Colon and dot in names",
    summary: "tmux 3.7 rejects ':' and '.' in session and window names after partly running.",
    broken_in: BREAK_BROKEN,
    fixed_in: BREAK_FIXED,
    apis: &[
        "server.Server.new_session",
        "session.Session.new_window",
        "session.Session.rename",
        "window.Window.rename",
    ],
    commands: &[
        ("new-session", &["-s"]),
        ("new-window", &["-n"]),
        ("rename-session", &[]),
        ("rename-window", &[]),
    ],
};

pub(crate) const RUN_SHELL_OUTPUT_IN_COPY_MODE: Workaround = Workaround {
    id: "run-shell-output-in-copy-mode",
    name: "run-shell output capture",
    summary: "tmux 3.3 through 3.4 send run-shell output to a pane's copy-mode buffer and exit zero; libtmux refuses it.",
    broken_in: ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL),
    fixed_in: ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL),
    apis: &["server.Server.run_shell"],
    commands: &[("run-shell", &[])],
};

pub(crate) const WORKAROUNDS: &[&Workaround] = &[
    &RUN_SHELL_OUTPUT_IN_COPY_MODE,
    &BREAK_PANE_MISNAMES_WINDOWS,
    &SPLIT_WINDOW_PERCENT_SIZE,
    &NAME_SEPARATORS_REJECTED,
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::{CAPABILITIES, Command, WORKAROUNDS};
    use crate::ReleaseVersion;
    use crate::TmuxVersion;

    /// Releases tmux has shipped from the floor up. A workaround's exact
    /// releases are read off the interval its refusal uses.
    const SHIPPED: &[&str] = &[
        "3.3a", "3.4", "3.5", "3.5a", "3.6", "3.6a", "3.6b", "3.7", "3.7a", "3.7b", "3.7c", "3.7d",
    ];

    fn releases_between(broken_in: ReleaseVersion, fixed_in: ReleaseVersion) -> Vec<String> {
        SHIPPED
            .iter()
            .filter(|raw| {
                TmuxVersion::parse_output(format!("tmux {raw}\n").as_bytes())
                    .ok()
                    .and_then(|version| version.behavior_release())
                    .is_some_and(|release| release >= broken_in && release < fixed_in)
            })
            .map(|raw| (*raw).to_owned())
            .collect()
    }

    fn commands(list: &[Command]) -> Vec<Value> {
        list.iter()
            .map(|(command, flags)| {
                if flags.is_empty() {
                    json!({ "command": command })
                } else {
                    json!({ "command": command, "flags": flags })
                }
            })
            .collect()
    }

    /// The manifest the docs site reads, rendered from the tables alone.
    fn manifest() -> String {
        let capabilities: Vec<Value> = CAPABILITIES
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "name": entry.name,
                    "summary": entry.summary,
                    "since": entry.since.to_string(),
                    "handling": entry.handling.as_str(),
                    "error": entry.error,
                    "apis": entry.apis,
                    "commands": commands(entry.commands),
                })
            })
            .collect();
        let workarounds: Vec<Value> = WORKAROUNDS
            .iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "name": entry.name,
                    "summary": entry.summary,
                    "releases": releases_between(entry.broken_in, entry.fixed_in),
                    "handling": "refuse",
                    "apis": entry.apis,
                    "commands": commands(entry.commands),
                })
            })
            .collect();
        let document = json!({
            "schema": "libtmux-capabilities/1",
            "port": "rs",
            "floor": { "minimum": TmuxVersion::MIN_SUPPORTED.to_string() },
            "capabilities": capabilities,
            "workarounds": workarounds,
        });
        format!("{document:#}\n")
    }

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// `LIBTMUX_EMIT_CAPABILITIES=1 cargo test -p libtmux --lib capabilities`
    /// rewrites the committed manifest from the tables.
    #[test]
    fn the_committed_manifest_is_the_registry() {
        let path = workspace_root().join("capabilities.json");
        let expected = manifest();
        if std::env::var_os("LIBTMUX_EMIT_CAPABILITIES").is_some() {
            std::fs::write(&path, &expected).expect("write capabilities.json");
            return;
        }
        let Ok(committed) = std::fs::read_to_string(&path) else {
            // A crate unpacked from its published archive has no workspace
            // root to hold the manifest; a checkout always does.
            assert!(
                !workspace_root().join("Cargo.lock").exists(),
                "capabilities.json is missing; run with LIBTMUX_EMIT_CAPABILITIES=1"
            );
            return;
        };
        assert_eq!(
            committed, expected,
            "capabilities.json drifted from the registry; run with LIBTMUX_EMIT_CAPABILITIES=1"
        );
    }

    #[test]
    fn ids_are_unique_and_workaround_releases_exist() {
        let mut seen = BTreeSet::new();
        for id in CAPABILITIES
            .iter()
            .map(|entry| entry.id)
            .chain(WORKAROUNDS.iter().map(|entry| entry.id))
        {
            assert!(seen.insert(id), "{id} appears twice");
        }
        for entry in WORKAROUNDS {
            assert!(
                !releases_between(entry.broken_in, entry.fixed_in).is_empty(),
                "{} covers no release",
                entry.id
            );
        }
    }

    /// Every `module.Type[.member]` name resolves inside the source: the type
    /// is declared in that module, and a member is a method of one of its
    /// `impl` blocks or a variant of its enum. A rename that lands on another
    /// type's method therefore fails. The docs build resolves the same paths
    /// against the generated reference.
    #[test]
    fn api_names_resolve_to_a_type_and_member() {
        fn walk(dir: &Path, text: &mut String) {
            for entry in std::fs::read_dir(dir).expect("read src") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    walk(&path, text);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    text.push_str(&std::fs::read_to_string(&path).expect("read source"));
                }
            }
        }
        let is_word = |text: &str, at: usize, len: usize| {
            let boundary = |byte: Option<&u8>| {
                byte.is_none_or(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_'))
            };
            boundary(
                at.checked_sub(1)
                    .and_then(|before| text.as_bytes().get(before)),
            ) && boundary(text.as_bytes().get(at + len))
        };
        // The brace-matched bodies of every `impl` block and type declaration
        // that names `ty` in its header line.
        let blocks = |text: &str, ty: &str| -> Vec<String> {
            let mut found = Vec::new();
            for (at, _) in text.match_indices(ty) {
                if !is_word(text, at, ty.len()) {
                    continue;
                }
                let start = text[..at].rfind('\n').map_or(0, |newline| newline + 1);
                let header = text[start..at].trim_start();
                let declares = ["impl", "enum ", "struct ", "pub enum ", "pub struct "]
                    .iter()
                    .chain(&["pub(crate) enum ", "pub(crate) struct "])
                    .any(|keyword| header.starts_with(keyword));
                let Some(open) = text[start..].find('{').map(|offset| start + offset) else {
                    continue;
                };
                if !declares || text[start..open].contains(';') {
                    continue;
                }
                let mut depth = 0_usize;
                for (offset, byte) in text[open..].bytes().enumerate() {
                    match byte {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                found.push(text[open..=open + offset].to_owned());
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            found
        };
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in CAPABILITIES
            .iter()
            .flat_map(|entry| entry.apis.iter().copied().chain([entry.error]))
            .chain(
                WORKAROUNDS
                    .iter()
                    .flat_map(|entry| entry.apis.iter().copied()),
            )
        {
            let mut parts = entry.split('.');
            let (module, ty, member) = (
                parts.next().expect("a module"),
                parts.next().expect("a type"),
                parts.next(),
            );
            let mut text = String::new();
            if let Ok(file) = std::fs::read_to_string(src.join(format!("{module}.rs"))) {
                text.push_str(&file);
            }
            if src.join(module).is_dir() {
                walk(&src.join(module), &mut text);
            }
            let found = blocks(&text, ty);
            assert!(
                !found.is_empty(),
                "{entry}: {ty} is not declared in {module}"
            );
            if let Some(member) = member {
                let method = format!("fn {member}");
                let has_member = found.iter().any(|body| {
                    body.match_indices(&method)
                        .any(|(at, _)| is_word(body, at + 3, member.len()))
                        || body.lines().any(|line| {
                            line.trim_start().strip_prefix(member).is_some_and(|rest| {
                                rest.bytes().next().is_none_or(|byte| {
                                    !(byte.is_ascii_alphanumeric() || byte == b'_')
                                })
                            })
                        })
                });
                assert!(has_member, "{entry}: {ty} has no `{member}`");
            }
        }
    }
}
