use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;

use crate::{Error, ReleaseSuffix, ReleaseVersion, TmuxVersion};

const SAFE_NAMED_BUFFERS: ReleaseVersion = crate::since::NAMED_BUFFER_IDENTITY;
const BREAK_BROKEN: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);
const BREAK_FIXED: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::PATCH_A);
const LAYOUT_ERRORS_FIXED: ReleaseVersion = ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL);

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
            version.require("exact named-buffer deletion", SAFE_NAMED_BUFFERS)?;
        }
    } else if native(name, b"set-buffer", b"setb", 5) {
        let flags = flags(arguments, b"bnt");
        if flags.iter().any(|(flag, _)| *flag == b'b')
            && flags.iter().any(|(flag, _)| *flag == b'n')
        {
            version.require("exact named-buffer rename", SAFE_NAMED_BUFFERS)?;
        }
    } else if native(name, b"break-pane", b"breakp", 2) {
        if version
            .behavior_release()
            .is_some_and(|release| release >= BREAK_BROKEN && release < BREAK_FIXED)
        {
            return Err(Error::CapabilityDefective {
                capability: "break-pane",
                found: version.clone(),
                broken_in: BREAK_BROKEN,
                fixed_in: BREAK_FIXED,
            });
        }
    } else if native(name, b"split-window", b"splitw", 2) {
        if flags(arguments, b"ceFlpt")
            .iter()
            .any(|(flag, _)| *flag == b'p')
        {
            refuse_interval(
                version,
                "split-window -p",
                ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL),
                ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL),
            )?;
        }
    } else if native(name, b"select-layout", b"selectl", 8) {
        if version
            .behavior_release()
            .is_some_and(|release| release < LAYOUT_ERRORS_FIXED)
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
        if flags(arguments, b"ABCFft").iter().any(|(flag, value)| {
            *flag == b'A' && value.is_some_and(|value| value.ends_with(b":off"))
        }) {
            version.require("refresh-client pane off", crate::since::CONTROL_PANE_OFF)?;
        }
    } else if native(name, b"capture-pane", b"capturep", 2) {
        for (flag, _) in flags(arguments, b"bESt") {
            match flag {
                b'T' => {
                    version.require("capture-pane -T", crate::since::CAPTURE_TRIM_BLANK_CELLS)?;
                }
                b'F' => {
                    version.require("capture-pane line flags", crate::since::CAPTURE_LINE_FLAGS)?;
                }
                _ => {}
            }
        }
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
