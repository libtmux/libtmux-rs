use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use super::Event;
use crate::{Error, PaneId, TmuxText, WindowId};

/// Read and classify one protocol line.
///
/// `pending` carries a line across calls. `read_until` appends what it read
/// before it was cancelled, which is what makes this usable in `select!` --
/// `read_line` would lose those bytes, and would also reject the pane output
/// that is not UTF-8.
pub(super) async fn read_line(
    stdout: &mut BufReader<ChildStdout>,
    pending: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<Line>, Error> {
    read_line_within(stdout, pending, limit, None).await
}

/// Read one line, classifying it for the block it arrived in.
///
/// `within` names the open block, if any. tmux(1) guarantees a notification
/// never appears inside an output block, so every line but its own terminator
/// is command output -- including one that looks like a notification, which
/// is what `list-panes -F '#{pane_id}'` produces for every row.
pub(super) async fn read_line_within(
    stdout: &mut BufReader<ChildStdout>,
    pending: &mut Vec<u8>,
    limit: usize,
    within: Option<u64>,
) -> Result<Option<Line>, Error> {
    // One byte past the budget is enough to know the line broke it, and
    // capping the read is what keeps the answer bounded: `read_until` on its
    // own appends until it finds a newline, so checking the length afterwards
    // measures memory already taken. `pending` carries bytes left by a
    // cancelled read, so the cap is what is left of the budget, not all of it.
    let allowance = limit.saturating_sub(pending.len()).saturating_add(1);
    let read = {
        let mut bounded = tokio::io::AsyncReadExt::take(&mut *stdout, allowance as u64);
        bounded
            .read_until(b'\n', pending)
            .await
            .map_err(Error::control_mode)?
    };
    if read == 0 && pending.is_empty() {
        return Ok(None);
    }
    // A line that never ends is the one shape a framed protocol cannot
    // recover from by reading further, so it stops here rather than growing.
    // The connection is not resynchronizable afterwards: the caller reopens.
    if pending.len() > limit {
        pending.clear();
        return Err(Error::control_mode_frame_too_large("line", limit));
    }

    // read_until stops at the newline or at end of input, so what is left
    // without one is the last line tmux managed to write.
    let bytes = pending.strip_suffix(b"\n").unwrap_or(pending);
    let line = match within {
        Some(number) => Line::parse_within_block(bytes, number),
        None => Line::parse(bytes),
    };
    pending.clear();

    Ok(Some(line))
}

/// Write one command line to the connection.
pub(super) async fn write_line(stdin: &mut ChildStdin, line: &str) -> Result<(), Error> {
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(Error::control_mode)?;
    stdin.write_all(b"\n").await.map_err(Error::control_mode)?;
    stdin.flush().await.map_err(Error::control_mode)?;

    Ok(())
}

/// One classified line of the control-mode protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Line {
    BlockStart(u64),
    BlockEnd { number: u64, succeeded: bool },
    Event(Event),
    Text(TmuxText),
}

impl Line {
    /// Classify a line arriving inside the block numbered `number`.
    ///
    /// Only that block's own terminator is structure. Everything else is
    /// output, however much it resembles a notification.
    pub(super) fn parse_within_block(line: &[u8], number: u64) -> Self {
        match Self::parse(line) {
            end @ Self::BlockEnd { number: found, .. } if found == number => end,
            _ => Self::Text(TmuxText::from_bytes(line)),
        }
    }

    pub(super) fn parse(line: &[u8]) -> Self {
        let text = || Self::Text(TmuxText::from_bytes(line));

        let Some(rest) = line.strip_prefix(b"%") else {
            return text();
        };
        let (name, arguments) = split_once(rest, b' ');
        // Every notification tmux names is ASCII. Anything else is a line
        // that happens to start with a percent, not a notification.
        let Ok(name) = std::str::from_utf8(name) else {
            return text();
        };

        // A recognized notification that will not parse answers `Text` rather
        // than falling through, so a malformed line is never reported as an
        // unmodelled one.
        Self::framing(name, arguments, line)
            .or_else(|| Self::about_output(name, arguments, line))
            .or_else(|| Self::about_a_session(name, arguments, line))
            .or_else(|| Self::about_a_window(name, arguments, line))
            .or_else(|| Self::about_the_server(name, arguments, line))
            .unwrap_or_else(|| {
                Self::Event(Event::Other {
                    name: name.to_owned(),
                    rest: TmuxText::from_bytes(arguments),
                })
            })
    }

    /// `%begin`, `%end` and `%error`, which bracket a command's result.
    ///
    /// Each carries a timestamp, a number, and flags. The number correlates a
    /// result with its command; a header without one is text, because guessing
    /// would hand the result to the wrong caller.
    fn framing(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        if !matches!(name, "begin" | "end" | "error") {
            return None;
        }

        let number = std::str::from_utf8(arguments).ok().and_then(|arguments| {
            arguments
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse().ok())
        });

        Some(match (name, number) {
            ("begin", Some(number)) => Self::BlockStart(number),
            (_, Some(number)) => Self::BlockEnd {
                number,
                succeeded: name == "end",
            },
            (_, None) => Self::Text(TmuxText::from_bytes(line)),
        })
    }

    /// What a pane wrote, and the flow control around it.
    fn about_output(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "output" => {
                let (pane, bytes) = split_once(arguments, b' ');
                parsed(pane).map_or_else(text, |pane| {
                    Self::Event(Event::Output {
                        pane,
                        bytes: unescape_output(bytes),
                    })
                })
            }
            // `%extended-output %1 42 : data`. The `:` separator is tmux's,
            // not a delimiter that could occur inside the age.
            "extended-output" => {
                let (pane, rest) = split_once(arguments, b' ');
                let (age, rest) = split_once(rest, b' ');
                let bytes = rest.strip_prefix(b": ").unwrap_or(rest);
                match (parsed(pane), parsed::<u64>(age)) {
                    (Some(pane), Some(age)) => Self::Event(Event::ExtendedOutput {
                        pane,
                        age: Duration::from_millis(age),
                        bytes: unescape_output(bytes),
                    }),
                    _ => text(),
                }
            }
            "pause" => pane_event(arguments, text, |pane| Event::Paused { pane }),
            "continue" => pane_event(arguments, text, |pane| Event::Continued { pane }),
            "pane-mode-changed" => {
                pane_event(arguments, text, |pane| Event::PaneModeChanged { pane })
            }
            _ => return None,
        })
    }

    /// Notifications naming a session.
    fn about_a_session(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "session-changed" => {
                let (session, _) = split_once(arguments, b' ');
                parsed(session).map_or_else(text, |session| {
                    Self::Event(Event::SessionChanged { session })
                })
            }
            "session-renamed" => {
                let (session, new_name) = split_once(arguments, b' ');
                parsed(session).map_or_else(text, |session| {
                    Self::Event(Event::SessionRenamed {
                        session,
                        name: TmuxText::from_bytes(new_name),
                    })
                })
            }
            "session-window-changed" => {
                let (session, window) = split_once(arguments, b' ');
                match (parsed(session), parsed(window)) {
                    (Some(session), Some(window)) => {
                        Self::Event(Event::SessionWindowChanged { session, window })
                    }
                    _ => text(),
                }
            }
            "sessions-changed" => Self::Event(Event::SessionsChanged),
            _ => return None,
        })
    }

    /// Notifications naming a window, linked into the attached session or not.
    fn about_a_window(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "window-add" => window_event(arguments, text, |window| Event::WindowAdded { window }),
            "window-close" => {
                window_event(arguments, text, |window| Event::WindowClosed { window })
            }
            "unlinked-window-add" => window_event(arguments, text, |window| {
                Event::UnlinkedWindowAdded { window }
            }),
            "unlinked-window-close" => window_event(arguments, text, |window| {
                Event::UnlinkedWindowClosed { window }
            }),
            "window-renamed" | "unlinked-window-renamed" => {
                let (window, new_name) = split_once(arguments, b' ');
                parsed(window).map_or_else(text, |window| {
                    let new_name = TmuxText::from_bytes(new_name);
                    Self::Event(if name == "window-renamed" {
                        Event::WindowRenamed {
                            window,
                            name: new_name,
                        }
                    } else {
                        Event::UnlinkedWindowRenamed {
                            window,
                            name: new_name,
                        }
                    })
                })
            }
            "window-pane-changed" => {
                let (window, pane) = split_once(arguments, b' ');
                match (parsed(window), parsed(pane)) {
                    (Some(window), Some(pane)) => {
                        Self::Event(Event::WindowPaneChanged { window, pane })
                    }
                    _ => text(),
                }
            }
            // Built from a format template rather than a printf, so it carries
            // whatever `#{window_raw_flags}` expanded to -- possibly nothing.
            "layout-change" => {
                let (window, rest) = split_once(arguments, b' ');
                let (layout, rest) = split_once(rest, b' ');
                let (visible_layout, flags) = split_once(rest, b' ');
                parsed(window).map_or_else(text, |window| {
                    Self::Event(Event::LayoutChanged {
                        window,
                        layout: TmuxText::from_bytes(layout),
                        visible_layout: TmuxText::from_bytes(visible_layout),
                        flags: TmuxText::from_bytes(flags),
                    })
                })
            }
            _ => return None,
        })
    }

    /// Notifications about clients, buffers, subscriptions, and the server.
    fn about_the_server(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        let text = || Self::Text(TmuxText::from_bytes(line));

        Some(match name {
            "client-detached" => Self::Event(Event::ClientDetached {
                client: TmuxText::from_bytes(arguments),
            }),
            "client-session-changed" => {
                let (client, rest) = split_once(arguments, b' ');
                let (session, session_name) = split_once(rest, b' ');
                parsed(session).map_or_else(text, |session| {
                    Self::Event(Event::ClientSessionChanged {
                        client: TmuxText::from_bytes(client),
                        session,
                        name: TmuxText::from_bytes(session_name),
                    })
                })
            }
            "paste-buffer-changed" => Self::Event(Event::PasteBufferChanged {
                name: TmuxText::from_bytes(arguments),
            }),
            "paste-buffer-deleted" => Self::Event(Event::PasteBufferDeleted {
                name: TmuxText::from_bytes(arguments),
            }),
            "subscription-changed" => Self::subscription(arguments).unwrap_or_else(text),
            "config-error" => Self::Event(Event::ConfigError {
                message: TmuxText::from_bytes(arguments),
            }),
            "message" => Self::Event(Event::Message {
                message: TmuxText::from_bytes(arguments),
            }),
            // A bare `%exit` is an ordinary shutdown; tmux adds a reason when
            // it has one, such as falling too far behind.
            "exit" => Self::Event(Event::Exit {
                reason: (!arguments.is_empty()).then(|| TmuxText::from_bytes(arguments)),
            }),
            _ => return None,
        })
    }

    /// Parse `%subscription-changed <name> $0 @1 2 %3 : <value>`.
    ///
    /// tmux writes `-` for each of window, index and pane when the
    /// subscription is not that specific, so an absent field is a real answer
    /// rather than a parse failure.
    fn subscription(arguments: &[u8]) -> Option<Self> {
        let (name, rest) = split_once(arguments, b' ');
        let (session, rest) = split_once(rest, b' ');
        let (window, rest) = split_once(rest, b' ');
        let (index, rest) = split_once(rest, b' ');
        let (pane, rest) = split_once(rest, b' ');

        Some(Self::Event(Event::SubscriptionChanged {
            name: TmuxText::from_bytes(name),
            session: parsed(session)?,
            window: named(window),
            index: named(index),
            pane: named(pane),
            value: TmuxText::from_bytes(rest.strip_prefix(b": ").unwrap_or(rest)),
        }))
    }
}

/// Parse a subscription field that tmux writes as `-` when it names nothing.
fn named<T: std::str::FromStr>(field: &[u8]) -> Option<T> {
    if field == b"-" {
        return None;
    }
    parsed(field)
}

/// Parse an ASCII field into whatever the caller is collecting.
///
/// Every field tmux puts in a notification is ASCII, so anything that is not
/// is a line which merely begins with a percent.
fn parsed<T: std::str::FromStr>(field: &[u8]) -> Option<T> {
    std::str::from_utf8(field).ok()?.parse().ok()
}

/// Build a notification whose only argument is a pane id.
fn pane_event(
    arguments: &[u8],
    text: impl FnOnce() -> Line,
    build: impl FnOnce(PaneId) -> Event,
) -> Line {
    let (pane, _) = split_once(arguments, b' ');
    parsed(pane).map_or_else(text, |pane| Line::Event(build(pane)))
}

/// Build a notification whose only argument is a window id.
fn window_event(
    arguments: &[u8],
    text: impl FnOnce() -> Line,
    build: impl FnOnce(WindowId) -> Event,
) -> Line {
    let (window, _) = split_once(arguments, b' ');
    parsed(window).map_or_else(text, |window| Line::Event(build(window)))
}

/// Split at the first occurrence of `byte`, which is not kept.
fn split_once(bytes: &[u8], byte: u8) -> (&[u8], &[u8]) {
    bytes
        .iter()
        .position(|found| *found == byte)
        .map_or((bytes, [].as_slice()), |index| {
            (&bytes[..index], &bytes[index + 1..])
        })
}

/// Undo the escaping tmux applies to `%output`.
///
/// tmux writes a byte below `0x20` as `\ooo` and a backslash as `\\`, and
/// leaves everything else alone -- so a pane emitting Latin-1 or binary
/// produces a line that is not UTF-8. Anything else after a backslash is not
/// an escape tmux produces, so it is kept as written rather than guessed at.
pub(super) fn unescape_output(source: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(source.len());
    let mut index = 0;

    while index < source.len() {
        if source[index] != b'\\' {
            bytes.push(source[index]);
            index += 1;
            continue;
        }

        match source.get(index + 1..index + 4) {
            Some(digits) if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) => {
                let value = digits
                    .iter()
                    .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                // Three octal digits can exceed one byte; tmux never emits
                // that, and truncating would corrupt rather than refuse.
                if let Ok(byte) = u8::try_from(value) {
                    bytes.push(byte);
                    index += 4;
                    continue;
                }
                bytes.push(source[index]);
                index += 1;
            }
            _ => {
                if source.get(index + 1) == Some(&b'\\') {
                    bytes.push(b'\\');
                    index += 2;
                } else {
                    bytes.push(source[index]);
                    index += 1;
                }
            }
        }
    }

    bytes
}
