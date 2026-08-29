//! Tmux release parsing and capability checks.

use std::cmp::Ordering;
use std::fmt;
use std::str;

use crate::Error;

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

    const PATCH_A: Self = Self(SuffixKind::Patch(b'a'));

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

/// A parsed raw tmux version.
///
/// Numbered releases expose an ordered [`ReleaseVersion`]. Development
/// identifiers preserve their token and remain unordered relative to numbered
/// releases.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let version = guard.server().capabilities().await?.tmux_version().clone();
///
/// // A development build carries no numbered release, so a caller asks
/// // `meets` rather than comparing the text tmux printed.
/// assert!(version.release().is_some() || version.is_development());
/// assert!(!version.raw().is_empty());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct TmuxVersion {
    raw: Box<str>,
    release: Option<ReleaseVersion>,
}

impl TmuxVersion {
    /// The minimum supported tmux release, 3.2a.
    pub const MIN_SUPPORTED: ReleaseVersion = ReleaseVersion::new(3, 2, ReleaseSuffix::PATCH_A);

    /// Parse the exact output from `tmux -V`.
    ///
    /// The output must contain one nonempty program-name token, one version
    /// token, and one trailing newline. Invalid process output is never stored
    /// in the returned error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVersionOutput`] for malformed framing or version
    /// syntax, unsupported characters, extra lines, or invalid UTF-8.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.7b\n")?;
    /// assert_eq!(version.raw(), "3.7b");
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    pub fn parse_output(output: &[u8]) -> Result<Self, Error> {
        let raw = parse_output_frame(output)
            .ok_or_else(|| Error::from_invalid_version_output(output.len()))?;

        let release = match raw.as_bytes().first() {
            Some(first) if first.is_ascii_digit() => Some(
                parse_release(raw)
                    .ok_or_else(|| Error::from_invalid_version_output(output.len()))?,
            ),
            Some(_) if is_development_identifier(raw) => None,
            _ => return Err(Error::from_invalid_version_output(output.len())),
        };

        Ok(Self {
            raw: raw.into(),
            release,
        })
    }

    /// Return the exact version token without the program-name prefix or newline.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux next-3.8\n")?;
    /// assert_eq!(version.raw(), "next-3.8");
    /// # Ok::<(), libtmux::Error>(())
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
    /// use libtmux::TmuxVersion;
    ///
    /// let release = TmuxVersion::parse_output(b"tmux 3.7\n")?;
    /// let development = TmuxVersion::parse_output(b"tmux master\n")?;
    /// assert_eq!(release.release().map(|value| value.major()), Some(3));
    /// assert_eq!(development.release(), None);
    /// # Ok::<(), libtmux::Error>(())
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
    /// use libtmux::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux master\n")?;
    /// assert!(version.is_development());
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    #[must_use]
    pub const fn is_development(&self) -> bool {
        self.release.is_none()
    }

    /// Return the release whose in-tree behavior this version carries.
    ///
    /// A numbered release returns itself. A `next-X.Y` identifier returns
    /// `X.Y`, because its tree already contains the changes being developed
    /// for that release. `master` returns `None` because it names no release.
    ///
    /// Unlike [`TmuxVersion::meets`], this does not clamp development
    /// identifiers to the crate's minimum supported release. It answers "which
    /// tmux source is running", not "may this version be relied on", so it
    /// suits behavior windows that both open and close at known releases.
    pub(crate) fn behavior_release(&self) -> Option<ReleaseVersion> {
        self.release.or_else(|| parse_next_release(&self.raw))
    }

    /// Return whether this version supports a required numbered release.
    ///
    /// Development identifiers meet only requirements at or below the crate's
    /// minimum supported release; they are not promoted to an invented release.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.2a\n")?;
    /// assert!(version.meets(&TmuxVersion::MIN_SUPPORTED));
    /// # Ok::<(), libtmux::Error>(())
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

    /// Require this version to meet the crate's minimum supported release.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedTmuxVersion`] when the detected version does
    /// not establish support for tmux 3.2a.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::TmuxVersion;
    ///
    /// let version = TmuxVersion::parse_output(b"tmux 3.2a\n")?;
    /// version.ensure_supported()?;
    /// # Ok::<(), libtmux::Error>(())
    /// ```
    pub fn ensure_supported(&self) -> Result<(), Error> {
        if self.meets(&Self::MIN_SUPPORTED) {
            return Ok(());
        }

        Err(Error::unsupported_tmux_version(
            self.clone(),
            Self::MIN_SUPPORTED,
        ))
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
        ReleaseSuffix::FINAL,
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
/// Every method that can answer [`crate::Error::UnsupportedCapability`] names
/// its floor here, so a caller can ask before it calls rather than learning
/// from the failure. Compare with [`TmuxVersion::meets`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let version = guard.server().capabilities().await?.tmux_version().clone();
///
/// if version.meets(&libtmux::since::SERVER_ACCESS) {
///     assert_eq!(guard.server().access_rules().await?.len(), 1);
/// }
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
pub mod since {
    use super::{ReleaseSuffix, ReleaseVersion};

    /// `server-access`, and so [`crate::Server::access_rules`].
    pub const SERVER_ACCESS: ReleaseVersion = ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL);

    /// The prompt history, and so [`crate::Server::prompt_history`].
    pub const PROMPT_HISTORY: ReleaseVersion = ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL);

    /// `capture-pane -F`, and so [`crate::Pane::capture_lines`].
    pub const CAPTURE_LINE_FLAGS: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// Taking a pane out of a control client's stream without crashing the
    /// server, and so [`crate::control::ControlSender::mute_pane`] using `off`.
    ///
    /// Before this release `refresh-client -A <pane>:off` leaves the output
    /// blocks already queued for that pane in place, while the pane stops
    /// holding the server's read buffer back. Writing those blocks later
    /// reads past the end of a buffer the server has drained, and the server
    /// segfaults. `mute_pane` pauses the pane below this release instead,
    /// which discards the queue on every supported release.
    pub const CONTROL_PANE_OFF: ReleaseVersion = ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL);

    /// `list-clients` leaving out a client that is stopped rather than gone,
    /// and so [`crate::Error::ClientSuspended`] being reachable at all.
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

    /// The mirrored layouts, and so [`crate::Layout::MainHorizontalMirrored`]
    /// and [`crate::Layout::MainVerticalMirrored`].
    ///
    /// Below this release `layout_set_lookup` does not carry those names, and
    /// tmux refuses one as it would a typo.
    pub const MIRRORED_LAYOUTS: ReleaseVersion = ReleaseVersion::new(3, 5, ReleaseSuffix::FINAL);
}
