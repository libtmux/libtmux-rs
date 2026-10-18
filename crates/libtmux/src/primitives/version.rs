//! Ordered tmux release values.

use std::fmt;

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum SuffixKind {
    ReleaseCandidate(u16),
    Final,
    Patch(u8),
}

/// The suffix of a numbered tmux release.
///
/// Release candidates sort before a final release, while lowercase patch
/// letters sort after it.
///
/// # Examples
///
/// ```
/// use libtmux::{ReleaseSuffix, ReleaseVersion};
///
/// // tmux's two suffix shapes order differently: a patch letter comes *after*
/// // the plain release, and a release candidate comes before it.
/// let plain = ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL);
/// let patched = ReleaseVersion::new(3, 5, ReleaseSuffix::patch('a').expect("a patch letter"));
/// let candidate = ReleaseVersion::new(
///     3,
///     5,
///     ReleaseSuffix::release_candidate(1).expect("one is nonzero"),
/// );
///
/// assert!(candidate < plain);
/// assert!(plain < patched);
///
/// // Only those two shapes exist, so anything else is rejected rather than
/// // guessed at.
/// assert_eq!(ReleaseSuffix::patch('B'), None);
/// assert_eq!(ReleaseSuffix::release_candidate(0), None);
/// ```
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseSuffix(SuffixKind);

impl ReleaseSuffix {
    /// An unnumbered `-rc` suffix, canonically equivalent to `-rc1`.
    pub const RELEASE_CANDIDATE: Self = Self(SuffixKind::ReleaseCandidate(1));

    /// A final release with no suffix.
    pub const FINAL: Self = Self(SuffixKind::Final);

    /// The first lowercase patch suffix, `a`.
    pub const PATCH_A: Self = Self(SuffixKind::Patch(b'a'));

    /// Construct a numbered release-candidate suffix.
    ///
    /// Candidate one displays canonically as `-rc`. Zero is not a valid
    /// release-candidate number.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::ReleaseSuffix;
    ///
    /// let suffix = ReleaseSuffix::release_candidate(2).expect("two is nonzero");
    /// assert_eq!(suffix.release_candidate_number(), Some(2));
    /// assert_eq!(ReleaseSuffix::release_candidate(0), None);
    /// ```
    #[must_use]
    pub const fn release_candidate(number: u16) -> Option<Self> {
        if number == 0 {
            None
        } else {
            Some(Self(SuffixKind::ReleaseCandidate(number)))
        }
    }

    /// Construct a lowercase patch-letter suffix.
    ///
    /// Returns `None` when `letter` is not an ASCII lowercase letter.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::ReleaseSuffix;
    ///
    /// let suffix = ReleaseSuffix::patch('b').expect("b is a patch letter");
    /// assert_eq!(suffix.patch_letter(), Some('b'));
    /// assert_eq!(ReleaseSuffix::patch('B'), None);
    /// ```
    #[must_use]
    pub fn patch(letter: char) -> Option<Self> {
        if !letter.is_ascii_lowercase() {
            return None;
        }

        u8::try_from(letter as u32)
            .ok()
            .map(|letter| Self(SuffixKind::Patch(letter)))
    }

    /// Return the lowercase patch letter, if this is a patch release.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::ReleaseSuffix;
    ///
    /// assert_eq!(ReleaseSuffix::FINAL.patch_letter(), None);
    /// assert_eq!(ReleaseSuffix::patch('a').and_then(|value| value.patch_letter()), Some('a'));
    /// ```
    #[must_use]
    pub fn patch_letter(self) -> Option<char> {
        match self.0 {
            SuffixKind::Patch(letter) => Some(char::from(letter)),
            SuffixKind::ReleaseCandidate(_) | SuffixKind::Final => None,
        }
    }

    /// Return the release-candidate number, if present.
    ///
    /// The unnumbered `-rc` spelling has the effective number one.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::ReleaseSuffix;
    ///
    /// assert_eq!(ReleaseSuffix::RELEASE_CANDIDATE.release_candidate_number(), Some(1));
    /// assert_eq!(ReleaseSuffix::FINAL.release_candidate_number(), None);
    /// ```
    #[must_use]
    pub const fn release_candidate_number(self) -> Option<u16> {
        match self.0 {
            SuffixKind::ReleaseCandidate(number) => Some(number),
            SuffixKind::Final | SuffixKind::Patch(_) => None,
        }
    }

    /// Return whether this suffix marks a final release.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::ReleaseSuffix;
    ///
    /// assert!(ReleaseSuffix::FINAL.is_final());
    /// assert!(!ReleaseSuffix::RELEASE_CANDIDATE.is_final());
    /// ```
    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self.0, SuffixKind::Final)
    }

    /// Return whether this suffix marks a release candidate.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::ReleaseSuffix;
    ///
    /// assert!(ReleaseSuffix::RELEASE_CANDIDATE.is_release_candidate());
    /// assert!(!ReleaseSuffix::FINAL.is_release_candidate());
    /// ```
    #[must_use]
    pub const fn is_release_candidate(self) -> bool {
        matches!(self.0, SuffixKind::ReleaseCandidate(_))
    }
}

impl fmt::Debug for ReleaseSuffix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            SuffixKind::ReleaseCandidate(number) => formatter
                .debug_tuple("ReleaseCandidate")
                .field(&number)
                .finish(),
            SuffixKind::Final => formatter.write_str("Final"),
            SuffixKind::Patch(letter) => formatter
                .debug_tuple("Patch")
                .field(&char::from(letter))
                .finish(),
        }
    }
}

impl fmt::Display for ReleaseSuffix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            SuffixKind::ReleaseCandidate(1) => formatter.write_str("-rc"),
            SuffixKind::ReleaseCandidate(number) => write!(formatter, "-rc{number}"),
            SuffixKind::Final => Ok(()),
            SuffixKind::Patch(letter) => write!(formatter, "{}", char::from(letter)),
        }
    }
}

/// A numbered tmux release.
///
/// # Examples
///
/// ```
/// use libtmux::{ReleaseSuffix, ReleaseVersion};
///
/// // tmux numbers a patch with a letter, so `3.5a` is later than `3.5`.
/// let plain = ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL);
/// let patched = ReleaseVersion::new(3, 5, ReleaseSuffix::patch('a').expect("a patch letter"));
/// assert!(patched > plain);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseVersion {
    major: u16,
    minor: u16,
    suffix: ReleaseSuffix,
}

impl ReleaseVersion {
    /// Construct a numbered tmux release.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{ReleaseSuffix, ReleaseVersion};
    ///
    /// let release = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);
    /// assert_eq!(release.to_string(), "3.7");
    /// ```
    #[must_use]
    pub const fn new(major: u16, minor: u16, suffix: ReleaseSuffix) -> Self {
        Self {
            major,
            minor,
            suffix,
        }
    }

    /// Return the major release number.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{ReleaseSuffix, ReleaseVersion};
    ///
    /// let release = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);
    /// assert_eq!(release.major(), 3);
    /// ```
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Return the minor release number.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{ReleaseSuffix, ReleaseVersion};
    ///
    /// let release = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);
    /// assert_eq!(release.minor(), 7);
    /// ```
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Return the release suffix.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{ReleaseSuffix, ReleaseVersion};
    ///
    /// let release = ReleaseVersion::new(3, 7, ReleaseSuffix::RELEASE_CANDIDATE);
    /// assert_eq!(release.suffix(), ReleaseSuffix::RELEASE_CANDIDATE);
    /// ```
    #[must_use]
    pub const fn suffix(self) -> ReleaseSuffix {
        self.suffix
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}{}", self.major, self.minor, self.suffix)
    }
}
