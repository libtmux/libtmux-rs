use std::{fmt, str::FromStr};

/// A native tmux pane ID such as `%1`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PaneId(u32);

/// A string that is not a `%` followed by a 32-bit unsigned integer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdParseError;

impl fmt::Display for IdParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected a pane ID such as %1")
    }
}

impl std::error::Error for IdParseError {}

impl FromStr for PaneId {
    type Err = IdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let digits = value.strip_prefix('%').ok_or(IdParseError)?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(IdParseError);
        }
        digits.parse().map(Self).map_err(|_| IdParseError)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "%{}", self.0)
    }
}
