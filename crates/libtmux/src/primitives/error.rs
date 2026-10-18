use std::fmt;

/// An invalid scope-specific tmux object ID.
///
/// The error records the expected sigil but never retains the rejected input.
///
/// # Examples
///
/// ```
/// use libtmux::SessionId;
///
/// // The sigil is the whole difference between the id types, so a mistake
/// // names the one that was expected.
/// let error = "@1".parse::<SessionId>().expect_err("@ denotes a window");
/// assert_eq!(error.expected_sigil(), '$');
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct IdParseError {
    expected_sigil: char,
}

impl IdParseError {
    /// Construct an error for a scope-specific ID decoder.
    ///
    /// The rejected input is never retained.
    #[must_use]
    pub const fn new(expected_sigil: char) -> Self {
        Self { expected_sigil }
    }

    /// Return the sigil required by the requested ID scope.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::SessionId;
    ///
    /// let error = "@1".parse::<SessionId>().expect_err("@ denotes a window");
    /// assert_eq!(error.expected_sigil(), '$');
    /// ```
    #[must_use]
    pub const fn expected_sigil(self) -> char {
        self.expected_sigil
    }
}

impl fmt::Display for IdParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid tmux ID: expected {} followed by an integer from 0 through {}",
            self.expected_sigil,
            u32::MAX,
        )
    }
}

impl std::error::Error for IdParseError {}
