//! What tmux declares about each of its options.
//!
//! tmux knows every option's type, but reports none of it over the command
//! line: `show-options` prints values and nothing else. The schema is
//! therefore generated from tmux's own `options-table.c` rather than guessed
//! from a value's shape, which would read `on` as a flag and `2` as a number
//! whatever the option actually is.

mod generated;

pub use generated::names;

use crate::formats::TmuxText;

/// What kind of value an option holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OptionKind {
    /// `on` or `off`.
    Flag,
    /// An integer.
    Number,
    /// One of a fixed set of words.
    Choice,
    /// Arbitrary text.
    Text,
    /// A terminal colour.
    Colour,
    /// A key name.
    Key,
    /// A tmux command.
    Command,
}

/// Which table an option primarily lives in.
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

/// What tmux declares about one option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OptionSchema {
    name: &'static str,
    kind: OptionKind,
    scope: OptionScope,
}

impl OptionSchema {
    pub(crate) const fn new(name: &'static str, kind: OptionKind, scope: OptionScope) -> Self {
        Self { name, kind, scope }
    }

    /// Return the option's tmux name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Return what kind of value the option holds.
    #[must_use]
    pub const fn kind(&self) -> OptionKind {
        self.kind
    }

    /// Return which table the option primarily lives in.
    #[must_use]
    pub const fn scope(&self) -> OptionScope {
        self.scope
    }
}

/// Look up what tmux declares about one option.
///
/// An option tmux does not declare, such as a user option beginning with `@`,
/// returns `None`: it has no type beyond the text stored in it.
///
/// The name may carry an array index, as `after-new-window[0]` does, which is
/// ignored for the lookup because every element of an array option shares one
/// type.
///
/// # Examples
///
/// ```
/// use libtmux::{OptionKind, option_schema};
///
/// // `status` looks like a flag but accepts on, off, and 2 through 5, so
/// // tmux declares it a choice. The schema records that rather than guessing.
/// assert_eq!(option_schema("status").map(|o| o.kind()), Some(OptionKind::Choice));
/// assert_eq!(option_schema("mouse").map(|o| o.kind()), Some(OptionKind::Flag));
/// assert_eq!(option_schema("history-limit").map(|o| o.kind()), Some(OptionKind::Number));
/// assert_eq!(option_schema("after-new-window[0]").map(|o| o.kind()), Some(OptionKind::Command));
/// assert_eq!(option_schema("@mine"), None);
/// ```
#[must_use]
pub fn option_schema(name: &str) -> Option<&'static OptionSchema> {
    let name = name.split_once('[').map_or(name, |(base, _)| base);

    generated::OPTION_SCHEMA
        .binary_search_by(|entry| entry.name.cmp(name))
        .ok()
        .map(|index| &generated::OPTION_SCHEMA[index])
}

/// One option's value, decoded according to what tmux declares about it.
///
/// This is what [`crate::Server::typed_option`] and its per-object siblings
/// return, so a caller reading `status` gets a flag without deciding for
/// itself that `on` means one.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::OptionValue;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let server = guard.server();
/// server.new_session("typed").await?;
///
/// // `mouse` is a flag, so `on` arrives as one.
/// let mouse = server.typed_global_option("mouse").await?.expect("mouse is set");
/// assert!(matches!(mouse, OptionValue::Flag(false)));
///
/// // `status` also reads `on`, and is *not* a flag: tmux accepts `on`, `off`,
/// // and `2` through `5`. Inferring the type from the value would call this a
/// // boolean and then fail on a value that is not one, which is why the
/// // schema is generated from tmux's own option table instead.
/// let status = server.typed_global_option("status").await?.expect("status is set");
/// assert!(matches!(status, OptionValue::Text(_)));
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OptionValue {
    /// A flag tmux wrote as `on` or `off`.
    Flag(bool),
    /// A number.
    Number(i64),
    /// Text, which covers choices, colours, keys, commands, and user options.
    ///
    /// tmux validates a choice when it is set, so a value read back is one
    /// tmux accepted. The variants are not enumerated here because they differ
    /// per option and per release.
    Text(TmuxText),
}

impl OptionValue {
    /// Decode a stored value according to an option's declared kind.
    ///
    /// A value that does not match its declared kind stays [`OptionValue::Text`]
    /// rather than being discarded, because tmux stored it and the caller may
    /// still want it.
    pub(crate) fn decode(name: &str, value: TmuxText) -> Self {
        match option_schema(name).map(OptionSchema::kind) {
            Some(OptionKind::Flag) => value
                .as_flag()
                .map_or_else(|| Self::Text(value.clone()), Self::Flag),
            Some(OptionKind::Number) => value
                .parse::<i64>()
                .map_or_else(|| Self::Text(value.clone()), Self::Number),
            _ => Self::Text(value),
        }
    }
}
