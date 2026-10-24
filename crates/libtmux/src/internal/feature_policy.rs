use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;

use crate::{Error, ReleaseVersion, TmuxVersion};

use crate::internal::support::capabilities::{
    BREAK_PANE_MISNAMES_WINDOWS, BUFFER_DELETE_NAMED, BUFFER_RENAME, CAPTURE_LINE_FLAGS,
    CAPTURE_TRIM_BLANK_CELLS, CLIPBOARD_QUERY, CONTROL_PANE_OFF, LAYOUT_WITHOUT_CHECKSUM,
    NAME_SEPARATORS_REJECTED, SPLIT_WINDOW_PERCENT_SIZE,
};

struct Arguments<'a, T> {
    flags: Vec<(u8, Option<&'a [u8]>)>,
    positional: &'a [T],
}

pub(crate) fn require(command: &[impl AsRef<OsStr>], version: &TmuxVersion) -> Result<(), Error> {
    let Some((name, arguments)) = command.split_first() else {
        return Ok(());
    };
    let name = name.as_ref().as_bytes();
    if native(name, b"delete-buffer", b"deleteb", 3) {
        if flags(arguments, b"b").iter().any(|(flag, _)| *flag == b'b') {
            version.require("exact named-buffer deletion", BUFFER_DELETE_NAMED.since)?;
        }
    } else if native(name, b"set-buffer", b"setb", 5) {
        let flags = flags(arguments, b"bnt");
        if flags.iter().any(|(flag, _)| *flag == b'b')
            && flags.iter().any(|(flag, _)| *flag == b'n')
        {
            version.require("exact named-buffer rename", BUFFER_RENAME.since)?;
        }
    } else if native(name, b"break-pane", b"breakp", 2) {
        refuse_interval(
            version,
            "break-pane",
            BREAK_PANE_MISNAMES_WINDOWS.broken_in,
            BREAK_PANE_MISNAMES_WINDOWS.fixed_in,
        )?;
    } else if native(name, b"split-window", b"splitw", 2) {
        if flags(arguments, b"ceFlpt")
            .iter()
            .any(|(flag, _)| *flag == b'p')
        {
            refuse_interval(
                version,
                "split-window -p",
                SPLIT_WINDOW_PERCENT_SIZE.broken_in,
                SPLIT_WINDOW_PERCENT_SIZE.fixed_in,
            )?;
        }
    } else if native(name, b"new-session", b"new", 3) {
        let found = flags(arguments, b"cefFnstxy");
        refuse_name_characters(version, &found, b"sn")?;
    } else if native(name, b"new-window", b"neww", 5) {
        let found = flags(arguments, b"cenFt");
        refuse_name_characters(version, &found, b"n")?;
    } else if native(name, b"rename-session", b"rename", 7)
        || native(name, b"rename-window", b"renamew", 8)
    {
        let Arguments { positional, .. } = scan(arguments, b"t");
        if let Some(new_name) = positional.first() {
            refuse_name_bytes(version, new_name.as_ref().as_bytes())?;
        }
    } else if native(name, b"select-layout", b"selectl", 8) {
        if version
            .behavior_release()
            .is_some_and(|release| release < LAYOUT_WITHOUT_CHECKSUM.since)
        {
            let Arguments { flags, positional } = scan(arguments, b"t");
            if !flags.iter().any(|(flag, _)| b"Enp".contains(flag)) {
                if let Some(layout) = positional.first() {
                    let bytes = layout.as_ref().as_bytes();
                    let serialized = bytes.len() > 5
                        && bytes[4] == b','
                        && bytes[..4].iter().all(u8::is_ascii_hexdigit);
                    let preset = !flags.iter().any(|(flag, _)| *flag == b'o')
                        && crate::layout::accepts(bytes, 1, false);
                    if !serialized && !preset {
                        return Err(Error::InvalidLayout {
                            reason: "tmux 3.3a needs a unique preset or four-hex serialized layout header",
                        });
                    }
                }
            }
        }
    } else if native(name, b"refresh-client", b"refresh", 3) {
        let found = flags(arguments, b"ABCFft");
        if found.iter().any(|(flag, value)| {
            *flag == b'A' && value.is_some_and(|value| value.ends_with(b":off"))
        }) {
            version.require("refresh-client pane off", CONTROL_PANE_OFF.since)?;
        }
        if found.iter().any(|(flag, _)| *flag == b'l') {
            version.require("refresh-client -l", CLIPBOARD_QUERY.since)?;
        }
    } else if native(name, b"capture-pane", b"capturep", 2) {
        for (flag, _) in flags(arguments, b"bESt") {
            match flag {
                b'T' => {
                    version.require("capture-pane -T", CAPTURE_TRIM_BLANK_CELLS.since)?;
                }
                b'F' => {
                    version.require("capture-pane line flags", CAPTURE_LINE_FLAGS.since)?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Exactly tmux 3.7 rejects a session or window name holding `:` or `.`
/// after the command has partly run; 3.7a and every other release accept or
/// refuse it cleanly. Refused before dispatch so nothing runs.
fn refuse_name_characters(
    version: &TmuxVersion,
    found: &[(u8, Option<&[u8]>)],
    naming: &[u8],
) -> Result<(), Error> {
    for (flag, value) in found {
        if naming.contains(flag) {
            if let Some(value) = value {
                refuse_name_bytes(version, value)?;
            }
        }
    }
    Ok(())
}

fn refuse_name_bytes(version: &TmuxVersion, name: &[u8]) -> Result<(), Error> {
    if name.iter().any(|byte| matches!(byte, b':' | b'.')) {
        refuse_interval(
            version,
            "session or window name containing ':' or '.'",
            NAME_SEPARATORS_REJECTED.broken_in,
            NAME_SEPARATORS_REJECTED.fixed_in,
        )?;
    }
    Ok(())
}

pub(crate) fn refuse_interval(
    version: &TmuxVersion,
    capability: &'static str,
    broken_in: ReleaseVersion,
    fixed_in: ReleaseVersion,
) -> Result<(), Error> {
    if version
        .behavior_release()
        .is_some_and(|release| release >= broken_in && release < fixed_in)
    {
        return Err(Error::CapabilityDefective {
            capability,
            found: version.clone(),
            broken_in,
            fixed_in,
        });
    }
    Ok(())
}

fn native(name: &[u8], full: &[u8], alias: &[u8], shortest: usize) -> bool {
    name == alias || (name.len() >= shortest && full.starts_with(name))
}

// Inspect only native argv flags. Positionals, including tmux command strings
// nested in another command (`if-shell`, `run-shell -C`, brace blocks),
// retain tmux's parser; their contents never become policy input.
fn flags<'a>(arguments: &'a [impl AsRef<OsStr>], valued: &[u8]) -> Vec<(u8, Option<&'a [u8]>)> {
    scan(arguments, valued).flags
}

fn scan<'a, T: AsRef<OsStr>>(arguments: &'a [T], valued: &[u8]) -> Arguments<'a, T> {
    let mut flags = Vec::new();
    let mut position = 0;
    while let Some(argument) = arguments.get(position) {
        let bytes = argument.as_ref().as_bytes();
        if bytes == b"--" {
            position += 1;
            break;
        }
        if bytes.len() < 2 || bytes[0] != b'-' {
            break;
        }
        position += 1;
        for (index, flag) in bytes[1..].iter().copied().enumerate() {
            if valued.contains(&flag) {
                let rest = &bytes[index + 2..];
                let value = if rest.is_empty() {
                    let value = arguments
                        .get(position)
                        .map(|value| value.as_ref().as_bytes());
                    position += usize::from(value.is_some());
                    value
                } else {
                    Some(rest)
                };
                flags.push((flag, value));
                break;
            }
            flags.push((flag, None));
        }
    }
    Arguments {
        flags,
        positional: &arguments[position..],
    }
}

#[cfg(test)]
mod tests {
    use super::require;
    use crate::TmuxVersion;

    fn version(raw: &str) -> TmuxVersion {
        TmuxVersion::parse_output(format!("tmux {raw}\n").as_bytes()).expect("version")
    }

    /// The guard checks the command it is given and nothing nested in it, as
    /// the documentation says. This pins that limit: a refused command
    /// reaches the daemon when it sits inside `if-shell` or `run-shell -C`.
    /// The nested forms are ones tmux accepts, and the direct spelling is
    /// the control that stays refused.
    #[test]
    fn commands_nested_in_another_command_are_not_inspected() {
        assert!(require(&["break-pane"], &version("3.7")).is_err());
        assert!(require(&["break-pane", "-d"], &version("3.7")).is_err());
        assert!(require(&["break-pane", "-d", "-n", "x"], &version("3.7")).is_err());
        for nested in [
            &["if-shell", "true", "break-pane"][..],
            &["run-shell", "-C", "break-pane"],
            &["if-shell", "-F", "1", "break-pane -d"],
        ] {
            assert!(require(nested, &version("3.7")).is_ok(), "{nested:?}");
        }
    }

    /// `-l` may lead the flags, follow a valued flag or share a cluster;
    /// every spelling tmux resolves to refresh-client is checked.
    #[test]
    fn clipboard_query_is_refused_below_3_6a() {
        let commands: [&[&str]; 5] = [
            &["refresh-client", "-l"],
            &["refresh", "-l"],
            &["refresh-c", "-t", "/dev/pts/1", "-l"],
            &["refresh-client", "-Sl"],
            &["refresh-client", "-l%1"],
        ];
        for command in commands {
            for raw in ["3.3a", "3.4", "3.5", "3.5a", "3.6"] {
                assert!(
                    require(command, &version(raw)).is_err(),
                    "{command:?} on {raw}"
                );
            }
            for raw in ["3.6a", "3.6b", "3.7c", "3.8", "next-3.9"] {
                assert!(
                    require(command, &version(raw)).is_ok(),
                    "{command:?} on {raw}"
                );
            }
        }
        for command in [
            &["refresh-client", "-S"][..],
            &["refresh-client", "-t", "-l"],
        ] {
            assert!(require(command, &version("3.3a")).is_ok(), "{command:?}");
        }
    }

    #[test]
    fn colon_and_dot_names_are_refused_on_exactly_3_7() {
        let commands: [&[&str]; 8] = [
            &["new-session", "-d", "-s", "a:b"],
            &["new", "-s", "ok", "-n", "w.x"],
            &["new-window", "-n", "a.b"],
            &["neww", "-t", "$1", "-n", "a:b"],
            &["rename-session", "-t", "$1", "x.y"],
            &["rename", "x.y"],
            &["rename-window", "-t", "@1", "x:y"],
            &["renamew", "x:y"],
        ];
        for command in commands {
            assert!(
                require(command, &version("3.7")).is_err(),
                "{command:?} on 3.7"
            );
            for raw in ["3.7a", "3.8", "3.6b", "3.3a", "next-3.7", "3.7-rc2"] {
                assert!(
                    require(command, &version(raw)).is_ok(),
                    "{command:?} on {raw}"
                );
            }
        }
        for command in [
            &["new-session", "-d", "-s", "plain"][..],
            &["rename-window", "plain"],
            &["new-window", "-t", "a:1", "-n", "plain"],
        ] {
            assert!(require(command, &version("3.7")).is_ok(), "{command:?}");
        }
    }
}
