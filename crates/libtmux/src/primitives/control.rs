//! Control-mode notifications and byte-preserving line decoding.
//!
//! This module performs no I/O. A transport supplies complete lines and the
//! current command block number, then handles the returned classifications.

use std::time::Duration;

use crate::{PaneId, SessionId, TmuxText, WindowId};

/// Something tmux reported that no command asked for.
///
/// The variants cover every notification tmux publishes across the supported
/// releases. [`Event::Other`] keeps an unrecognized one rather than dropping
/// it, because tmux adds notifications between releases; its name is the tmux
/// notification without the leading `%`.
///
/// Four of these are newer than the oldest tmux this crate supports:
/// `%config-error`, `%message`, `%paste-buffer-changed` and
/// `%paste-buffer-deleted` are never emitted by 3.2a. Nothing else in the
/// vocabulary is version-dependent.
///
/// # Examples
///
/// ```
/// use libtmux::protocol::{Event, Line};
///
/// let Line::Event(event) = Line::parse(b"%output %3 hello") else {
///     panic!("an output notification is an event");
/// };
/// assert!(matches!(event, Event::Output { .. }));
/// assert!(!event.invalidates_listings());
/// assert_eq!(event.pane().map(AsRef::as_ref), Some("%3"));
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A pane produced output.
    ///
    /// The bytes are exactly what the pane wrote. tmux escapes only what
    /// would break the line protocol -- bytes below `0x20`, and backslash --
    /// so everything above `0x7f` arrives literally and the line as a whole
    /// is not necessarily UTF-8. That is why this is bytes.
    Output {
        /// The pane that produced it.
        pane: PaneId,
        /// The output bytes.
        bytes: Vec<u8>,
    },
    /// A pane produced output, and tmux said how far behind it is.
    ///
    /// Replaces [`Event::Output`] for the whole connection once a client asks
    /// for `pause-after`, so a caller that sets that flag must handle both.
    ExtendedOutput {
        /// The pane that produced it.
        pane: PaneId,
        /// How long this output sat before tmux sent it.
        age: Duration,
        /// The output bytes.
        bytes: Vec<u8>,
    },
    /// tmux stopped sending a pane's output.
    ///
    /// A client can request this with the `pause-after` flag or with
    /// `refresh-client -A` and the `pause` action. The `continue` action
    /// resumes output.
    Paused {
        /// The pane that was paused.
        pane: PaneId,
    },
    /// tmux resumed a pane it had paused.
    Continued {
        /// The pane that resumed.
        pane: PaneId,
    },
    /// The attached session changed.
    SessionChanged {
        /// The session now attached.
        session: SessionId,
    },
    /// A session was renamed.
    SessionRenamed {
        /// The session that was renamed.
        session: SessionId,
        /// Its new name.
        name: TmuxText,
    },
    /// A session's active window changed.
    SessionWindowChanged {
        /// The session whose active window changed.
        session: SessionId,
        /// The window now active in it.
        window: WindowId,
    },
    /// A session was created or destroyed, so the session list is now wrong.
    ///
    /// tmux says only that the set changed, not which session it was.
    SessionsChanged,
    /// A window was linked into the attached session.
    WindowAdded {
        /// The window that appeared.
        window: WindowId,
    },
    /// A window in the attached session closed.
    WindowClosed {
        /// The window that closed.
        window: WindowId,
    },
    /// A window in the attached session was renamed.
    WindowRenamed {
        /// The window that was renamed.
        window: WindowId,
        /// Its new name.
        name: TmuxText,
    },
    /// A window's active pane changed.
    WindowPaneChanged {
        /// The window whose active pane changed.
        window: WindowId,
        /// The pane now active in it.
        pane: PaneId,
    },
    /// A window appeared that the attached session does not link.
    UnlinkedWindowAdded {
        /// The window that appeared.
        window: WindowId,
    },
    /// A window the attached session does not link closed.
    UnlinkedWindowClosed {
        /// The window that closed.
        window: WindowId,
    },
    /// A window the attached session does not link was renamed.
    UnlinkedWindowRenamed {
        /// The window that was renamed.
        window: WindowId,
        /// Its new name.
        name: TmuxText,
    },
    /// A window's panes were rearranged, added to, or removed from.
    ///
    /// tmux has no notification for a pane appearing, so this is the one that
    /// reports it: every split changes the layout, including a detached split
    /// that leaves the active pane alone and reports nothing else.
    LayoutChanged {
        /// The window whose layout changed.
        window: WindowId,
        /// The new layout, in tmux's own layout syntax.
        layout: TmuxText,
        /// The layout as displayed, which differs when a pane is zoomed.
        visible_layout: TmuxText,
        /// The window's flags, such as `*` for active.
        flags: TmuxText,
    },
    /// A pane entered or left a mode, such as copy mode.
    ///
    /// tmux says the pane changed mode, not which mode it is now; read
    /// `pane_mode` to learn that.
    PaneModeChanged {
        /// The pane whose mode changed.
        pane: PaneId,
    },
    /// A client detached from the server.
    ClientDetached {
        /// The client that left.
        client: TmuxText,
    },
    /// A client switched to a different session.
    ClientSessionChanged {
        /// The client that switched.
        client: TmuxText,
        /// The session it switched to.
        session: SessionId,
        /// That session's name.
        name: TmuxText,
    },
    /// A paste buffer was created or replaced.
    PasteBufferChanged {
        /// The buffer's name.
        name: TmuxText,
    },
    /// A paste buffer was deleted.
    PasteBufferDeleted {
        /// The buffer's name.
        name: TmuxText,
    },
    /// A format this client subscribed to with `refresh-client -B` changed.
    SubscriptionChanged {
        /// The subscription name the caller chose.
        name: TmuxText,
        /// The session it is about.
        session: SessionId,
        /// The window it is about, when the subscription names one.
        window: Option<WindowId>,
        /// That window's index, when the subscription names one.
        index: Option<u32>,
        /// The pane it is about, when the subscription names one.
        pane: Option<PaneId>,
        /// The format's new value.
        value: TmuxText,
    },
    /// tmux could not read part of its configuration.
    ConfigError {
        /// What tmux said was wrong.
        message: TmuxText,
    },
    /// A message tmux was asked to display, by `display-message` or a hook.
    Message {
        /// The message text.
        message: TmuxText,
    },
    /// The server is going away, so no further events will arrive.
    Exit {
        /// Why, when tmux gave a reason. `None` is an ordinary shutdown.
        reason: Option<TmuxText>,
    },
    /// A notification this crate does not model.
    Other {
        /// The notification name, without its `%`.
        name: String,
        /// The rest of the line.
        rest: TmuxText,
    },
}

impl Event {
    /// Report whether a listing taken before this event may now be wrong.
    ///
    /// Output and the flow-control events say nothing about the shape of the
    /// server. [`Event::Other`] counts as invalidating, because an unmodelled
    /// notification is one whose meaning is not known here.
    #[must_use]
    pub const fn invalidates_listings(&self) -> bool {
        !matches!(
            self,
            Self::Output { .. }
                | Self::ExtendedOutput { .. }
                | Self::Paused { .. }
                | Self::Continued { .. }
                | Self::SubscriptionChanged { .. }
                | Self::ConfigError { .. }
                | Self::Message { .. }
        )
    }

    /// Report whether a pane may have appeared since the last look.
    ///
    /// tmux publishes no notification for a pane being created, so this is a
    /// conservative union of the events that can accompany one. A caller that
    /// disabled output from other panes must update that selection when
    /// this answers `true`.
    #[must_use]
    pub const fn may_have_added_a_pane(&self) -> bool {
        matches!(
            self,
            Self::LayoutChanged { .. }
                | Self::WindowAdded { .. }
                | Self::UnlinkedWindowAdded { .. }
                | Self::SessionsChanged
                | Self::SessionChanged { .. }
                | Self::Other { .. }
        )
    }

    /// Return the pane this event is about, when it is about one.
    #[must_use]
    pub const fn pane(&self) -> Option<&PaneId> {
        match self {
            Self::Output { pane, .. }
            | Self::ExtendedOutput { pane, .. }
            | Self::Paused { pane }
            | Self::Continued { pane }
            | Self::PaneModeChanged { pane }
            | Self::WindowPaneChanged { pane, .. } => Some(pane),
            Self::SubscriptionChanged { pane, .. } => pane.as_ref(),
            _ => None,
        }
    }

    /// Return the window this event is about, when it is about one.
    #[must_use]
    pub const fn window(&self) -> Option<&WindowId> {
        match self {
            Self::WindowAdded { window }
            | Self::WindowClosed { window }
            | Self::WindowRenamed { window, .. }
            | Self::WindowPaneChanged { window, .. }
            | Self::UnlinkedWindowAdded { window }
            | Self::UnlinkedWindowClosed { window }
            | Self::UnlinkedWindowRenamed { window, .. }
            | Self::LayoutChanged { window, .. }
            | Self::SessionWindowChanged { window, .. } => Some(window),
            Self::SubscriptionChanged { window, .. } => window.as_ref(),
            _ => None,
        }
    }
}

/// One classified line of the control-mode protocol.
///
/// Pass bytes without the trailing newline. Inside a command output block,
/// use [`Line::parse_within_block`] so notification-looking output remains
/// command output. These functions classify a complete line; the transport
/// owns line buffering, size limits, and end-of-input handling.
///
/// # Examples
///
/// ```
/// use libtmux::protocol::Line;
///
/// assert_eq!(Line::parse(b"%begin 1700000000 12 0"), Line::BlockStart { number: 12, flags: 0 });
/// assert!(matches!(
///     Line::parse_within_block(b"%output %3 literal output", 12),
///     Line::Text(_),
/// ));
/// assert_eq!(
///     Line::parse_within_block(b"%end 1700000000 12 0", 12),
///     Line::BlockEnd { number: 12, succeeded: true },
/// );
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Line {
    /// A command output block began, with its correlation number.
    BlockStart {
        /// The number shared by this block's opening and closing frames.
        number: u64,
        /// tmux's origin flags; bit zero identifies a control command.
        flags: u64,
    },
    /// A command output block ended.
    BlockEnd {
        /// The correlation number assigned by tmux.
        number: u64,
        /// Whether tmux sent `%end` rather than `%error`.
        succeeded: bool,
    },
    /// A notification outside a command output block.
    Event(Event),
    /// Command output or a malformed recognized notification.
    Text(TmuxText),
}

impl Line {
    /// Classify a line arriving inside the block numbered `number`.
    ///
    /// Only that block's own terminator is structure. Everything else is
    /// output, however much it resembles a notification.
    #[must_use]
    pub fn parse_within_block(line: &[u8], number: u64) -> Self {
        match Self::parse(line) {
            end @ Self::BlockEnd { number: found, .. } if found == number => end,
            _ => Self::Text(TmuxText::from_bytes(line)),
        }
    }

    /// Classify a complete line received outside a command output block.
    ///
    /// Unknown notifications become [`Event::Other`]. Malformed recognized
    /// notifications remain [`Line::Text`], preserving their bytes.
    #[must_use]
    pub fn parse(line: &[u8]) -> Self {
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
    /// The number pairs the opening and closing frames. Origin flags keep
    /// independent hooks and background jobs apart from submitted commands.
    fn framing(name: &str, arguments: &[u8], line: &[u8]) -> Option<Self> {
        if !matches!(name, "begin" | "end" | "error") {
            return None;
        }

        let header = std::str::from_utf8(arguments).ok().and_then(|arguments| {
            let mut fields = arguments.split_whitespace();
            fields.next()?;
            let number = fields.next()?.parse().ok()?;
            let flags = fields.next()?.parse().ok()?;
            Some((number, flags))
        });

        Some(match (name, header) {
            ("begin", Some((number, flags))) => Self::BlockStart { number, flags },
            (_, Some((number, _))) => Self::BlockEnd {
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
#[must_use]
pub fn unescape_output(source: &[u8]) -> Vec<u8> {
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
