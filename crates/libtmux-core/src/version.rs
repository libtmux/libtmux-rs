//! Tmux release parsing and capability checks.

use std::cmp::Ordering;
use std::fmt;
use std::str;

/// A version report that cannot qualify a tmux operation.
///
/// Invalid output retains only its length. An unrecognised report retains a
/// token only after the version decoder bounds and validates its bytes.
///
/// # Examples
///
/// ```
/// use libtmux_core::{TmuxVersion, VersionError};
///
/// let error = TmuxVersion::parse_output(b"invalid\n").expect_err("no version frame");
/// assert!(matches!(error, VersionError::InvalidVersionOutput { output_len: 8 }));
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VersionError {
    /// The output is not a valid tmux version report.
    InvalidVersionOutput {
        /// The number of bytes read, without retaining the output.
        output_len: usize,
    },
    /// The reported release is below the supported floor.
    UnsupportedTmuxVersion {
        /// The detected version.
        found: TmuxVersion,
        /// The minimum supported release.
        minimum: ReleaseVersion,
    },
    /// A bounded report names no release the library can qualify.
    UnrecognizedTmuxVersion {
        /// The bounded version token reported by tmux.
        found: TmuxVersion,
    },
    /// A supported release predates the requested capability.
    UnsupportedCapability {
        /// The capability being checked.
        capability: &'static str,
        /// The first release carrying the capability.
        needs: ReleaseVersion,
        /// The detected version.
        found: TmuxVersion,
    },
}

impl VersionError {
    fn from_invalid_version_output(output_len: usize) -> Self {
        Self::InvalidVersionOutput { output_len }
    }

    fn unrecognized_tmux_version(found: TmuxVersion) -> Self {
        Self::UnrecognizedTmuxVersion { found }
    }

    fn unsupported_tmux_version(found: TmuxVersion, minimum: ReleaseVersion) -> Self {
        Self::UnsupportedTmuxVersion { found, minimum }
    }

    /// Return the invalid output's byte length, when present.
    #[must_use]
    pub const fn invalid_version_output_len(&self) -> Option<usize> {
        match self {
            Self::InvalidVersionOutput { output_len } => Some(*output_len),
            _ => None,
        }
    }

    /// Borrow the version reported by a floor or capability failure.
    #[must_use]
    pub const fn found_version(&self) -> Option<&TmuxVersion> {
        match self {
            Self::UnsupportedTmuxVersion { found, .. }
            | Self::UnrecognizedTmuxVersion { found }
            | Self::UnsupportedCapability { found, .. } => Some(found),
            Self::InvalidVersionOutput { .. } => None,
        }
    }

    /// Borrow the required release, when the failure names one.
    #[must_use]
    pub const fn minimum_version(&self) -> Option<&ReleaseVersion> {
        match self {
            Self::UnsupportedTmuxVersion { minimum, .. } => Some(minimum),
            Self::UnsupportedCapability { needs, .. } => Some(needs),
            Self::InvalidVersionOutput { .. } | Self::UnrecognizedTmuxVersion { .. } => None,
        }
    }
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVersionOutput { .. } => formatter.write_str("invalid tmux version output"),
            Self::UnsupportedTmuxVersion { found, minimum } => write!(
                formatter,
                "tmux {found} is not supported; libtmux requires tmux {minimum} or newer. \
                 Upgrade tmux and restart the server, or pin the previous release of libtmux",
            ),
            Self::UnrecognizedTmuxVersion { found } => write!(
                formatter,
                "tmux reports the version \"{found}\", which names no release libtmux can compare \
                 with the supported minimum; run a released tmux build and restart the server",
            ),
            Self::UnsupportedCapability {
                capability,
                needs,
                found,
            } => write!(
                formatter,
                "{capability} needs tmux {needs} or newer, and this is {found}",
            ),
        }
    }
}

impl std::error::Error for VersionError {}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum SuffixKind {
    /// Master before the release branch is cut: `next-X.Y`.
    Next,
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
/// use libtmux_core::{ReleaseSuffix, ReleaseVersion};
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

    /// A development build of the release, ranked below its candidates.
    pub(crate) const NEXT: Self = Self(SuffixKind::Next);

    /// The first lowercase patch suffix, shared by capability tables.
    #[doc(hidden)]
    pub const PATCH_A: Self = Self(SuffixKind::Patch(b'a'));

    /// Construct a numbered release-candidate suffix.
    ///
    /// Candidate one displays canonically as `-rc`. Zero is not a valid
    /// release-candidate number.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::ReleaseSuffix;
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
    /// use libtmux_core::ReleaseSuffix;
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
    /// use libtmux_core::ReleaseSuffix;
    ///
    /// assert_eq!(ReleaseSuffix::FINAL.patch_letter(), None);
    /// assert_eq!(ReleaseSuffix::patch('a').and_then(|value| value.patch_letter()), Some('a'));
    /// ```
    #[must_use]
    pub fn patch_letter(self) -> Option<char> {
        match self.0 {
            SuffixKind::Patch(letter) => Some(char::from(letter)),
            SuffixKind::Next | SuffixKind::ReleaseCandidate(_) | SuffixKind::Final => None,
        }
    }

    /// Return the release-candidate number, if present.
    ///
    /// The unnumbered `-rc` spelling has the effective number one.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::ReleaseSuffix;
    ///
    /// assert_eq!(ReleaseSuffix::RELEASE_CANDIDATE.release_candidate_number(), Some(1));
    /// assert_eq!(ReleaseSuffix::FINAL.release_candidate_number(), None);
    /// ```
    #[must_use]
    pub const fn release_candidate_number(self) -> Option<u16> {
        match self.0 {
            SuffixKind::ReleaseCandidate(number) => Some(number),
            SuffixKind::Next | SuffixKind::Final | SuffixKind::Patch(_) => None,
        }
    }

    /// Return whether this suffix marks a final release.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::ReleaseSuffix;
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
    /// use libtmux_core::ReleaseSuffix;
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
            SuffixKind::Next => formatter.write_str("Next"),
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
            SuffixKind::Next => formatter.write_str("-next"),
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
/// use libtmux_core::{ReleaseSuffix, ReleaseVersion};
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
    /// use libtmux_core::{ReleaseSuffix, ReleaseVersion};
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
    /// use libtmux_core::{ReleaseSuffix, ReleaseVersion};
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
    /// use libtmux_core::{ReleaseSuffix, ReleaseVersion};
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
    /// use libtmux_core::{ReleaseSuffix, ReleaseVersion};
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

/// A parsed raw tmux version.
///
/// Numbered releases expose an ordered [`ReleaseVersion`]. Development
/// identifiers preserve their token and remain unordered relative to numbered
/// releases.
///
/// # Examples
///
/// ```
/// use libtmux_core::TmuxVersion;
///
/// let version = TmuxVersion::parse_output(b"tmux next-3.8\n")?;
/// assert!(version.is_development());
/// assert_eq!(version.raw(), "next-3.8");
/// # Ok::<(), libtmux_core::VersionError>(())
/// ```
#[derive(Clone, Debug)]
pub struct TmuxVersion {
    raw: Box<str>,
    release: Option<ReleaseVersion>,
}

/// The longest version token an unrecognized report may carry into an error.
const MAX_REPORTED_TOKEN: usize = 64;

impl TmuxVersion {
    /// The minimum supported tmux release, 3.3a.
    pub const MIN_SUPPORTED: ReleaseVersion = ReleaseVersion::new(3, 3, ReleaseSuffix::PATCH_A);

    /// Parse the exact output from `tmux -V`.
    ///
    /// The output must contain one nonempty program-name token, one version
    /// token, and one trailing newline. Invalid process output is never stored
    /// in the returned error.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError::InvalidVersionOutput`] for malformed framing or version
    /// syntax, unsupported characters, extra lines, or invalid UTF-8.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.7b\n")?;
    /// assert_eq!(version.raw(), "3.7b");
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    pub fn parse_output(output: &[u8]) -> Result<Self, VersionError> {
        let raw = parse_output_frame(output)
            .ok_or_else(|| VersionError::from_invalid_version_output(output.len()))?;

        let release = match raw.as_bytes().first() {
            Some(first) if first.is_ascii_digit() => Some(
                parse_release(raw)
                    .ok_or_else(|| VersionError::from_invalid_version_output(output.len()))?,
            ),
            Some(_) if is_development_identifier(raw) => None,
            _ => return Err(VersionError::from_invalid_version_output(output.len())),
        };

        Ok(Self {
            raw: raw.into(),
            release,
        })
    }

    /// Read the version a daemon or `tmux -V` reported, for the version check.
    ///
    /// Strict like [`TmuxVersion::parse_output`], except that a well-framed
    /// report whose token names no release is
    /// [`VersionError::UnrecognizedTmuxVersion`] rather than invalid output: the
    /// daemon answered, and the answer cannot be compared with the floor. The
    /// token is kept for the message only when it is at most
    /// 64 printable ASCII bytes. Anything else is
    /// [`VersionError::InvalidVersionOutput`] with `reported_len` as its length.
    #[doc(hidden)]
    pub fn parse_report(output: &[u8], reported_len: usize) -> Result<Self, VersionError> {
        match Self::parse_output(output) {
            Err(VersionError::InvalidVersionOutput { .. }) => {
                match parse_output_frame(output).filter(|token| {
                    token.len() <= MAX_REPORTED_TOKEN && token.bytes().all(|b| b.is_ascii_graphic())
                }) {
                    Some(token) => Err(VersionError::unrecognized_tmux_version(Self {
                        raw: token.into(),
                        release: None,
                    })),
                    None => Err(VersionError::from_invalid_version_output(reported_len)),
                }
            }
            other => other.map_err(|error| match error {
                VersionError::InvalidVersionOutput { .. } => {
                    VersionError::from_invalid_version_output(reported_len)
                }
                other => other,
            }),
        }
    }

    /// Return the exact version token without the program-name prefix or newline.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux next-3.8\n")?;
    /// assert_eq!(version.raw(), "next-3.8");
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Return the numbered release, or `None` for a development identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::TmuxVersion;
    ///
    /// let release = TmuxVersion::parse_output(b"tmux 3.7\n")?;
    /// let development = TmuxVersion::parse_output(b"tmux master\n")?;
    /// assert_eq!(release.release().map(|value| value.major()), Some(3));
    /// assert_eq!(development.release(), None);
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    #[must_use]
    pub const fn release(&self) -> Option<&ReleaseVersion> {
        self.release.as_ref()
    }

    /// Return whether this is a development identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux master\n")?;
    /// assert!(version.is_development());
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    #[must_use]
    pub const fn is_development(&self) -> bool {
        self.release.is_none()
    }

    /// Return the release whose in-tree behavior this version carries.
    ///
    /// A numbered release returns itself. A `next-X.Y` identifier returns
    /// `X.Y` ranked below its release candidates: its tree is master before
    /// the release branch is cut, so it carries what shipped in earlier
    /// releases but not yet the changes that complete `X.Y`. `master` returns
    /// `None` because it names no release.
    ///
    /// Unlike [`TmuxVersion::meets`], this does not clamp development
    /// identifiers to the crate's minimum supported release. It answers "which
    /// tmux source is running", not "may this version be relied on", so it
    /// suits behavior windows that both open and close at known releases.
    #[doc(hidden)]
    #[must_use]
    pub fn behavior_release(&self) -> Option<ReleaseVersion> {
        self.release.or_else(|| parse_next_release(&self.raw))
    }

    /// Return whether this version supports a required numbered release.
    ///
    /// Development identifiers meet only requirements at or below the crate's
    /// minimum supported release; they are not promoted to an invented release.
    /// A `next-X.Y` identifier's own release number is not read here even
    /// when `required` is that same `X.Y` or earlier -- this is a floor
    /// check, not a capability check. Asking "does this tree already contain
    /// a capability that shipped at version V" is [`Self::has_behavior`]'s
    /// question, not this one; a call site gating a specific capability
    /// behind a release above [`Self::MIN_SUPPORTED`] almost always wants
    /// that instead, or it refuses a development build that already has the
    /// capability it is checking for.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.3a\n")?;
    /// assert!(version.meets(&TmuxVersion::MIN_SUPPORTED));
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    ///
    /// The English verb suggests this answers "is a capability available",
    /// but on a development build it can refuse one that is already there:
    ///
    /// ```
    /// use libtmux_core::{ReleaseSuffix, ReleaseVersion, TmuxVersion};
    ///
    /// let next = TmuxVersion::parse_output(b"tmux next-3.9\n")?;
    /// let capture_line_flags = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);
    ///
    /// // `next-3.9`'s tree already has 3.7's behavior, but this clamps every
    /// // development identifier to the crate's floor, so it says no anyway.
    /// // `TmuxVersion::has_behavior` is the question that says yes.
    /// assert!(!next.meets(&capture_line_flags));
    /// assert!(next.has_behavior(&capture_line_flags));
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    #[must_use]
    pub fn meets(&self, required: &ReleaseVersion) -> bool {
        match self.release {
            Some(release) => release >= *required,
            None if *required > Self::MIN_SUPPORTED => false,
            None if self.raw.as_ref() == "master" => true,
            None => parse_next_release(&self.raw).is_some_and(|release| release >= *required),
        }
    }

    /// Report whether this version's tree carries a capability's behavior.
    ///
    /// The shared rule behind `Self::require`: a development identifier
    /// asks its `behavior_release`, never clamped to the crate's minimum
    /// supported release. Both this and `require` call it, so they cannot
    /// drift apart the way two independently written rules could.
    fn behavior_satisfies(&self, needs: ReleaseVersion) -> bool {
        self.behavior_release()
            .is_none_or(|release| release >= needs)
    }

    /// Report whether this version's tree carries a capability's behavior.
    ///
    /// This is `Self::require`'s refusal rule in boolean form, so a caller
    /// gating a specific capability -- or a test predicting which branch
    /// `require` takes -- uses the same rule `require` does, rather than an
    /// independently derived one that can drift from it. [`Self::meets`] is
    /// a different, deliberately non-identical rule: it clamps *every*
    /// development identifier -- `next-X.Y` included, not only a bare
    /// `master` -- to "no more than the crate's minimum supported release",
    /// so it refuses any requirement above that floor regardless of what the
    /// build's own next-release number is. This reads `next-X.Y` as the real
    /// release `X.Y` instead, so the two disagree for any development build
    /// checked against a requirement above the floor: [`crate::since`] holds
    /// several, and this crate's own tmux-matrix probe self-reports
    /// `next-3.9`. Checking a capability with `meets` instead of this
    /// refuses that capability on a development build that already has it.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::{ReleaseSuffix, ReleaseVersion, TmuxVersion};
    ///
    /// let next = TmuxVersion::parse_output(b"tmux next-3.9\n")?;
    /// let capture_line_flags = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);
    ///
    /// // `next-3.9` already contains 3.7's behavior, so this reports it --
    /// // unlike `meets`, which clamps every development identifier to the
    /// // floor and would refuse it.
    /// assert!(next.has_behavior(&capture_line_flags));
    /// assert!(!next.meets(&capture_line_flags));
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    #[must_use]
    pub fn has_behavior(&self, needs: &ReleaseVersion) -> bool {
        self.behavior_satisfies(*needs)
    }

    /// Check the qualified daemon floor and an established native client match.
    ///
    /// The daemon must name a supported release ranking at or above 3.7 final.
    /// `native_pair_compatible` is the caller's evidence about the configured
    /// client and daemon; pass `false` when it is unknown. Version numbers alone
    /// do not establish native protocol compatibility for an arbitrary pair.
    /// This does not qualify a one-shot control route on older daemons.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::TmuxVersion;
    ///
    /// let daemon = TmuxVersion::parse_output(b"tmux 3.7\n")?;
    /// assert!(!daemon.managed_control_eligible(false));
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    #[must_use]
    pub fn managed_control_eligible(&self, native_pair_compatible: bool) -> bool {
        native_pair_compatible
            && self.ensure_supported().is_ok()
            && self
                .behavior_release()
                .is_some_and(|release| release >= since::CONTROL_ACQUISITION)
    }

    /// Refuse a capability this release is too old for.
    ///
    /// tmux usually accepts an unknown flag and ignores it, so without this
    /// "your tmux is too old" arrives as "the command did nothing". A
    /// development build carries no numbered release to compare and is taken
    /// at its word rather than refused.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError::UnsupportedCapability`] when this release is below
    /// `needs`.
    #[doc(hidden)]
    pub fn require(
        &self,
        capability: &'static str,
        needs: ReleaseVersion,
    ) -> Result<(), VersionError> {
        if !self.behavior_satisfies(needs) {
            return Err(VersionError::UnsupportedCapability {
                capability,
                needs,
                found: self.clone(),
            });
        }

        Ok(())
    }

    /// Require a numbered release to meet the minimum supported version.
    ///
    /// A numbered `next-X.Y` report can establish the floor. An undated
    /// `master` report remains diagnostic and cannot qualify dispatch.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError::UnsupportedTmuxVersion`] when the detected release is
    /// below tmux 3.3a, and [`VersionError::UnrecognizedTmuxVersion`] for a bare
    /// `master`, which names no release. The version check also refuses a
    /// daemon report that is well framed but names no release at all
    /// (`openbsd-*`, `3.3a-dev`) as [`VersionError::UnrecognizedTmuxVersion`], before
    /// a [`TmuxVersion`] exists to call this on.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.3a\n")?;
    /// version.ensure_supported()?;
    /// # Ok::<(), libtmux_core::VersionError>(())
    /// ```
    pub fn ensure_supported(&self) -> Result<(), VersionError> {
        match self.release.or_else(|| parse_next_release(&self.raw)) {
            Some(release) if release >= Self::MIN_SUPPORTED => Ok(()),
            Some(_) => Err(VersionError::unsupported_tmux_version(
                self.clone(),
                Self::MIN_SUPPORTED,
            )),
            None => Err(VersionError::unrecognized_tmux_version(self.clone())),
        }
    }

    /// The version of a daemon too old to run the version probe.
    ///
    /// `run-shell -C` arrived in tmux 3.2, so an older daemon refuses the
    /// probe's flag and never reports a version of its own.
    #[doc(hidden)]
    #[must_use]
    pub fn older_than_probe() -> Self {
        Self {
            raw: "older than 3.2".into(),
            release: None,
        }
    }
}

impl fmt::Display for TmuxVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

impl PartialEq for TmuxVersion {
    fn eq(&self, other: &Self) -> bool {
        match (self.release, other.release) {
            (Some(left), Some(right)) => left == right,
            (None, None) => self.raw == other.raw,
            (Some(_), None) | (None, Some(_)) => false,
        }
    }
}

impl Eq for TmuxVersion {}

impl PartialOrd for TmuxVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self.release, other.release) {
            (Some(left), Some(right)) => Some(left.cmp(&right)),
            (None, None) if self.raw == other.raw => Some(Ordering::Equal),
            _ => None,
        }
    }
}

fn parse_release(raw: &str) -> Option<ReleaseVersion> {
    let (major, remainder) = raw.split_once('.')?;
    let minor_end = remainder
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(remainder.len());
    let (minor, suffix) = remainder.split_at(minor_end);
    let major = parse_u16_component(major)?;
    let minor = parse_u16_component(minor)?;
    let suffix = parse_release_suffix(suffix)?;

    Some(ReleaseVersion::new(major, minor, suffix))
}

fn is_development_identifier(raw: &str) -> bool {
    raw == "master" || parse_next_release(raw).is_some()
}

fn parse_next_release(raw: &str) -> Option<ReleaseVersion> {
    let (major, minor) = raw.strip_prefix("next-")?.split_once('.')?;
    Some(ReleaseVersion::new(
        parse_u16_component(major)?,
        parse_u16_component(minor)?,
        ReleaseSuffix::NEXT,
    ))
}

fn parse_release_suffix(suffix: &str) -> Option<ReleaseSuffix> {
    if suffix.is_empty() {
        return Some(ReleaseSuffix::FINAL);
    }
    if suffix == "-rc" {
        return Some(ReleaseSuffix::RELEASE_CANDIDATE);
    }
    if let Some(number) = suffix.strip_prefix("-rc") {
        return ReleaseSuffix::release_candidate(parse_u16_component(number)?);
    }
    match suffix.as_bytes() {
        [letter] if letter.is_ascii_lowercase() => Some(ReleaseSuffix(SuffixKind::Patch(*letter))),
        _ => None,
    }
}

fn parse_u16_component(component: &str) -> Option<u16> {
    if component.is_empty()
        || (component.len() > 1 && component.starts_with('0'))
        || !component.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    component.parse().ok()
}

fn parse_output_frame(output: &[u8]) -> Option<&str> {
    let line = output.strip_suffix(b"\n")?;
    let separator = line.iter().position(|byte| *byte == b' ')?;
    let program = line.get(..separator)?;
    let version = line.get(separator + 1..)?;
    if program.is_empty()
        || version.is_empty()
        || program
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        || version
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return None;
    }
    str::from_utf8(version).ok()
}

/// The tmux release each version-gated capability arrived in.
///
/// Every method that can answer [`crate::VersionError::UnsupportedCapability`] names
/// its floor here, so a caller can ask before it calls rather than learning
/// from the failure. Compare with [`TmuxVersion::meets`].
///
/// # Examples
///
/// ```
/// use libtmux_core::{TmuxVersion, since};
///
/// let version = TmuxVersion::parse_output(b"tmux 3.7\n")?;
/// assert!(version.has_behavior(&since::CAPTURE_LINE_FLAGS));
/// # Ok::<(), libtmux_core::VersionError>(())
/// ```
pub mod since {
    use super::{ReleaseSuffix, ReleaseVersion};

    /// Exact `delete-buffer -b` and `set-buffer -b -n` identity.
    ///
    /// Older daemons fall back to the top buffer when the requested name is
    /// absent. Managed temporary staging needs this before creating a buffer,
    /// because abort cleanup can race another client removing its name.
    pub const NAMED_BUFFER_IDENTITY: ReleaseVersion =
        ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL);

    /// `refresh-client -l`, which asks an attached client's terminal for its
    /// clipboard.
    ///
    /// On 3.3a the request stops the server when the client's terminal is
    /// `xterm-256color` or `tmux-256color`. On 3.4 through 3.6 the request
    /// returns, but the server stops if the client leaves before its terminal
    /// answers. 3.6a is the first release where neither happens. Raw commands
    /// carrying `-l` are refused below this release.
    pub const CLIPBOARD_QUERY: ReleaseVersion = ReleaseVersion::new(3, 6, ReleaseSuffix::PATCH_A);

    /// Managed control acquisition, decided before `-C`.
    ///
    /// Any build ranking at or above 3.7 final is eligible, including
    /// `3.8` and `next-3.8`; `next-3.7` and 3.7 release candidates rank below
    /// it.
    /// [`crate::TmuxVersion::managed_control_eligible`] also requires the
    /// configured client and daemon to be natively compatible.
    pub const CONTROL_ACQUISITION: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// `pane-border-style` and `pane-active-border-style` becoming pane
    /// options as well as window ones, below it tmux stores them only per window, and a
    /// write sent with `-p` lands there instead -- reporting success for a change
    /// the caller did not ask for. See `OptionSchema::accepts`.
    pub const PANE_BORDER_STYLE_PER_PANE: ReleaseVersion =
        ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// `capture-pane -F`, and so `Pane::capture_lines`.
    pub const CAPTURE_LINE_FLAGS: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// `list-keys -F`, and so `Server::typed_key_bindings`.
    ///
    /// Below this release `list-keys` prints only its own `bind-key` lines,
    /// which quote a key and leave a table name bare, and carry no note.
    pub const LIST_KEYS_FORMAT: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// Taking a pane out of a control client's stream without crashing the
    /// server, and so `control::ControlSender::mute_pane` using `off`.
    ///
    /// Before this release `refresh-client -A <pane>:off` leaves the output
    /// blocks already queued for that pane in place, while the pane stops
    /// holding the server's read buffer back. Writing those blocks later
    /// reads past the end of a buffer the server has drained, and the server
    /// segfaults. `mute_pane` pauses the pane below this release instead,
    /// which discards the queue on every supported release.
    pub const CONTROL_PANE_OFF: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// `capture-pane -T`, and so `CaptureOptions::trim_blank_cells`.
    ///
    /// Added in 3.4 to stop at the last used cell instead of the pane's full
    /// width. The option string is `ab:CeE:JNpPqS:t:` on 3.2a and gains `T`
    /// by 3.5a; tmux's own CHANGES puts it in the 3.3a-to-3.4 section.
    ///
    /// `-N` and `-P` are available on every supported release.
    pub const CAPTURE_TRIM_BLANK_CELLS: ReleaseVersion =
        ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL);

    /// `list-clients` leaving out a client that is stopped rather than gone,
    /// and so `Error::ClientSuspended` being reachable at all.
    ///
    /// From this release `sort_get_clients` screens the listing on
    /// `CLIENT_UNATTACHEDFLAGS`, which covers the dead, the exiting and the
    /// suspended together, so a suspended or locked client disappears from it.
    /// Earlier releases screen on the session alone, and suspending a client
    /// never clears that, so it stays listed and reads back normally.
    ///
    /// Measured on 3.2a, 3.4, 3.5a, 3.6b, 3.7 and 3.7c: the first four list a
    /// suspended client, the last two do not.
    pub const CLIENTS_HIDE_STOPPED: ReleaseVersion =
        ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// The mirrored layouts, and so `Layout::MainHorizontalMirrored`
    /// and `Layout::MainVerticalMirrored`.
    ///
    /// Below this release `layout_set_lookup` does not carry those names, and
    /// tmux refuses one as it would a typo.
    pub const MIRRORED_LAYOUTS: ReleaseVersion = ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL);

    /// `window_layout` and `select-layout` using a JSON subset instead of the
    /// classic checksum-prefixed string, and so `LayoutSpec::Saved`
    /// accepting one.
    ///
    /// tmux's own `CHANGES FROM 3.7c TO 3.8` names the release: "Layout
    /// strings now use a JSON subset format ... The old format is still
    /// accepted; control mode clients receive old layouts unless they set the
    /// new-layouts flag." Below this release a JSON string is refused as an
    /// unrecognised layout, with no hint that the value is simply from a
    /// newer tmux.
    ///
    /// 3.8 dumps JSON and 3.7c does not.
    pub const JSON_LAYOUTS: ReleaseVersion = ReleaseVersion::new(3, 8, ReleaseSuffix::FINAL);
}

#[cfg(test)]
mod grammar_tests {
    use super::VersionError;
    use super::{MAX_REPORTED_TOKEN, ReleaseVersion, TmuxVersion};

    fn parsed(raw: &str) -> Result<TmuxVersion, VersionError> {
        TmuxVersion::parse_output(format!("tmux {raw}\n").as_bytes())
    }

    /// The reading the version check gives a daemon's report.
    fn report(raw: &str) -> Result<TmuxVersion, VersionError> {
        let output = format!("tmux {raw}\n");
        TmuxVersion::parse_report(output.as_bytes(), output.len())
    }

    fn rank(raw: &str) -> ReleaseVersion {
        parsed(raw)
            .unwrap_or_else(|error| panic!("{raw}: {error:?}"))
            .behavior_release()
            .unwrap_or_else(|| panic!("{raw} names no release"))
    }

    /// Every form tmux has reported through `#{version}`, and the library-side
    /// spellings it never reported.
    #[test]
    fn the_canonical_grammar_accepts_refuses_and_orders() {
        for raw in [
            "3.3a", "3.4", "3.7d", "3.8", "3.10", "4.0", "3.4-rc", "3.8-rc", "3.8-rc2", "next-3.4",
            "next-3.9",
        ] {
            let version = parsed(raw).unwrap_or_else(|error| panic!("{raw}: {error:?}"));
            assert!(version.ensure_supported().is_ok(), "{raw} is supported");
        }
        for raw in ["3.3", "3.2a", "3.3-rc", "3.2-rc5", "next-3.3"] {
            let version = parsed(raw).unwrap_or_else(|error| panic!("{raw}: {error:?}"));
            assert!(
                matches!(
                    version.ensure_supported(),
                    Err(VersionError::UnsupportedTmuxVersion { .. })
                ),
                "{raw} is below the floor"
            );
        }
        // A report that names no release is unrecognized, never "too old".
        for raw in [
            "master",
            "openbsd-7.6",
            "3",
            "garbage",
            "3.3a-master",
            "3.3a-dev",
            "next-3.3a",
            "next-3.9-master",
        ] {
            let outcome = report(raw).and_then(|version| version.ensure_supported());
            assert!(
                matches!(outcome, Err(VersionError::UnrecognizedTmuxVersion { .. })),
                "{raw} is unrecognized: {outcome:?}"
            );
        }
        // Output that is not a version report is invalid, with the length the
        // caller says it read.
        let long = "x".repeat(MAX_REPORTED_TOKEN + 1);
        let at_limit = "x".repeat(MAX_REPORTED_TOKEN);
        for output in [
            b"tmux \n".to_vec(),
            b"tmux 3.3a garbage\n".to_vec(),
            b"tmux 3.3a".to_vec(),
            b"3.3a\n".to_vec(),
            format!("tmux {long}\n").into_bytes(),
            "tmux 3.3\u{e9}\n".as_bytes().to_vec(),
            b"tmux 3.3\x01\n".to_vec(),
        ] {
            let outcome = TmuxVersion::parse_report(&output, 7);
            assert!(
                matches!(
                    outcome,
                    Err(VersionError::InvalidVersionOutput { output_len: 7 })
                ),
                "{}: {outcome:?}",
                output.escape_ascii()
            );
        }
        let outcome = TmuxVersion::parse_report(format!("tmux {at_limit}\n").as_bytes(), 0);
        assert!(
            matches!(outcome, Err(VersionError::UnrecognizedTmuxVersion { .. })),
            "a token at the limit is kept: {outcome:?}"
        );

        for chain in [
            &["next-3.7", "3.7-rc", "3.7-rc3", "3.7", "3.7a"][..],
            &["next-3.4", "3.4-rc", "3.4"],
            &["3.9", "3.10"],
        ] {
            for pair in chain.windows(2) {
                assert!(
                    rank(pair[0]) < rank(pair[1]),
                    "{} ranks below {}",
                    pair[0],
                    pair[1]
                );
            }
        }

        // Anything ranking at or above 3.7 final may acquire control.
        let control = ReleaseVersion::new(3, 7, crate::ReleaseSuffix::FINAL);
        for (raw, allowed) in [
            ("3.8", true),
            ("next-3.8", true),
            ("3.7", true),
            ("3.7d", true),
            ("next-3.7", false),
            ("3.7-rc2", false),
            ("3.6b", false),
        ] {
            let version = parsed(raw).unwrap_or_else(|error| panic!("{raw}: {error:?}"));
            assert_eq!(
                version.has_behavior(&control),
                allowed,
                "{raw} control acquisition"
            );
        }
    }
}

#[cfg(test)]
mod eligibility_tests;

#[cfg(test)]
mod catalog_tests {
    /// A gate at or below the supported floor is always true, so it only hides
    /// that the code it guards is unconditional.
    #[test]
    fn no_since_gate_sits_at_or_below_the_supported_floor() {
        let source = include_str!("version.rs");
        let module = source
            .split("pub mod since {")
            .nth(1)
            .expect("the since module exists")
            .split("\n}\n")
            .next()
            .expect("the since module ends");
        let mut gates = 0;
        for piece in module.split("ReleaseVersion::new(").skip(1) {
            let mut numbers = piece
                .split(',')
                .take(2)
                .map(|number| number.trim().parse::<u8>().expect("a numeric release"));
            let (major, minor) = (numbers.next().unwrap(), numbers.next().unwrap());
            assert!(
                (major, minor) > (3, 3),
                "a gate for {major}.{minor} is always true on a 3.3a floor"
            );
            gates += 1;
        }
        assert!(gates > 0, "the scan found no gates");
    }
}
