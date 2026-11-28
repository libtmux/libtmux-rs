use std::ffi::OsStr;

use super::arguments::{flags, native, positionals};

/// Why a public command cannot share a managed control connection.
///
/// Refusal precedes admission and never dispatches the command. A high-level
/// operation may choose its ordinary transport before submission instead.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::{ControlCommandRefusal, require_shared_control};
///
/// assert_eq!(
///     require_shared_control(&["capture-pane", "-p"]),
///     Err(ControlCommandRefusal::UnrepresentableCommand),
/// );
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ControlCommandRefusal {
    /// The command can hold the shared queue waiting for input or completion.
    #[error("command may wait on the shared control queue; use an ordinary client")]
    BlockingCommand,
    /// The command can emit unqualified bytes or execute an untracked queue.
    #[error("command cannot share control reply framing; use an ordinary client")]
    UnrepresentableCommand,
    /// A subscription name or target does not have its checked native form.
    #[error("invalid control subscription name or target; use an ASCII name and typed target")]
    InvalidSubscriptionName,
}

// These native commands do not project arbitrary text onto this client.
// Unknown commands may be aliases that execute a script or nested queue.
const MUTATIONS: &[(&[u8], &[u8], usize)] = &[
    (b"bind-key", b"bind", 4),
    (b"clear-history", b"clearhist", 7),
    (b"clear-prompt-history", b"clearphist", 7),
    (b"clock-mode", b"clock", 5),
    (b"copy-mode", b"copy", 4),
    (b"delete-buffer", b"deleteb", 3),
    (b"detach-client", b"detach", 6),
    (b"has-session", b"has", 3),
    (b"join-pane", b"joinp", 5),
    (b"kill-pane", b"killp", 5),
    (b"kill-server", b"kill-server", 6),
    (b"kill-session", b"kill-session", 6),
    (b"kill-window", b"killw", 5),
    (b"last-pane", b"lastp", 5),
    (b"last-window", b"last", 4),
    (b"link-window", b"linkw", 5),
    (b"move-pane", b"movep", 5),
    (b"move-window", b"movew", 5),
    (b"next-layout", b"nextl", 6),
    (b"next-window", b"next", 4),
    (b"paste-buffer", b"pasteb", 6),
    (b"pipe-pane", b"pipep", 5),
    (b"previous-layout", b"prevl", 6),
    (b"previous-window", b"prev", 4),
    (b"rename-session", b"rename", 7),
    (b"rename-window", b"renamew", 8),
    (b"resize-pane", b"resizep", 8),
    (b"resize-window", b"resizew", 8),
    (b"respawn-pane", b"respawnp", 9),
    (b"respawn-window", b"respawnw", 9),
    (b"rotate-window", b"rotatew", 7),
    (b"select-layout", b"selectl", 8),
    (b"select-pane", b"selectp", 8),
    (b"select-window", b"selectw", 8),
    (b"send-keys", b"send", 4),
    (b"send-prefix", b"sendp", 5),
    (b"set-buffer", b"setb", 5),
    (b"set-environment", b"setenv", 5),
    (b"set-hook", b"set-hook", 5),
    (b"set-option", b"set", 3),
    (b"set-window-option", b"setw", 5),
    (b"start-server", b"start", 5),
    (b"suspend-client", b"suspendc", 8),
    (b"swap-pane", b"swapp", 5),
    (b"swap-window", b"swapw", 5),
    (b"switch-client", b"switchc", 7),
    (b"unbind-key", b"unbind", 6),
    (b"unlink-window", b"unlinkw", 7),
];

/// Require a native command whose waits and output preserve shared framing.
///
/// Drivers must render the command with [`crate::Command::control_mode_line_bounded`]
/// before calling this policy. This check does not qualify a daemon, bound a
/// record or admit a request. Internally authored challenges and drain fences
/// are separate from this public command grammar.
///
/// # Errors
///
/// Returns a typed refusal for waits, scripts, unknown aliases, arbitrary
/// output formats and malformed subscription names or targets.
///
/// # Examples
///
/// ```
/// use libtmux_core::control::require_shared_control;
///
/// require_shared_control(&["list-panes", "-F", "#{pane_id}"])?;
/// # Ok::<(), libtmux_core::control::ControlCommandRefusal>(())
/// ```
pub fn require_shared_control(command: &[impl AsRef<OsStr>]) -> Result<(), ControlCommandRefusal> {
    let Some((name, arguments)) = command.split_first() else {
        return Err(ControlCommandRefusal::UnrepresentableCommand);
    };
    let name = name.as_ref().as_encoded_bytes();
    let is = |full: &[u8], alias: &[u8], shortest| native(name, full, alias, shortest);
    if is(b"wait-for", b"wait", 5) {
        let found = flags(arguments, b"");
        return if found.iter().any(|(flag, _)| b"SU".contains(flag))
            && !found.iter().any(|(flag, _)| *flag == b'L')
        {
            Ok(())
        } else {
            Err(ControlCommandRefusal::BlockingCommand)
        };
    }
    if is(b"run-shell", b"run", 3) {
        return Err(
            if flags(arguments, b"dt")
                .iter()
                .any(|(flag, _)| *flag == b'b')
            {
                ControlCommandRefusal::UnrepresentableCommand
            } else {
                ControlCommandRefusal::BlockingCommand
            },
        );
    }
    if is(b"if-shell", b"if", 2)
        || is(b"command-prompt", b"command-prompt", 8)
        || is(b"confirm-before", b"confirm", 7)
        || is(b"display-menu", b"menu", 9)
        || is(b"display-popup", b"popup", 9)
        || is(b"display-panes", b"displayp", 9)
        || is(b"find-window", b"findw", 5)
        || is(b"choose-tree", b"choose-tree", 8)
        || is(b"choose-client", b"choose-client", 8)
        || is(b"choose-buffer", b"choose-buffer", 8)
        || is(b"lock-client", b"lockc", 5)
        || is(b"lock-session", b"locks", 5)
        || is(b"lock-server", b"lock", 5)
    {
        return Err(ControlCommandRefusal::BlockingCommand);
    }
    if let Some(result) = require_output(name, arguments) {
        return result;
    }
    if is(b"refresh-client", b"refresh", 3) {
        for (flag, value) in flags(arguments, b"ABCFft") {
            if flag == b'B' {
                let Some(value) = value else {
                    return Err(ControlCommandRefusal::UnrepresentableCommand);
                };
                require_subscription(value)?;
            }
        }
        return Ok(());
    }
    if is(b"set-hook", b"set-hook", 5)
        && flags(arguments, b"t").iter().any(|(flag, _)| *flag == b'R')
    {
        return Err(ControlCommandRefusal::UnrepresentableCommand);
    }
    if MUTATIONS
        .iter()
        .any(|(full, alias, shortest)| native(name, full, alias, *shortest))
    {
        Ok(())
    } else {
        Err(ControlCommandRefusal::UnrepresentableCommand)
    }
}

fn require_output(
    name: &[u8],
    arguments: &[impl AsRef<OsStr>],
) -> Option<Result<(), ControlCommandRefusal>> {
    let is = |full: &[u8], alias: &[u8], shortest| native(name, full, alias, shortest);
    if is(b"capture-pane", b"capturep", 2) {
        return Some(
            if flags(arguments, b"bESt")
                .iter()
                .any(|(flag, _)| *flag == b'p')
            {
                Err(ControlCommandRefusal::UnrepresentableCommand)
            } else {
                Ok(())
            },
        );
    }
    if is(b"display-message", b"display", 8) {
        let found = flags(arguments, b"cdFt");
        if found.iter().any(|(flag, _)| b"aIv".contains(flag)) {
            return Some(Err(ControlCommandRefusal::UnrepresentableCommand));
        }
        let positional = positionals(arguments, b"cdFt");
        return Some(
            if matches!(positional, [format] if safe_projection(format.as_ref().as_encoded_bytes()))
                && !found.iter().any(|(flag, _)| *flag == b'F')
            {
                Ok(())
            } else if positional.is_empty() {
                require_projection(arguments, b"cdFt")
            } else {
                Err(ControlCommandRefusal::UnrepresentableCommand)
            },
        );
    }
    if is(b"list-panes", b"lsp", 6)
        || is(b"list-windows", b"lsw", 6)
        || is(b"list-sessions", b"ls", 6)
        || is(b"list-clients", b"lsc", 6)
    {
        return Some(require_projection(arguments, b"fFt"));
    }
    for (full, alias, shortest, valued) in [
        (
            b"new-session".as_slice(),
            b"new".as_slice(),
            3,
            b"cefFnstxy".as_slice(),
        ),
        (b"new-window", b"neww", 5, b"cefFnt"),
        (b"split-window", b"splitw", 2, b"cdefFlt"),
        (b"break-pane", b"breakp", 2, b"Fnt"),
    ] {
        if native(name, full, alias, shortest) {
            return Some(
                if flags(arguments, valued)
                    .iter()
                    .any(|(flag, _)| *flag == b'P')
                {
                    require_projection(arguments, valued)
                } else {
                    Ok(())
                },
            );
        }
    }
    None
}

fn require_projection(
    arguments: &[impl AsRef<OsStr>],
    valued: &[u8],
) -> Result<(), ControlCommandRefusal> {
    let found = flags(arguments, valued);
    let mut formats = found.iter().filter(|(flag, _)| *flag == b'F');
    if matches!((formats.next(), formats.next()), (Some((_, Some(format))), None) if safe_projection(format))
    {
        Ok(())
    } else {
        Err(ControlCommandRefusal::UnrepresentableCommand)
    }
}

/// Whether a nonempty subscription name contains only ASCII letters, digits, `.`, `_` or `-`.
#[doc(hidden)]
#[must_use]
pub fn valid_subscription_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
}

fn require_subscription(value: &[u8]) -> Result<(), ControlCommandRefusal> {
    let Some(first) = value.iter().position(|byte| *byte == b':') else {
        return if valid_subscription_name(value) {
            Ok(())
        } else {
            Err(ControlCommandRefusal::InvalidSubscriptionName)
        };
    };
    if !valid_subscription_name(&value[..first]) {
        return Err(ControlCommandRefusal::InvalidSubscriptionName);
    }
    let rest = &value[first + 1..];
    let Some(second) = rest.iter().position(|byte| *byte == b':') else {
        return Err(ControlCommandRefusal::InvalidSubscriptionName);
    };
    let target = &rest[..second];
    let valid_target = matches!(target, b"" | b"@*" | b"%*")
        || std::str::from_utf8(target).is_ok_and(|target| {
            target.parse::<crate::PaneId>().is_ok() || target.parse::<crate::WindowId>().is_ok()
        });
    if !valid_target {
        return Err(ControlCommandRefusal::InvalidSubscriptionName);
    }
    if !safe_projection(&rest[second + 1..]) {
        return Err(ControlCommandRefusal::UnrepresentableCommand);
    }
    Ok(())
}

fn safe_projection(format: &[u8]) -> bool {
    const FIELDS: &[&[u8]] = &[
        b"#{pid}",
        b"#{start_time}",
        b"#{version}",
        b"#{session_id}",
        b"#{window_id}",
        b"#{pane_id}",
        b"#{pane_height}",
        b"#{pane_width}",
        b"#{pane_index}",
        b"#{window_index}",
        b"#{client_pid}",
        b"#{client_width}",
        b"#{client_height}",
        b"#{client_control_mode}",
        b"#{session_windows}",
        b"#{window_panes}",
        b"#{window_layout}",
        b"#{window_visible_layout}",
        b"#{pane_dead}",
    ];
    if format.iter().any(|byte| matches!(byte, 0 | b'\r' | b'\n'))
        || format.windows(2).any(|bytes| bytes == b"#(")
    {
        return false;
    }
    let mut remaining = format;
    while let Some(offset) = remaining.windows(2).position(|bytes| bytes == b"#{") {
        if !safe_literal(&remaining[..offset]) {
            return false;
        }
        let expression = &remaining[offset..];
        let Some(end) = expression.iter().position(|byte| *byte == b'}') else {
            return false;
        };
        if !FIELDS.contains(&&expression[..=end]) {
            return false;
        }
        remaining = &expression[end + 1..];
    }
    safe_literal(remaining)
}

fn safe_literal(bytes: &[u8]) -> bool {
    // Short formats expand names, and strftime's %n inserts a record delimiter.
    !bytes.iter().any(|byte| matches!(byte, b'#' | b'%'))
}

#[cfg(test)]
mod tests {
    use super::{ControlCommandRefusal, require_shared_control as require_shared};

    #[test]
    fn refusal_kinds_preserve_wait_output_and_subscription_causes() {
        for (command, refusal) in [
            (
                vec!["wait-for", "-L", "channel"],
                ControlCommandRefusal::BlockingCommand,
            ),
            (
                vec!["if", "-F", "1", "display-message -p nested"],
                ControlCommandRefusal::BlockingCommand,
            ),
            (
                vec!["run", "-b", "printf data"],
                ControlCommandRefusal::UnrepresentableCommand,
            ),
            (
                vec!["capturep", "-p"],
                ControlCommandRefusal::UnrepresentableCommand,
            ),
            (
                vec!["refresh", "-B", "bad name::#{pane_id}"],
                ControlCommandRefusal::InvalidSubscriptionName,
            ),
            (
                vec!["refresh", "-B", "valid:$0:#{pane_id}"],
                ControlCommandRefusal::InvalidSubscriptionName,
            ),
            (vec![], ControlCommandRefusal::UnrepresentableCommand),
        ] {
            assert_eq!(require_shared(&command), Err(refusal), "{command:?}");
        }
    }

    #[test]
    fn valued_flags_and_terminators_cannot_hide_output_or_wait_flags() {
        for command in [
            vec!["capture-pane", "-t", "-p", "-p"],
            vec!["capture-pane", "-pE0"],
            vec!["wait-for", "--", "-S"],
            vec!["wait-for", "-SL", "channel"],
            vec!["list-panes", "-Ft", "-F#{pane_title}"],
            vec!["display-message", "-F", "#{pane_id}", "#{pane_id}"],
            vec!["display-message", "--", "#{pane_title}"],
            vec!["display-message", "-pF#{pane_id}#S"],
            vec!["refresh-client", "-Bids:%3:#{pane_id}%n"],
        ] {
            assert!(require_shared(&command).is_err(), "{command:?}");
        }
        for command in [
            vec!["capture-pane", "-t", "-p"],
            vec!["capture-pane", "-E0p"],
            vec!["capture-pane", "--", "-p"],
            vec!["display-message", "-pt", "%3", "#{pane_id}"],
            vec!["display-message", "--", "#{pane_id}"],
            vec!["refresh", "-Bids:@*:#{window_id}"],
            vec!["refresh", "-Bids.3_"],
        ] {
            assert!(require_shared(&command).is_ok(), "{command:?}");
        }
    }

    #[test]
    fn client_waits_and_ambiguous_payloads_refuse_before_admission() {
        for command in [
            vec!["wait-for", "channel"],
            vec!["wait", "-L", "channel"],
            vec!["run-shell", "sleep 2"],
            vec!["run-shell", "-b", "echo uncontrolled"],
            vec!["if", "true", "wait channel"],
            vec!["capturep", "-p"],
            vec!["show-buffer"],
            vec!["list-windows"],
            vec!["list-panes", "-F", "#{pane_title}"],
            vec!["new-window", "-P"],
            vec!["new-window", "-P", "-F", "#{window_name}"],
            vec!["display-message", "-p", "#{window_name}"],
            vec!["display-message", "-p", "%end 1 2 1"],
            vec!["display-message", "-a"],
            vec!["display-message", "-p", "#(printf uncontrolled)"],
            vec!["display-message", "-p", "#S"],
            vec!["display-message", "-p", "%n"],
            vec!["display-message", "-p", "prefix#Wsuffix"],
            vec![
                "display-message",
                "-p",
                "#{?pane_dead,#{pane_id},#{pane_title}}",
            ],
            vec!["display-message", "-p", "#{pane_id}#T"],
            vec!["display-message", "-pF#{pane_title}"],
            vec!["source-file", "commands"],
            vec!["custom-script-alias"],
            vec!["set-hook", "-R", "after-new-window"],
            vec!["refresh-client", "-B", "bad name::#{pane_id}"],
            vec!["refresh-client", "-B", "good:%3 extra:#{pane_id}"],
            vec!["refresh-client", "-B", "good:unexpected:#{pane_id}"],
            vec!["refresh-client", "-B", "good::#S"],
            vec!["refresh-client", "-B", "good::#{pane_id}#T"],
        ] {
            assert!(require_shared(&command).is_err(), "{command:?}");
        }
        for command in [
            vec!["wait-for", "-S", "channel"],
            vec!["wait", "-U", "channel"],
            vec!["capture-pane", "-b", "held"],
            vec!["send-keys", "-l", "-p"],
            vec!["display-message", "-pt%3", "literal:#{pane_id}"],
            vec!["display-message", "-p", "-t", "%3", "#{pane_height}"],
            vec!["display-message", "-pF#{pane_id}"],
            vec!["list-panes", "-aF#{pane_id}"],
            vec!["new-window", "-P", "-F", "#{window_id}"],
            vec!["refresh-client", "-B", "ids:%3:#{pane_id}"],
        ] {
            assert!(require_shared(&command).is_ok(), "{command:?}");
        }
        assert!(require_shared(&["display-message", "-p", "-t", "%3"]).is_err());
        assert!(
            require_shared(&["list-panes", "-F", "#{pane_id}", "-F", "#{pane_title}"]).is_err()
        );
    }
}
