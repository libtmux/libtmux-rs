use std::ffi::{OsStr, OsString};

/// The table where tmux reads or writes an option.
///
/// # Examples
///
/// ```
/// use libtmux_core::OptionScope;
/// assert_ne!(OptionScope::Session, OptionScope::Window);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OptionScope {
    /// Server options, read with tmux's `-s`.
    Server,
    /// Session options.
    Session,
    /// Window options, read with tmux's `-w`.
    Window,
    /// Pane options, read with tmux's `-p`.
    Pane,
}

/// The side of the original pane where a split places its new pane.
///
/// # Examples
///
/// ```
/// use libtmux_core::SplitDirection;
/// assert_eq!(SplitDirection::Left.flags(), ("-h", true));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    /// Above the pane being divided.
    Above,
    /// Below it, which is tmux's default.
    Below,
    /// To its left.
    Left,
    /// To its right.
    Right,
}

impl SplitDirection {
    /// Return the tmux flags that produce this position.
    #[must_use]
    pub const fn flags(self) -> (&'static str, bool) {
        (if self.is_vertical() { "-v" } else { "-h" }, self.before())
    }

    /// Whether this divides a pane top from bottom rather than side to side.
    #[must_use]
    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::Above | Self::Below)
    }

    /// Whether tmux needs `-b` to put the new pane on this side.
    #[must_use]
    pub const fn before(self) -> bool {
        matches!(self, Self::Above | Self::Left)
    }
}

/// Encode one environment assignment for tmux's `-e` flag.
#[must_use]
pub fn assignment(name: &OsStr, value: &OsStr) -> OsString {
    let mut pair = name.to_owned();
    pair.push("=");
    pair.push(value);
    pair
}
