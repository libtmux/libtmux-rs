//! What tmux declares about each of its options.
//!
//! tmux knows every option's type, but reports none of it over the command
//! line: `show-options` prints values and nothing else. The schema is
//! therefore generated from tmux's own `options-table.c` rather than guessed
//! from a value's shape, which would read `on` as a flag and `2` as a number
//! whatever the option actually is.
//!
//! The catalogue is generated from [tmux 3.7's option table][table]. Later
//! scope requirements are reported separately by [`OptionSchema::required_version`].
//!
//! [table]: https://github.com/tmux/tmux/blob/94cb3cec6a39aadefe7b1e81068caa72c9345952/options-table.c

mod generated;

pub use generated::names;

use std::fmt;
use std::ops::RangeInclusive;

use crate::TmuxText;

/// What kind of value an option holds.
///
/// # Examples
///
/// ```
/// use libtmux_core::options::{OptionKind, option_schema};
///
/// // `mouse` is a real flag: on or off.
/// assert_eq!(option_schema("mouse").map(OptionSchema::kind), Some(OptionKind::Flag));
///
/// // `status` looks like one and is not: it also accepts a count of status
/// // lines, so reading it as a boolean discards those values.
/// assert_eq!(option_schema("status").map(OptionSchema::kind), Some(OptionKind::Choice));
/// # use libtmux_core::options::OptionSchema;
/// ```
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

pub use crate::OptionScope;

/// Options tmux started keeping at a second scope after the supported floor.
///
/// Gating on the current table alone would allow a write that an older tmux
/// silently places at the other scope, which is the defect this guard exists
/// to stop.
const LATE_SCOPES: &[(&str, OptionScope, crate::ReleaseVersion)] = &[
    (
        "pane-active-border-style",
        OptionScope::Pane,
        crate::since::PANE_BORDER_STYLE_PER_PANE,
    ),
    (
        "pane-border-style",
        OptionScope::Pane,
        crate::since::PANE_BORDER_STYLE_PER_PANE,
    ),
];

/// What tmux declares about one option.
///
/// # Examples
///
/// ```
/// use libtmux_core::options::{OptionKind, OptionScope, option_schema};
///
/// let schema = option_schema("history-limit").expect("a documented option");
/// assert_eq!(schema.name(), "history-limit");
/// assert_eq!(schema.kind(), OptionKind::Number);
/// assert_eq!(schema.scopes(), [OptionScope::Session]);
/// assert!(schema.accepts(OptionScope::Session));
///
/// // An option tmux does not have has no schema, which catches a typo before it
/// // reaches the server.
/// assert!(option_schema("history-limits").is_none());
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OptionSchema {
    name: &'static str,
    kind: OptionKind,
    scopes: &'static [OptionScope],
    choices: &'static [&'static str],
    range: Option<(i64, i64)>,
}

impl OptionSchema {
    pub(crate) const fn new(
        name: &'static str,
        kind: OptionKind,
        scopes: &'static [OptionScope],
    ) -> Self {
        Self {
            name,
            kind,
            scopes,
            choices: &[],
            range: None,
        }
    }

    pub(crate) const fn with_choices(mut self, choices: &'static [&'static str]) -> Self {
        self.choices = choices;
        self
    }

    pub(crate) const fn with_range(mut self, minimum: i64, maximum: i64) -> Self {
        self.range = Some((minimum, maximum));
        self
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

    /// Return every table the option may be written in.
    ///
    /// Usually one, and tmux names no primary among the rest: `remain-on-exit`
    /// is a window option and a pane option both, and a write is legal at
    /// either.
    #[must_use]
    pub const fn scopes(&self) -> &'static [OptionScope] {
        self.scopes
    }

    /// Report whether tmux will place a write of this option at `scope`.
    ///
    /// tmux resolves an option by name rather than by the flags it was sent
    /// with, so a write it does not accept here is not refused: it lands at
    /// whichever table the name belongs to, and reports success for doing it.
    #[must_use]
    pub fn accepts(&self, scope: OptionScope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Return the required version for a scope added after the supported floor.
    ///
    /// `None` means no additional version requirement is recorded. Check
    /// [`Self::accepts`] first: an absent scope also has no requirement here.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::{OptionScope, since};
    /// use libtmux_core::options::option_schema;
    ///
    /// assert!(option_schema("pane-border-style").is_some_and(|schema|
    ///     schema.required_version(OptionScope::Pane) == Some(since::PANE_BORDER_STYLE_PER_PANE)));
    /// ```
    #[must_use]
    pub fn required_version(&self, scope: OptionScope) -> Option<crate::ReleaseVersion> {
        LATE_SCOPES.iter().find_map(|(name, late, needs)| {
            (*name == self.name && *late == scope).then_some(*needs)
        })
    }

    /// Return the words a [`OptionKind::Choice`] option accepts, in tmux's
    /// order.
    ///
    /// Empty for every other kind. tmux compares a choice exactly, so case
    /// matters.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::options::option_schema;
    ///
    /// let keys = option_schema("mode-keys").expect("a documented option");
    /// assert_eq!(keys.choices(), ["emacs", "vi"]);
    ///
    /// let limit = option_schema("history-limit").expect("a documented option");
    /// assert!(limit.choices().is_empty());
    /// ```
    #[must_use]
    pub const fn choices(&self) -> &'static [&'static str] {
        self.choices
    }

    /// Return the inclusive range a [`OptionKind::Number`] option accepts.
    ///
    /// `None` for every other kind.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::options::option_schema;
    ///
    /// let limit = option_schema("buffer-limit").expect("a documented option");
    /// assert_eq!(limit.range(), Some(1..=i64::from(i32::MAX)));
    ///
    /// let keys = option_schema("mode-keys").expect("a documented option");
    /// assert_eq!(keys.range(), None);
    /// ```
    #[must_use]
    pub fn range(&self) -> Option<RangeInclusive<i64>> {
        self.range.map(|(minimum, maximum)| minimum..=maximum)
    }

    /// Check a value against what the table declares, before it is sent.
    ///
    /// Flags require [`OptionValue::Flag`], numbers [`OptionValue::Number`],
    /// and other kinds [`OptionValue::Text`].
    ///
    /// # Errors
    ///
    /// Returns a refusal for the wrong variant, an unlisted choice or an
    /// out-of-range number. The refusal never retains the supplied value.
    pub fn check(&self, value: &OptionValue) -> Result<(), OptionValueRefusal> {
        match (self.kind, value) {
            (OptionKind::Number, OptionValue::Number(number)) => match self.range() {
                Some(range) if !range.contains(number) => {
                    Err(OptionValueRefusal::OutOfRange { range })
                }
                _ => Ok(()),
            },
            (OptionKind::Choice, OptionValue::Text(text)) => {
                if self
                    .choices
                    .iter()
                    .any(|choice| choice.as_bytes() == text.as_bytes())
                {
                    Ok(())
                } else {
                    Err(OptionValueRefusal::NotAChoice {
                        choices: self.choices,
                    })
                }
            }
            (OptionKind::Flag, OptionValue::Flag(_))
            | (
                OptionKind::Text | OptionKind::Colour | OptionKind::Key | OptionKind::Command,
                OptionValue::Text(_),
            ) => Ok(()),
            _ => Err(OptionValueRefusal::WrongKind {
                expected: self.kind,
            }),
        }
    }
}

/// Why a typed option write was refused before it reached tmux.
///
/// Returned by [`OptionSchema::check`]. Never holds the supplied value, which
/// may contain sensitive text.
///
/// # Examples
///
/// ```
/// use libtmux_core::options::{OptionKind, OptionValueRefusal};
///
/// fn advise(refusal: &OptionValueRefusal) -> String {
///     match refusal {
///         OptionValueRefusal::WrongKind { expected } => format!("pass a {expected:?}"),
///         OptionValueRefusal::NotAChoice { choices } => format!("pick one of {choices:?}"),
///         OptionValueRefusal::OutOfRange { range } => format!("stay within {range:?}"),
///         _ => "check the value".to_owned(),
///     }
/// }
///
/// let refusal = OptionValueRefusal::WrongKind { expected: OptionKind::Flag };
/// assert_eq!(advise(&refusal), "pass a Flag");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OptionValueRefusal {
    /// The value is not the variant this option reads back as: a flag needs
    /// [`OptionValue::Flag`], a number [`OptionValue::Number`], and every
    /// other kind [`OptionValue::Text`].
    WrongKind {
        /// What the option holds.
        expected: OptionKind,
    },
    /// The option holds one of a fixed set of words, and the value is none of
    /// them.
    NotAChoice {
        /// Every word the option accepts.
        choices: &'static [&'static str],
    },
    /// The number is outside the range the option accepts.
    OutOfRange {
        /// The inclusive range the option accepts.
        range: RangeInclusive<i64>,
    },
}

impl fmt::Display for OptionValueRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongKind { expected } => {
                let (holds, variant) = match expected {
                    OptionKind::Flag => ("a flag", "Flag"),
                    OptionKind::Number => ("a number", "Number"),
                    OptionKind::Choice => ("one of a fixed set of words", "Text"),
                    OptionKind::Text => ("text", "Text"),
                    OptionKind::Colour => ("a colour", "Text"),
                    OptionKind::Key => ("a key", "Text"),
                    OptionKind::Command => ("a command", "Text"),
                };
                write!(
                    formatter,
                    "it holds {holds}, written as OptionValue::{variant}"
                )
            }
            Self::NotAChoice { choices } => {
                write!(formatter, "it accepts only {}", choices.join(", "))
            }
            Self::OutOfRange { range } => write!(
                formatter,
                "it accepts {} through {}",
                range.start(),
                range.end()
            ),
        }
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
/// use libtmux_core::options::{OptionKind, option_schema};
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

    // tmux maps a few legacy spellings before it looks a name up, so both
    // spellings have to reach the same entry.
    let name = generated::OPTION_ALIASES
        .iter()
        .find_map(|(from, to)| (*from == name).then_some(*to))
        .unwrap_or(name);

    if let Ok(index) = generated::OPTION_SCHEMA.binary_search_by(|entry| entry.name.cmp(name)) {
        return Some(&generated::OPTION_SCHEMA[index]);
    }

    // tmux takes an unambiguous prefix of an option's name for the option, so
    // `mous` reaches `mouse`. Answering `None` for one would report a real
    // option as unknown, and anything deciding where a write lands would then
    // be deciding about a name tmux does not use.
    //
    // Reached only when the exact search missed, which is the rarer half.
    let mut matched = None;
    for entry in &generated::OPTION_SCHEMA {
        if entry.name.starts_with(name) {
            if matched.is_some() {
                // Ambiguous. tmux refuses these by name, and says so better.
                return None;
            }
            matched = Some(entry);
        }
    }
    matched
}

/// One option's value, typed by what tmux's own option table declares.
///
/// Flags and numbers use their declared kind. Choices, colours, keys, commands
/// and user options keep their original text bytes. [`Self::decode`] preserves
/// text even when it does not parse as the declared kind.
///
/// [`OptionSchema::check`] validates a typed value before an adapter writes it.
/// A choice such as `status` takes text (`"on"` or `"2"`), not a flag. Unknown
/// options have no schema and remain the adapter's responsibility.
///
/// # Examples
///
/// ```
/// use libtmux_core::TmuxText;
/// use libtmux_core::options::{OptionValue, option_schema};
///
/// assert!(option_schema("mouse").is_some_and(|schema| schema.check(&true.into()).is_ok()));
/// assert_eq!(OptionValue::decode("mouse", TmuxText::from("on")), OptionValue::Flag(true));
/// let status = OptionValue::decode("status", TmuxText::from("on"));
/// assert_eq!(TmuxText::from(status).as_bytes(), b"on");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OptionValue {
    /// A flag tmux wrote as `on` or `off`.
    Flag(
        /// `true` for `on`, `false` for `off`.
        bool,
    ),
    /// A number.
    Number(
        /// The option's numeric value, encoded as decimal for tmux.
        i64,
    ),
    /// Text, which covers choices, colours, keys, commands, and user options.
    ///
    /// tmux validates a choice when it is set, so a value read back is one
    /// tmux accepted. [`OptionSchema::choices`] lists the words a choice
    /// takes.
    Text(
        /// The option's bytes, preserved without UTF-8 conversion.
        TmuxText,
    ),
}

impl From<bool> for OptionValue {
    fn from(value: bool) -> Self {
        Self::Flag(value)
    }
}

// Only the integers every value of which fits tmux's `i64`: a `u64` or `usize`
// caller converts with `i64::try_from` and decides what an overflow means.
macro_rules! number_from {
    ($($integer:ty),*) => {$(
        impl From<$integer> for OptionValue {
            fn from(value: $integer) -> Self {
                Self::Number(i64::from(value))
            }
        }
    )*};
}

number_from!(i8, i16, i32, i64, u8, u16, u32);

impl From<&str> for OptionValue {
    fn from(value: &str) -> Self {
        Self::Text(TmuxText::from(value))
    }
}

impl From<String> for OptionValue {
    fn from(value: String) -> Self {
        Self::Text(TmuxText::from(value))
    }
}

impl From<TmuxText> for OptionValue {
    fn from(value: TmuxText) -> Self {
        Self::Text(value)
    }
}

/// The bytes tmux stores for a value: `on` or `off` for a flag, decimal for a
/// number, and text unchanged.
///
/// Exact for a value read back through `typed_option`, since tmux prints a
/// flag and a number in these same forms.
///
/// # Examples
///
/// ```
/// use libtmux_core::{TmuxText, options::OptionValue};
///
/// assert_eq!(TmuxText::from(OptionValue::Flag(true)), "on");
/// assert_eq!(TmuxText::from(OptionValue::Number(-3)), "-3");
/// assert_eq!(TmuxText::from(OptionValue::from("vi")), "vi");
/// ```
impl From<OptionValue> for TmuxText {
    fn from(value: OptionValue) -> Self {
        match value {
            OptionValue::Flag(true) => Self::from("on"),
            OptionValue::Flag(false) => Self::from("off"),
            OptionValue::Number(number) => Self::from(number.to_string()),
            OptionValue::Text(text) => text,
        }
    }
}

impl OptionValue {
    /// Decode a stored value according to an option's declared kind.
    ///
    /// A value that does not match its declared kind stays [`OptionValue::Text`]
    /// rather than being discarded, because tmux stored it and the caller may
    /// still want it.
    #[must_use]
    pub fn decode(name: &str, value: TmuxText) -> Self {
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
