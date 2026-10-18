use std::borrow::Cow;

/// Captured tmux bytes; UTF-8 decoding is a caller's choice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmuxText(Vec<u8>);

impl TmuxText {
    /// Retain bytes without changing or validating them.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// Borrow the original bytes, including invalid UTF-8.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Borrow the text if it is valid UTF-8.
    ///
    /// # Errors
    /// Returns [`std::str::Utf8Error`] for bytes that are not valid UTF-8.
    pub fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.0)
    }

    /// Replace invalid UTF-8 with the replacement character for display.
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }
}
