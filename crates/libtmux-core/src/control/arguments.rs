//! Native argv flag inspection shared by command admission and feature gates.

use std::ffi::OsStr;

struct Arguments<'a, T> {
    flags: Vec<(u8, Option<&'a [u8]>)>,
    positional: &'a [T],
}

/// Match a native command's full name, exact alias or unambiguous prefix.
#[must_use]
pub fn native(name: &[u8], full: &[u8], alias: &[u8], shortest: usize) -> bool {
    name == alias || (name.len() >= shortest && full.starts_with(name))
}

/// Inspect flags before the first positional, consuming values as tmux does.
#[must_use]
pub fn flags<'a>(arguments: &'a [impl AsRef<OsStr>], valued: &[u8]) -> Vec<(u8, Option<&'a [u8]>)> {
    scan(arguments, valued).flags
}

/// Return the native positional operands after the flags or an explicit `--`.
#[must_use]
pub fn positionals<'a, T: AsRef<OsStr>>(arguments: &'a [T], valued: &[u8]) -> &'a [T] {
    scan(arguments, valued).positional
}

// Shell strings and nested command queues remain opaque positional operands.
fn scan<'a, T: AsRef<OsStr>>(arguments: &'a [T], valued: &[u8]) -> Arguments<'a, T> {
    let mut flags = Vec::new();
    let mut position = 0;
    while let Some(argument) = arguments.get(position) {
        let bytes = argument.as_ref().as_encoded_bytes();
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
                        .map(|value| value.as_ref().as_encoded_bytes());
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
    use super::{flags, native, positionals};

    #[test]
    fn native_flags_stop_at_operands_and_preserve_dash_prefixed_values() {
        let arguments = ["-aFt", "-l", "operand", "-p"];
        assert_eq!(
            flags(&arguments, b"Ft"),
            [(b'a', None), (b'F', Some(b"t".as_slice())), (b'l', None)]
        );
        assert_eq!(positionals(&arguments, b"Ft"), ["operand", "-p"]);
        assert_eq!(
            flags(&["-t", "-l", "-p"], b"t"),
            [(b't', Some(b"-l".as_slice())), (b'p', None)]
        );
        assert_eq!(positionals(&["-t", "-l", "--", "-p"], b"t"), ["-p"]);
        assert_eq!(flags(&["--", "-p"], b""), []);
        assert_eq!(flags(&["nested -p; capture-pane -p"], b""), []);
        assert_eq!(flags(&["-t"], b"t"), [(b't', None)]);
    }

    #[test]
    fn aliases_are_exact_and_abbreviations_respect_the_native_minimum() {
        assert!(native(b"capturep", b"capture-pane", b"capturep", 2));
        assert!(native(b"ca", b"capture-pane", b"capturep", 2));
        assert!(!native(b"c", b"capture-pane", b"capturep", 2));
        assert!(!native(b"capturep-extra", b"capture-pane", b"capturep", 2));
    }
}
