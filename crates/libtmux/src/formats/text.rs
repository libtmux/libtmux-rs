use std::borrow::Cow;
use std::fmt;

/// Immutable text bytes returned by tmux.
///
/// `TmuxText` preserves bytes exactly. Callers choose whether to inspect the
/// raw bytes, require UTF-8, or decode lossily.
///
/// # Examples
///
/// ```
/// use libtmux::TmuxText;
///
/// let borrowed = TmuxText::from("session");
/// let owned = TmuxText::from(String::from("window"));
/// let bytes = TmuxText::from(vec![b'p', 0xff]);
///
/// assert_eq!(borrowed.as_bytes(), b"session");
/// assert_eq!(owned.as_bytes(), b"window");
/// assert_eq!(bytes.as_bytes(), [b'p', 0xff]);
/// ```
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TmuxText {
    bytes: Vec<u8>,
}

impl TmuxText {
    /// Construct a value without validating or changing its bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxText;
    ///
    /// let text = TmuxText::from_bytes(vec![0, 0xff]);
    /// assert_eq!(text.as_bytes(), [0, 0xff]);
    /// ```
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
        }
    }

    /// Return the exact stored bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxText;
    ///
    /// let text = TmuxText::from_bytes(vec![b'a', 0xff]);
    /// assert_eq!(text.as_bytes(), [b'a', 0xff]);
    /// ```
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Borrow the stored bytes as UTF-8.
    ///
    /// # Errors
    ///
    /// Returns [`std::str::Utf8Error`] when the stored bytes are not valid
    /// UTF-8.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxText;
    ///
    /// let text = TmuxText::from("session");
    /// assert_eq!(text.as_str(), Ok("session"));
    /// ```
    pub fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.bytes)
    }

    /// Decode the stored bytes with replacement characters for invalid UTF-8.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxText;
    ///
    /// let text = TmuxText::from_bytes(vec![b'a', 0xff]);
    /// assert_eq!(text.to_string_lossy(), "a\u{fffd}");
    /// ```
    #[must_use]
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    /// Interpret these bytes as a tmux flag.
    ///
    /// tmux writes flags as `on` and `off`, and accepts `yes` and `no` for
    /// some. Anything else returns `None` rather than guessing, because an
    /// option holding arbitrary text is not a flag.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxText;
    ///
    /// assert_eq!(TmuxText::from("on").as_flag(), Some(true));
    /// assert_eq!(TmuxText::from("off").as_flag(), Some(false));
    /// assert_eq!(TmuxText::from("sometimes").as_flag(), None);
    /// ```
    #[must_use]
    pub fn as_flag(&self) -> Option<bool> {
        match self.as_bytes() {
            b"on" | b"yes" | b"1" => Some(true),
            b"off" | b"no" | b"0" => Some(false),
            _ => None,
        }
    }

    /// Parse these bytes as any type that reads from a string.
    ///
    /// Returns `None` when the bytes are not UTF-8 or do not parse, so a
    /// caller reading a numeric option does not have to check both.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxText;
    ///
    /// assert_eq!(TmuxText::from("2000").parse::<u32>(), Some(2000));
    /// assert_eq!(TmuxText::from("many").parse::<u32>(), None);
    /// ```
    #[must_use]
    pub fn parse<T: std::str::FromStr>(&self) -> Option<T> {
        self.as_str().ok()?.parse().ok()
    }
}

impl From<&str> for TmuxText {
    fn from(value: &str) -> Self {
        Self::from_bytes(value.as_bytes())
    }
}

impl From<String> for TmuxText {
    fn from(value: String) -> Self {
        Self::from_bytes(value.into_bytes())
    }
}

/// Compare tmux bytes against a string without converting either.
///
/// tmux text is bytes, but the thing it is usually compared against is a
/// literal. Requiring `.as_bytes()` at every comparison put the burden on the
/// common case to serve the rare one.
///
/// Text that is not valid UTF-8 never equals a `str`, which is the honest
/// answer: a `str` cannot hold those bytes, so nothing it contains can match.
///
/// # Examples
///
/// ```
/// use libtmux::TmuxText;
///
/// assert_eq!(TmuxText::from("editor"), *"editor");
/// assert!(TmuxText::from("editor") == "editor");
/// assert!(TmuxText::from(vec![0xff]) != "\u{fffd}");
/// ```
impl PartialEq<str> for TmuxText {
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for TmuxText {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

/// Compare tmux bytes against bytes, for a name that is not a literal.
impl PartialEq<[u8]> for TmuxText {
    fn eq(&self, other: &[u8]) -> bool {
        self.as_bytes() == other
    }
}

impl PartialEq<&[u8]> for TmuxText {
    fn eq(&self, other: &&[u8]) -> bool {
        self == *other
    }
}

impl PartialEq<TmuxText> for [u8] {
    fn eq(&self, other: &TmuxText) -> bool {
        other == self
    }
}

impl PartialEq<TmuxText> for str {
    fn eq(&self, other: &TmuxText) -> bool {
        other == self
    }
}

impl PartialEq<TmuxText> for &str {
    fn eq(&self, other: &TmuxText) -> bool {
        other == *self
    }
}

impl From<Vec<u8>> for TmuxText {
    fn from(value: Vec<u8>) -> Self {
        Self::from_bytes(value)
    }
}

impl fmt::Debug for TmuxText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TmuxText(<redacted>)")
    }
}
