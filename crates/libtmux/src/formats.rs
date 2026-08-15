//! Byte-preserving tmux text values.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use crate::version::{ReleaseSuffix, ReleaseVersion, TmuxVersion};

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

/// Decoder applied to a parsed format slot.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecoderKind {
    /// Accept only ASCII bytes.
    Ascii,
    /// Preserve arbitrary non-NUL bytes.
    Text,
    /// Parse a canonical tmux boolean.
    Bool,
    /// Parse a canonical unsigned 8-bit integer.
    U8,
    /// Parse a canonical unsigned 32-bit integer.
    U32,
    /// Parse a canonical unsigned 64-bit integer.
    U64,
    /// Parse a canonical signed 32-bit integer.
    I32,
    /// Parse canonical signed Unix seconds.
    Timestamp,
    /// Parse a native Session ID.
    SessionId,
    /// Parse a native Window ID.
    WindowId,
    /// Parse a native Pane ID.
    PaneId,
    /// Parse a bounded pane progress percentage.
    PaneProgress,
    /// Parse a closed pane progress state.
    PaneProgressState,
}

/// Tmux object that semantically owns a format field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticOwner {
    /// Server-owned field.
    Server,
    /// Caller-owned list-row field.
    ListRow,
    /// Global mode formatting field.
    Mode,
    /// Paste-buffer field.
    Buffer,
    /// Client-owned field.
    Client,
    /// Client attachment field.
    ClientAttachment,
    /// Client-specific Window view field.
    ClientWindowView,
    /// Session-owned field.
    Session,
    /// Window-owned field.
    Window,
    /// Pane-owned field.
    Pane,
    /// Window-link-owned field.
    WindowLink,
    /// Command metadata field.
    Command,
    /// Configuration metadata field.
    Config,
    /// Copy-mode field.
    CopyMode,
}

/// Tmux context required to resolve a format field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequiredContext {
    /// No object context.
    None,
    /// Original list format type.
    FormatType,
    /// Caller-owned list row.
    ListRow,
    /// Paste-buffer context.
    Buffer,
    /// Client context.
    Client,
    /// Session context.
    Session,
    /// Window context.
    Window,
    /// Pane context.
    Pane,
    /// Window-link context.
    WindowLink,
    /// Command context.
    Command,
    /// Configuration context.
    Config,
    /// Copy-mode context.
    CopyMode,
}

/// Intrinsic snapshot placement for a modeled field.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InfoPlacement {
    /// Metadata is available only to explicit projections.
    CatalogOnly,
    /// Field belongs to Session snapshots.
    SessionInfo,
    /// Field belongs to Window snapshots.
    WindowInfo,
    /// Field belongs to Pane snapshots.
    PaneInfo,
    /// Field belongs to Client snapshots.
    ClientInfo,
}

/// Canonical list-profile admission set.
#[allow(
    dead_code,
    reason = "catalog-only metadata is verified by the checked parity fixture"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ListProfiles(u8);

#[allow(
    dead_code,
    reason = "catalog-only metadata is verified by the checked parity fixture"
)]
impl ListProfiles {
    const SESSIONS: u8 = 1;
    const WINDOWS: u8 = 1 << 1;
    const PANES: u8 = 1 << 2;
    const CLIENTS: u8 = 1 << 3;

    /// Admit all four list commands.
    const fn all() -> Self {
        Self(Self::SESSIONS | Self::WINDOWS | Self::PANES | Self::CLIENTS)
    }

    /// Admit only Client listings.
    const fn clients() -> Self {
        Self(Self::CLIENTS)
    }

    /// Admit no list command.
    const fn none() -> Self {
        Self(0)
    }

    /// Return whether this set admits a profile.
    pub(crate) const fn contains(self, profile: ListProfile) -> bool {
        let bit = match profile {
            ListProfile::Sessions => Self::SESSIONS,
            ListProfile::Windows => Self::WINDOWS,
            ListProfile::Panes => Self::PANES,
            ListProfile::Clients => Self::CLIENTS,
        };
        self.0 & bit != 0
    }
}

/// Policy for an empty supported field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmptyPolicy {
    /// Reject an empty field.
    Required,
    /// Interpret an empty field as absent.
    Absent,
    /// Pass an empty field to its decoder.
    Available,
}

/// Trusted static metadata for one tmux format field.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FormatDescriptor {
    /// Trusted tmux format name.
    name: &'static str,
    /// Object that semantically owns the field.
    owner: SemanticOwner,
    /// Context tmux needs to resolve the field.
    required_context: RequiredContext,
    /// List commands that can resolve the field.
    profiles: ListProfiles,
    /// First numbered tmux release that provides the field.
    minimum_release: ReleaseVersion,
    /// Primitive byte decoder for the field.
    decoder: DecoderKind,
    /// Later snapshot policy for an empty supported field.
    empty_policy: EmptyPolicy,
    /// Intrinsic snapshot placement.
    placement: InfoPlacement,
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl FormatDescriptor {
    /// Return the trusted tmux format name.
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Return the semantic owner.
    pub(crate) const fn owner(&self) -> SemanticOwner {
        self.owner
    }

    /// Return the required tmux context.
    pub(crate) const fn required_context(&self) -> RequiredContext {
        self.required_context
    }

    /// Return the admitted list profiles.
    pub(crate) const fn profiles(&self) -> ListProfiles {
        self.profiles
    }

    /// Return the first tmux release that provides this field.
    pub(crate) const fn minimum_release(&self) -> ReleaseVersion {
        self.minimum_release
    }

    /// Return the primitive decoder.
    pub(crate) const fn decoder(&self) -> DecoderKind {
        self.decoder
    }

    /// Return the empty-field policy.
    pub(crate) const fn empty_policy(&self) -> EmptyPolicy {
        self.empty_policy
    }

    /// Return the intrinsic snapshot placement.
    pub(crate) const fn placement(&self) -> InfoPlacement {
        self.placement
    }

    /// Construct trusted baseline-era metadata for codec tests.
    #[cfg(test)]
    pub(crate) const fn for_codec_test(name: &'static str, decoder: DecoderKind) -> Self {
        Self {
            name,
            owner: SemanticOwner::Session,
            required_context: RequiredContext::Session,
            profiles: ListProfiles::all(),
            minimum_release: TmuxVersion::MIN_SUPPORTED,
            decoder,
            empty_policy: EmptyPolicy::Available,
            placement: InfoPlacement::CatalogOnly,
        }
    }
}

/// Invoke a consumer with the complete placement-grouped format catalog.
macro_rules! format_catalog {
    ($consumer:ident) => {
        $consumer! {
            SessionInfo {
                target: "session";
                baseline: (SESSION_ID, session_id, "session_id", Session, Session, All, V3_2A, SessionId, Required);
                supplements: [
                    (SESSION_ACTIVITY, session_activity, "session_activity", Session, Session, All, V3_2A, Timestamp, Required),
                    (SESSION_ATTACHED, session_attached, "session_attached", Session, Session, All, V3_2A, U32, Required),
                    (SESSION_CREATED, session_created, "session_created", Session, Session, All, V3_2A, Timestamp, Required),
                    (SESSION_LAST_ATTACHED, session_last_attached, "session_last_attached", Session, Session, All, V3_2A, Timestamp, Absent),
                    (SESSION_MANY_ATTACHED, session_many_attached, "session_many_attached", Session, Session, All, V3_2A, Bool, Required),
                    (SESSION_NAME, session_name, "session_name", Session, Session, All, V3_2A, Text, Required),
                    (SESSION_PATH, session_path, "session_path", Session, Session, All, V3_2A, Text, Required),
                    (SESSION_WINDOWS, session_windows, "session_windows", Session, Session, All, V3_2A, U32, Required),
                ];
            }
            WindowInfo {
                target: "window";
                baseline: (WINDOW_ID, window_id, "window_id", Window, Window, All, V3_2A, WindowId, Required);
                supplements: [
                    (WINDOW_ACTIVITY, window_activity, "window_activity", Window, Window, All, V3_2A, Timestamp, Required),
                    (WINDOW_CELL_HEIGHT, window_cell_height, "window_cell_height", Window, Window, All, V3_2A, U32, Required),
                    (WINDOW_CELL_WIDTH, window_cell_width, "window_cell_width", Window, Window, All, V3_2A, U32, Required),
                    (WINDOW_HEIGHT, window_height, "window_height", Window, Window, All, V3_2A, U32, Required),
                    (WINDOW_LAYOUT, window_layout, "window_layout", Window, Window, All, V3_2A, Text, Required),
                    (WINDOW_NAME, window_name, "window_name", Window, Window, All, V3_2A, Text, Available),
                    (WINDOW_PANES, window_panes, "window_panes", Window, Window, All, V3_2A, U32, Required),
                    (WINDOW_VISIBLE_LAYOUT, window_visible_layout, "window_visible_layout", Window, Window, All, V3_2A, Text, Required),
                    (WINDOW_WIDTH, window_width, "window_width", Window, Window, All, V3_2A, U32, Required),
                    (WINDOW_ZOOMED_FLAG, window_zoomed_flag, "window_zoomed_flag", Window, Window, All, V3_2A, Bool, Required),
                ];
            }
            PaneInfo {
                target: "pane";
                baseline: (PANE_ID, pane_id, "pane_id", Pane, Pane, All, V3_2A, PaneId, Required);
                supplements: [
                    (ALTERNATE_SAVED_X, alternate_saved_x, "alternate_saved_x", Pane, Pane, All, V3_2A, U32, Required),
                    (ALTERNATE_SAVED_Y, alternate_saved_y, "alternate_saved_y", Pane, Pane, All, V3_2A, U32, Required),
                    (BRACKET_PASTE_FLAG, bracket_paste_flag, "bracket_paste_flag", Pane, Pane, All, V3_7, Bool, Required),
                    (CURSOR_CHARACTER, cursor_character, "cursor_character", Pane, Pane, All, V3_2A, Text, Absent),
                    (CURSOR_FLAG, cursor_flag, "cursor_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (CURSOR_X, cursor_x, "cursor_x", Pane, Pane, All, V3_2A, U32, Required),
                    (CURSOR_Y, cursor_y, "cursor_y", Pane, Pane, All, V3_2A, U32, Required),
                    (HISTORY_BYTES, history_bytes, "history_bytes", Pane, Pane, All, V3_2A, U64, Required),
                    (HISTORY_LIMIT, history_limit, "history_limit", Pane, Pane, All, V3_2A, U32, Required),
                    (HISTORY_SIZE, history_size, "history_size", Pane, Pane, All, V3_2A, U32, Required),
                    (INSERT_FLAG, insert_flag, "insert_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (KEYPAD_CURSOR_FLAG, keypad_cursor_flag, "keypad_cursor_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (KEYPAD_FLAG, keypad_flag, "keypad_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (MOUSE_ALL_FLAG, mouse_all_flag, "mouse_all_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (MOUSE_ANY_FLAG, mouse_any_flag, "mouse_any_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (MOUSE_BUTTON_FLAG, mouse_button_flag, "mouse_button_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (MOUSE_SGR_FLAG, mouse_sgr_flag, "mouse_sgr_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (MOUSE_STANDARD_FLAG, mouse_standard_flag, "mouse_standard_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (ORIGIN_FLAG, origin_flag, "origin_flag", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_ACTIVE, pane_active, "pane_active", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_AT_BOTTOM, pane_at_bottom, "pane_at_bottom", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_AT_LEFT, pane_at_left, "pane_at_left", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_AT_RIGHT, pane_at_right, "pane_at_right", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_AT_TOP, pane_at_top, "pane_at_top", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_BG, pane_bg, "pane_bg", Pane, Pane, All, V3_2A, Text, Required),
                    (PANE_BOTTOM, pane_bottom, "pane_bottom", Pane, Pane, All, V3_2A, I32, Required),
                    (PANE_CURRENT_COMMAND, pane_current_command, "pane_current_command", Pane, Pane, All, V3_2A, Text, Absent),
                    (PANE_CURRENT_PATH, pane_current_path, "pane_current_path", Pane, Pane, All, V3_2A, Text, Absent),
                    (PANE_DEAD, pane_dead, "pane_dead", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_DEAD_SIGNAL, pane_dead_signal, "pane_dead_signal", Pane, Pane, All, V3_3, Text, Absent),
                    (PANE_DEAD_STATUS, pane_dead_status, "pane_dead_status", Pane, Pane, All, V3_2A, U8, Absent),
                    (PANE_DEAD_TIME, pane_dead_time, "pane_dead_time", Pane, Pane, All, V3_3, Timestamp, Absent),
                    (PANE_FG, pane_fg, "pane_fg", Pane, Pane, All, V3_2A, Text, Required),
                    (PANE_FLAGS, pane_flags, "pane_flags", Pane, Pane, All, V3_7, Text, Available),
                    (PANE_FLOATING_FLAG, pane_floating_flag, "pane_floating_flag", Pane, Pane, All, V3_7, Bool, Required),
                    (PANE_HEIGHT, pane_height, "pane_height", Pane, Pane, All, V3_2A, U32, Required),
                    (PANE_IN_MODE, pane_in_mode, "pane_in_mode", Pane, Pane, All, V3_2A, U32, Required),
                    (PANE_INDEX, pane_index, "pane_index", Pane, Pane, All, V3_2A, U32, Required),
                    (PANE_INPUT_OFF, pane_input_off, "pane_input_off", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_LAST, pane_last, "pane_last", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_LEFT, pane_left, "pane_left", Pane, Pane, All, V3_2A, I32, Required),
                    (PANE_MARKED, pane_marked, "pane_marked", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_MODE, pane_mode, "pane_mode", Pane, Pane, All, V3_2A, Text, Absent),
                    (PANE_PATH, pane_path, "pane_path", Pane, Pane, All, V3_2A, Text, Available),
                    (PANE_PB_PROGRESS, pane_pb_progress, "pane_pb_progress", Pane, Pane, All, V3_7, PaneProgress, Required),
                    (PANE_PB_STATE, pane_pb_state, "pane_pb_state", Pane, Pane, All, V3_7, PaneProgressState, Required),
                    (PANE_PID, pane_pid, "pane_pid", Pane, Pane, All, V3_2A, U32, Required),
                    (PANE_PIPE, pane_pipe, "pane_pipe", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_PIPE_PID, pane_pipe_pid, "pane_pipe_pid", Pane, Pane, All, V3_7, U32, Absent),
                    (PANE_RIGHT, pane_right, "pane_right", Pane, Pane, All, V3_2A, I32, Required),
                    (PANE_SEARCH_STRING, pane_search_string, "pane_search_string", Pane, Pane, All, V3_2A, Text, Available),
                    (PANE_START_COMMAND, pane_start_command, "pane_start_command", Pane, Pane, All, V3_2A, Text, Available),
                    (PANE_START_PATH, pane_start_path, "pane_start_path", Pane, Pane, All, V3_3, Text, Available),
                    (PANE_SYNCHRONIZED, pane_synchronized, "pane_synchronized", Pane, Pane, All, V3_2A, Bool, Required),
                    (PANE_TABS, pane_tabs, "pane_tabs", Pane, Pane, All, V3_2A, Text, Available),
                    (PANE_TITLE, pane_title, "pane_title", Pane, Pane, All, V3_2A, Text, Available),
                    (PANE_TOP, pane_top, "pane_top", Pane, Pane, All, V3_2A, I32, Required),
                    (PANE_TTY, pane_tty, "pane_tty", Pane, Pane, All, V3_2A, Text, Required),
                    (PANE_WIDTH, pane_width, "pane_width", Pane, Pane, All, V3_2A, U32, Required),
                    (PANE_X, pane_x, "pane_x", Pane, Pane, All, V3_7, I32, Required),
                    (PANE_Y, pane_y, "pane_y", Pane, Pane, All, V3_7, I32, Required),
                    (PANE_Z, pane_z, "pane_z", Pane, Pane, All, V3_7, U32, Required),
                    (PANE_ZOOMED_FLAG, pane_zoomed_flag, "pane_zoomed_flag", Pane, Pane, All, V3_7, Bool, Required),
                    (SCROLL_REGION_LOWER, scroll_region_lower, "scroll_region_lower", Pane, Pane, All, V3_2A, U32, Required),
                    (SCROLL_REGION_UPPER, scroll_region_upper, "scroll_region_upper", Pane, Pane, All, V3_2A, U32, Required),
                    (SYNCHRONIZED_OUTPUT_FLAG, synchronized_output_flag, "synchronized_output_flag", Pane, Pane, All, V3_7, Bool, Required),
                    (WRAP_FLAG, wrap_flag, "wrap_flag", Pane, Pane, All, V3_2A, Bool, Required),
                ];
            }
            ClientInfo {
                target: "client";
                baseline: (CLIENT_NAME, client_name, "client_name", Client, Client, Clients, V3_2A, Text, Required);
                supplements: [
                    (CLIENT_ACTIVITY, client_activity, "client_activity", Client, Client, Clients, V3_2A, Timestamp, Required),
                    (CLIENT_CELL_HEIGHT, client_cell_height, "client_cell_height", Client, Client, Clients, V3_2A, U32, Absent),
                    (CLIENT_CELL_WIDTH, client_cell_width, "client_cell_width", Client, Client, Clients, V3_2A, U32, Absent),
                    (CLIENT_CONTROL_MODE, client_control_mode, "client_control_mode", Client, Client, Clients, V3_2A, Bool, Required),
                    (CLIENT_CREATED, client_created, "client_created", Client, Client, Clients, V3_2A, Timestamp, Required),
                    (CLIENT_DISCARDED, client_discarded, "client_discarded", Client, Client, Clients, V3_2A, U64, Required),
                    (CLIENT_FLAGS, client_flags, "client_flags", Client, Client, Clients, V3_2A, Text, Available),
                    (CLIENT_HEIGHT, client_height, "client_height", Client, Client, Clients, V3_2A, U32, Absent),
                    (CLIENT_KEY_TABLE, client_key_table, "client_key_table", Client, Client, Clients, V3_2A, Text, Required),
                    (CLIENT_PID, client_pid, "client_pid", Client, Client, Clients, V3_2A, U32, Required),
                    (CLIENT_PREFIX, client_prefix, "client_prefix", Client, Client, Clients, V3_2A, Bool, Required),
                    (CLIENT_READONLY, client_readonly, "client_readonly", Client, Client, Clients, V3_2A, Bool, Required),
                    (CLIENT_TERMFEATURES, client_termfeatures, "client_termfeatures", Client, Client, Clients, V3_2A, Text, Available),
                    (CLIENT_TERMNAME, client_termname, "client_termname", Client, Client, Clients, V3_2A, Text, Available),
                    (CLIENT_TERMTYPE, client_termtype, "client_termtype", Client, Client, Clients, V3_2A, Text, Available),
                    (CLIENT_TTY, client_tty, "client_tty", Client, Client, Clients, V3_2A, Text, Available),
                    (CLIENT_UID, client_uid, "client_uid", Client, Client, Clients, V3_3, U32, Absent),
                    (CLIENT_USER, client_user, "client_user", Client, Client, Clients, V3_3, Text, Absent),
                    (CLIENT_UTF8, client_utf8, "client_utf8", Client, Client, Clients, V3_2A, Bool, Required),
                    (CLIENT_WIDTH, client_width, "client_width", Client, Client, Clients, V3_2A, U32, Required),
                    (CLIENT_WRITTEN, client_written, "client_written", Client, Client, Clients, V3_2A, U64, Required),
                ];
            }
            CatalogOnly {
                fields: [
                    (ACTIVE_WINDOW_INDEX, active_window_index, "active_window_index", Session, Session, All, V3_2A, U32, Required),
                    (BUFFER_NAME, buffer_name, "buffer_name", Buffer, Buffer, None, V3_2A, Text, Required),
                    (BUFFER_SAMPLE, buffer_sample, "buffer_sample", Buffer, Buffer, None, V3_2A, Text, Available),
                    (BUFFER_SIZE, buffer_size, "buffer_size", Buffer, Buffer, None, V3_2A, U64, Required),
                    (CLIENT_LAST_SESSION, client_last_session, "client_last_session", ClientAttachment, Client, Clients, V3_2A, Text, Absent),
                    (CLIENT_MODE_FORMAT, client_mode_format, "client_mode_format", Mode, None, All, V3_2A, Text, Required),
                    (CLIENT_SESSION, client_session, "client_session", ClientAttachment, Client, Clients, V3_2A, Text, Absent),
                    (COMMAND_LIST_ALIAS, command_list_alias, "command_list_alias", Command, Command, None, V3_2A, Text, Available),
                    (COMMAND_LIST_NAME, command_list_name, "command_list_name", Command, Command, None, V3_2A, Text, Required),
                    (COMMAND_LIST_USAGE, command_list_usage, "command_list_usage", Command, Command, None, V3_2A, Text, Available),
                    (CONFIG_FILES, config_files, "config_files", Server, None, All, V3_2A, Text, Available),
                    (COPY_CURSOR_LINE, copy_cursor_line, "copy_cursor_line", CopyMode, CopyMode, None, V3_2A, Text, Available),
                    (COPY_CURSOR_WORD, copy_cursor_word, "copy_cursor_word", CopyMode, CopyMode, None, V3_2A, Text, Available),
                    (COPY_CURSOR_X, copy_cursor_x, "copy_cursor_x", CopyMode, CopyMode, None, V3_2A, I32, Required),
                    (COPY_CURSOR_Y, copy_cursor_y, "copy_cursor_y", CopyMode, CopyMode, None, V3_2A, I32, Required),
                    (CURRENT_FILE, current_file, "current_file", Config, Config, None, V3_2A, Text, Required),
                    (LAST_WINDOW_INDEX, last_window_index, "last_window_index", Session, Session, All, V3_2A, U32, Required),
                    (LINE, line, "line", ListRow, ListRow, All, V3_2A, U32, Required),
                    (NEXT_SESSION_ID, next_session_id, "next_session_id", Server, None, All, V3_3, SessionId, Required),
                    (PANE_FORMAT, pane_format, "pane_format", ListRow, FormatType, All, V3_2A, Bool, Required),
                    (PANE_MARKED_SET, pane_marked_set, "pane_marked_set", Server, Pane, All, V3_2A, Bool, Required),
                    (PID, pid, "pid", Server, None, All, V3_2A, U32, Required),
                    (SCROLL_POSITION, scroll_position, "scroll_position", CopyMode, CopyMode, None, V3_2A, I32, Required),
                    (SEARCH_MATCH, search_match, "search_match", CopyMode, CopyMode, None, V3_2A, Text, Absent),
                    (SELECTION_END_X, selection_end_x, "selection_end_x", CopyMode, CopyMode, None, V3_2A, I32, Absent),
                    (SELECTION_END_Y, selection_end_y, "selection_end_y", CopyMode, CopyMode, None, V3_2A, I32, Absent),
                    (SELECTION_START_X, selection_start_x, "selection_start_x", CopyMode, CopyMode, None, V3_2A, I32, Absent),
                    (SELECTION_START_Y, selection_start_y, "selection_start_y", CopyMode, CopyMode, None, V3_2A, I32, Absent),
                    (SESSION_ALERTS, session_alerts, "session_alerts", Session, Session, All, V3_2A, Text, Available),
                    (SESSION_ATTACHED_LIST, session_attached_list, "session_attached_list", Session, Session, All, V3_2A, Text, Available),
                    (SESSION_FORMAT, session_format, "session_format", ListRow, FormatType, All, V3_2A, Bool, Required),
                    (SESSION_GROUP, session_group, "session_group", Session, Session, All, V3_2A, Text, Absent),
                    (SESSION_GROUP_ATTACHED, session_group_attached, "session_group_attached", Session, Session, All, V3_2A, U32, Absent),
                    (SESSION_GROUP_ATTACHED_LIST, session_group_attached_list, "session_group_attached_list", Session, Session, All, V3_2A, Text, Absent),
                    (SESSION_GROUP_LIST, session_group_list, "session_group_list", Session, Session, All, V3_2A, Text, Absent),
                    (SESSION_GROUP_MANY_ATTACHED, session_group_many_attached, "session_group_many_attached", Session, Session, All, V3_2A, Bool, Absent),
                    (SESSION_GROUP_SIZE, session_group_size, "session_group_size", Session, Session, All, V3_2A, U32, Absent),
                    (SESSION_GROUPED, session_grouped, "session_grouped", Session, Session, All, V3_2A, Bool, Required),
                    (SESSION_MARKED, session_marked, "session_marked", Session, Session, All, V3_2A, Bool, Required),
                    (SESSION_STACK, session_stack, "session_stack", Session, Session, All, V3_2A, Text, Required),
                    (SOCKET_PATH, socket_path, "socket_path", Server, None, All, V3_2A, Text, Required),
                    (START_TIME, start_time, "start_time", Server, None, All, V3_2A, Timestamp, Required),
                    (UID, uid, "uid", Server, None, All, V3_3, U32, Required),
                    (USER, user, "user", Server, None, All, V3_3, Text, Absent),
                    (VERSION, version, "version", Server, None, All, V3_2A, Text, Required),
                    (WINDOW_ACTIVE, window_active, "window_active", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_ACTIVE_CLIENTS, window_active_clients, "window_active_clients", Window, WindowLink, All, V3_2A, U32, Required),
                    (WINDOW_ACTIVE_CLIENTS_LIST, window_active_clients_list, "window_active_clients_list", Window, WindowLink, All, V3_2A, Text, Available),
                    (WINDOW_ACTIVE_SESSIONS, window_active_sessions, "window_active_sessions", Window, WindowLink, All, V3_2A, U32, Required),
                    (WINDOW_ACTIVE_SESSIONS_LIST, window_active_sessions_list, "window_active_sessions_list", Window, WindowLink, All, V3_2A, Text, Available),
                    (WINDOW_ACTIVITY_FLAG, window_activity_flag, "window_activity_flag", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_BELL_FLAG, window_bell_flag, "window_bell_flag", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_BIGGER, window_bigger, "window_bigger", ClientWindowView, Client, Clients, V3_2A, Bool, Required),
                    (WINDOW_END_FLAG, window_end_flag, "window_end_flag", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_FLAGS, window_flags, "window_flags", WindowLink, WindowLink, All, V3_2A, Text, Available),
                    (WINDOW_FORMAT, window_format, "window_format", ListRow, FormatType, All, V3_2A, Bool, Required),
                    (WINDOW_INDEX, window_index, "window_index", WindowLink, WindowLink, All, V3_2A, I32, Required),
                    (WINDOW_LAST_FLAG, window_last_flag, "window_last_flag", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_LINKED, window_linked, "window_linked", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_LINKED_SESSIONS, window_linked_sessions, "window_linked_sessions", Window, WindowLink, All, V3_2A, U32, Required),
                    (WINDOW_LINKED_SESSIONS_LIST, window_linked_sessions_list, "window_linked_sessions_list", Window, WindowLink, All, V3_2A, Text, Required),
                    (WINDOW_MARKED_FLAG, window_marked_flag, "window_marked_flag", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_OFFSET_X, window_offset_x, "window_offset_x", ClientWindowView, Client, Clients, V3_2A, U32, Absent),
                    (WINDOW_OFFSET_Y, window_offset_y, "window_offset_y", ClientWindowView, Client, Clients, V3_2A, U32, Absent),
                    (WINDOW_RAW_FLAGS, window_raw_flags, "window_raw_flags", WindowLink, WindowLink, All, V3_2A, Text, Available),
                    (WINDOW_SILENCE_FLAG, window_silence_flag, "window_silence_flag", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                    (WINDOW_STACK_INDEX, window_stack_index, "window_stack_index", WindowLink, WindowLink, All, V3_2A, U32, Required),
                    (WINDOW_START_FLAG, window_start_flag, "window_start_flag", WindowLink, WindowLink, All, V3_2A, Bool, Required),
                ];
            }
        }
    };
}
pub(crate) use format_catalog;

macro_rules! catalog_profiles {
    (All) => {
        ListProfiles::all()
    };
    (Clients) => {
        ListProfiles::clients()
    };
    (None) => {
        ListProfiles::none()
    };
}

macro_rules! catalog_floor {
    (V3_2A) => {
        TmuxVersion::MIN_SUPPORTED
    };
    (V3_3) => {
        ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL)
    };
    (V3_7) => {
        ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL)
    };
}

macro_rules! define_catalog_descriptor {
    ($static:ident, $name:literal, $owner:ident, $context:ident, $profiles:ident, $floor:ident, $decoder:ident, $empty:ident, $placement:ident) => {
        #[allow(
            dead_code,
            reason = "modelled and tested; only a projection of it is hydrated today"
        )]
        pub(crate) static $static: FormatDescriptor = FormatDescriptor {
            name: $name,
            owner: SemanticOwner::$owner,
            required_context: RequiredContext::$context,
            profiles: catalog_profiles!($profiles),
            minimum_release: catalog_floor!($floor),
            decoder: DecoderKind::$decoder,
            empty_policy: EmptyPolicy::$empty,
            placement: InfoPlacement::$placement,
        };
    };
}

macro_rules! define_format_catalog {
    (
        SessionInfo {
            target: $session_target:literal;
            baseline: ($session_static:ident, $session_field:ident, $session_name:literal, $session_owner:ident, $session_context:ident, $session_profiles:ident, $session_floor:ident, $session_decoder:ident, $session_empty:ident);
            supplements: [$(($session_s_static:ident, $session_s_field:ident, $session_s_name:literal, $session_s_owner:ident, $session_s_context:ident, $session_s_profiles:ident, $session_s_floor:ident, $session_s_decoder:ident, $session_s_empty:ident)),* $(,)?];
        }
        WindowInfo {
            target: $window_target:literal;
            baseline: ($window_static:ident, $window_field:ident, $window_name:literal, $window_owner:ident, $window_context:ident, $window_profiles:ident, $window_floor:ident, $window_decoder:ident, $window_empty:ident);
            supplements: [$(($window_s_static:ident, $window_s_field:ident, $window_s_name:literal, $window_s_owner:ident, $window_s_context:ident, $window_s_profiles:ident, $window_s_floor:ident, $window_s_decoder:ident, $window_s_empty:ident)),* $(,)?];
        }
        PaneInfo {
            target: $pane_target:literal;
            baseline: ($pane_static:ident, $pane_field:ident, $pane_name:literal, $pane_owner:ident, $pane_context:ident, $pane_profiles:ident, $pane_floor:ident, $pane_decoder:ident, $pane_empty:ident);
            supplements: [$(($pane_s_static:ident, $pane_s_field:ident, $pane_s_name:literal, $pane_s_owner:ident, $pane_s_context:ident, $pane_s_profiles:ident, $pane_s_floor:ident, $pane_s_decoder:ident, $pane_s_empty:ident)),* $(,)?];
        }
        ClientInfo {
            target: $client_target:literal;
            baseline: ($client_static:ident, $client_field:ident, $client_name:literal, $client_owner:ident, $client_context:ident, $client_profiles:ident, $client_floor:ident, $client_decoder:ident, $client_empty:ident);
            supplements: [$(($client_s_static:ident, $client_s_field:ident, $client_s_name:literal, $client_s_owner:ident, $client_s_context:ident, $client_s_profiles:ident, $client_s_floor:ident, $client_s_decoder:ident, $client_s_empty:ident)),* $(,)?];
        }
        CatalogOnly {
            fields: [$(($catalog_static:ident, $catalog_field:ident, $catalog_name:literal, $catalog_owner:ident, $catalog_context:ident, $catalog_profiles:ident, $catalog_floor:ident, $catalog_decoder:ident, $catalog_empty:ident)),* $(,)?];
        }
    ) => {
        define_catalog_descriptor!($session_static, $session_name, $session_owner, $session_context, $session_profiles, $session_floor, $session_decoder, $session_empty, SessionInfo);
        $(define_catalog_descriptor!($session_s_static, $session_s_name, $session_s_owner, $session_s_context, $session_s_profiles, $session_s_floor, $session_s_decoder, $session_s_empty, SessionInfo);)*
        define_catalog_descriptor!($window_static, $window_name, $window_owner, $window_context, $window_profiles, $window_floor, $window_decoder, $window_empty, WindowInfo);
        $(define_catalog_descriptor!($window_s_static, $window_s_name, $window_s_owner, $window_s_context, $window_s_profiles, $window_s_floor, $window_s_decoder, $window_s_empty, WindowInfo);)*
        define_catalog_descriptor!($pane_static, $pane_name, $pane_owner, $pane_context, $pane_profiles, $pane_floor, $pane_decoder, $pane_empty, PaneInfo);
        $(define_catalog_descriptor!($pane_s_static, $pane_s_name, $pane_s_owner, $pane_s_context, $pane_s_profiles, $pane_s_floor, $pane_s_decoder, $pane_s_empty, PaneInfo);)*
        define_catalog_descriptor!($client_static, $client_name, $client_owner, $client_context, $client_profiles, $client_floor, $client_decoder, $client_empty, ClientInfo);
        $(define_catalog_descriptor!($client_s_static, $client_s_name, $client_s_owner, $client_s_context, $client_s_profiles, $client_s_floor, $client_s_decoder, $client_s_empty, ClientInfo);)*
        $(define_catalog_descriptor!($catalog_static, $catalog_name, $catalog_owner, $catalog_context, $catalog_profiles, $catalog_floor, $catalog_decoder, $catalog_empty, CatalogOnly);)*

        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static SESSION_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$session_static, $(&$session_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static SESSION_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$session_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static WINDOW_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$window_static, $(&$window_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static WINDOW_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$window_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static PANE_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$pane_static, $(&$pane_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static PANE_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$pane_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static CLIENT_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$client_static, $(&$client_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static CLIENT_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$client_s_static),*];

        #[allow(dead_code, reason = "catalog-only metadata is verified by the checked parity fixture")]
        static GROUPED_CATALOG: &[&FormatDescriptor] = &[
            &$session_static, $(&$session_s_static),*,
            &$window_static, $(&$window_s_static),*,
            &$pane_static, $(&$pane_s_static),*,
            &$client_static, $(&$client_s_static),*,
            $(&$catalog_static),*
        ];
    };
}
format_catalog!(define_format_catalog);

/// Lexicographic identifier-only catalog index.
#[allow(
    dead_code,
    reason = "catalog-only metadata is verified by the checked parity fixture"
)]
static CATALOG: [&FormatDescriptor; 178] = [
    &ACTIVE_WINDOW_INDEX,
    &ALTERNATE_SAVED_X,
    &ALTERNATE_SAVED_Y,
    &BRACKET_PASTE_FLAG,
    &BUFFER_NAME,
    &BUFFER_SAMPLE,
    &BUFFER_SIZE,
    &CLIENT_ACTIVITY,
    &CLIENT_CELL_HEIGHT,
    &CLIENT_CELL_WIDTH,
    &CLIENT_CONTROL_MODE,
    &CLIENT_CREATED,
    &CLIENT_DISCARDED,
    &CLIENT_FLAGS,
    &CLIENT_HEIGHT,
    &CLIENT_KEY_TABLE,
    &CLIENT_LAST_SESSION,
    &CLIENT_MODE_FORMAT,
    &CLIENT_NAME,
    &CLIENT_PID,
    &CLIENT_PREFIX,
    &CLIENT_READONLY,
    &CLIENT_SESSION,
    &CLIENT_TERMFEATURES,
    &CLIENT_TERMNAME,
    &CLIENT_TERMTYPE,
    &CLIENT_TTY,
    &CLIENT_UID,
    &CLIENT_USER,
    &CLIENT_UTF8,
    &CLIENT_WIDTH,
    &CLIENT_WRITTEN,
    &COMMAND_LIST_ALIAS,
    &COMMAND_LIST_NAME,
    &COMMAND_LIST_USAGE,
    &CONFIG_FILES,
    &COPY_CURSOR_LINE,
    &COPY_CURSOR_WORD,
    &COPY_CURSOR_X,
    &COPY_CURSOR_Y,
    &CURRENT_FILE,
    &CURSOR_CHARACTER,
    &CURSOR_FLAG,
    &CURSOR_X,
    &CURSOR_Y,
    &HISTORY_BYTES,
    &HISTORY_LIMIT,
    &HISTORY_SIZE,
    &INSERT_FLAG,
    &KEYPAD_CURSOR_FLAG,
    &KEYPAD_FLAG,
    &LAST_WINDOW_INDEX,
    &LINE,
    &MOUSE_ALL_FLAG,
    &MOUSE_ANY_FLAG,
    &MOUSE_BUTTON_FLAG,
    &MOUSE_SGR_FLAG,
    &MOUSE_STANDARD_FLAG,
    &NEXT_SESSION_ID,
    &ORIGIN_FLAG,
    &PANE_ACTIVE,
    &PANE_AT_BOTTOM,
    &PANE_AT_LEFT,
    &PANE_AT_RIGHT,
    &PANE_AT_TOP,
    &PANE_BG,
    &PANE_BOTTOM,
    &PANE_CURRENT_COMMAND,
    &PANE_CURRENT_PATH,
    &PANE_DEAD,
    &PANE_DEAD_SIGNAL,
    &PANE_DEAD_STATUS,
    &PANE_DEAD_TIME,
    &PANE_FG,
    &PANE_FLAGS,
    &PANE_FLOATING_FLAG,
    &PANE_FORMAT,
    &PANE_HEIGHT,
    &PANE_ID,
    &PANE_IN_MODE,
    &PANE_INDEX,
    &PANE_INPUT_OFF,
    &PANE_LAST,
    &PANE_LEFT,
    &PANE_MARKED,
    &PANE_MARKED_SET,
    &PANE_MODE,
    &PANE_PATH,
    &PANE_PB_PROGRESS,
    &PANE_PB_STATE,
    &PANE_PID,
    &PANE_PIPE,
    &PANE_PIPE_PID,
    &PANE_RIGHT,
    &PANE_SEARCH_STRING,
    &PANE_START_COMMAND,
    &PANE_START_PATH,
    &PANE_SYNCHRONIZED,
    &PANE_TABS,
    &PANE_TITLE,
    &PANE_TOP,
    &PANE_TTY,
    &PANE_WIDTH,
    &PANE_X,
    &PANE_Y,
    &PANE_Z,
    &PANE_ZOOMED_FLAG,
    &PID,
    &SCROLL_POSITION,
    &SCROLL_REGION_LOWER,
    &SCROLL_REGION_UPPER,
    &SEARCH_MATCH,
    &SELECTION_END_X,
    &SELECTION_END_Y,
    &SELECTION_START_X,
    &SELECTION_START_Y,
    &SESSION_ACTIVITY,
    &SESSION_ALERTS,
    &SESSION_ATTACHED,
    &SESSION_ATTACHED_LIST,
    &SESSION_CREATED,
    &SESSION_FORMAT,
    &SESSION_GROUP,
    &SESSION_GROUP_ATTACHED,
    &SESSION_GROUP_ATTACHED_LIST,
    &SESSION_GROUP_LIST,
    &SESSION_GROUP_MANY_ATTACHED,
    &SESSION_GROUP_SIZE,
    &SESSION_GROUPED,
    &SESSION_ID,
    &SESSION_LAST_ATTACHED,
    &SESSION_MANY_ATTACHED,
    &SESSION_MARKED,
    &SESSION_NAME,
    &SESSION_PATH,
    &SESSION_STACK,
    &SESSION_WINDOWS,
    &SOCKET_PATH,
    &START_TIME,
    &SYNCHRONIZED_OUTPUT_FLAG,
    &UID,
    &USER,
    &VERSION,
    &WINDOW_ACTIVE,
    &WINDOW_ACTIVE_CLIENTS,
    &WINDOW_ACTIVE_CLIENTS_LIST,
    &WINDOW_ACTIVE_SESSIONS,
    &WINDOW_ACTIVE_SESSIONS_LIST,
    &WINDOW_ACTIVITY,
    &WINDOW_ACTIVITY_FLAG,
    &WINDOW_BELL_FLAG,
    &WINDOW_BIGGER,
    &WINDOW_CELL_HEIGHT,
    &WINDOW_CELL_WIDTH,
    &WINDOW_END_FLAG,
    &WINDOW_FLAGS,
    &WINDOW_FORMAT,
    &WINDOW_HEIGHT,
    &WINDOW_ID,
    &WINDOW_INDEX,
    &WINDOW_LAST_FLAG,
    &WINDOW_LAYOUT,
    &WINDOW_LINKED,
    &WINDOW_LINKED_SESSIONS,
    &WINDOW_LINKED_SESSIONS_LIST,
    &WINDOW_MARKED_FLAG,
    &WINDOW_NAME,
    &WINDOW_OFFSET_X,
    &WINDOW_OFFSET_Y,
    &WINDOW_PANES,
    &WINDOW_RAW_FLAGS,
    &WINDOW_SILENCE_FLAG,
    &WINDOW_STACK_INDEX,
    &WINDOW_START_FLAG,
    &WINDOW_VISIBLE_LAYOUT,
    &WINDOW_WIDTH,
    &WINDOW_ZOOMED_FLAG,
    &WRAP_FLAG,
];

/// Tmux list command profile represented by a format plan.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListProfile {
    /// Session rows.
    Sessions,
    /// Window rows.
    Windows,
    /// Pane rows.
    Panes,
    /// Client rows.
    Clients,
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl ListProfile {
    /// Return the mandatory identity descriptor for this profile.
    const fn baseline(self) -> &'static FormatDescriptor {
        match self {
            Self::Sessions => &SESSION_ID,
            Self::Windows => &WINDOW_ID,
            Self::Panes => &PANE_ID,
            Self::Clients => &CLIENT_NAME,
        }
    }

    /// Return version-gated descriptors after the mandatory identity.
    const fn supplements(self) -> &'static [&'static FormatDescriptor] {
        match self {
            Self::Sessions => SESSION_INFO_SUPPLEMENTS,
            Self::Windows => WINDOW_INFO_SUPPLEMENTS,
            Self::Panes => PANE_INFO_SUPPLEMENTS,
            Self::Clients => CLIENT_INFO_SUPPLEMENTS,
        }
    }
}

/// Version evidence retained by a format plan.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
enum PlanVersion {
    /// Version detected from tmux.
    Detected(TmuxVersion),
    /// Fixed evidence used by descriptor-only codec fixtures.
    #[cfg(test)]
    MinimumSupportedFixture,
}

impl PlanVersion {
    /// Select the transport dialect implied by this plan's version evidence.
    fn dialect(&self) -> TransportDialect {
        match self {
            Self::Detected(version) => TransportDialect::for_version(version),
            #[cfg(test)]
            Self::MinimumSupportedFixture => TransportDialect::RawQ,
        }
    }
}

/// Bytes that `#{q:}` prefixes with a backslash.
///
/// This is tmux's `format_quote_shell` set. None of these bytes is an octal
/// digit or one of the letters `vis` emits, so the two escaping layers below
/// compose into one unambiguous grammar.
const QUOTE_SHELL_SPECIALS: &[u8] = b"|&;<>()$`\\\"'*?[# =%";

/// Escaping tmux applies to expanded format output before it reaches stdout.
///
/// The daemon that owns the socket decides this, not the client executable the
/// version probe ran. A mismatch is possible, so each dialect rejects escapes
/// the other produces instead of decoding them into different bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransportDialect {
    /// `#{q:}` escaping only, printed verbatim.
    ///
    /// Applies to releases before 3.4 and from 3.6 onward.
    RawQ,
    /// `#{q:}` escaping wrapped in `VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`.
    ///
    /// tmux 3.4 and 3.5 ran command output through `utf8_stravisx`, so control
    /// bytes and invalid UTF-8 arrive as `\r`-style or `\ooo` escapes.
    Vis,
}

impl TransportDialect {
    /// First release that visually encoded command output.
    ///
    /// Introduced before tag 3.4 by upstream commits `7e497c7f` and
    /// `93b1b781`.
    const VIS_FIRST: ReleaseVersion = ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL);

    /// First release that restored verbatim command output.
    ///
    /// Restored before tag 3.6 by upstream commit `5fd45b38`, "Do not strvis
    /// output to terminal from commands."
    const VIS_RESTORED: ReleaseVersion = ReleaseVersion::new(3, 6, ReleaseSuffix::FINAL);

    /// Select the dialect a detected tmux version emits.
    ///
    /// `master` names no release and resolves to [`TransportDialect::RawQ`],
    /// matching every tmux tree since the 3.6 restore. A build that is wrong
    /// about this fails loudly during decoding rather than returning altered
    /// bytes, because neither dialect accepts the other's escapes.
    pub(crate) fn for_version(version: &TmuxVersion) -> Self {
        match version.behavior_release() {
            Some(release) if release >= Self::VIS_FIRST && release < Self::VIS_RESTORED => {
                Self::Vis
            }
            _ => Self::RawQ,
        }
    }
}

/// Consumer intent carried by a format plan.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanPurpose {
    /// Complete intrinsic snapshot for one placement.
    Intrinsic(InfoPlacement),
    /// Explicit trusted-static projection.
    Projection,
}

/// Version-selection state for one planned field.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanFieldState {
    /// Field is rendered at this selected-slot coordinate.
    Selected { slot: usize },
    /// Numbered tmux release predates the field.
    Unsupported,
    /// Development build provides no numbered availability proof.
    Unproven,
}

/// Descriptor and availability evidence retained in request order.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlannedField {
    /// Trusted static descriptor.
    pub(crate) descriptor: &'static FormatDescriptor,
    /// Selected slot or unavailable evidence.
    pub(crate) state: PlanFieldState,
}

/// Ordered trusted metadata and exact tmux format template.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
pub(crate) struct FormatPlan {
    /// List operation represented by the plan.
    profile: ListProfile,
    /// Version evidence used to select descriptors.
    version: PlanVersion,
    /// Mandatory identity, stored independently from optional catalog entries.
    baseline: &'static FormatDescriptor,
    /// Intrinsic or explicit projection intent.
    purpose: PlanPurpose,
    /// Complete requested fields, including unavailable evidence.
    planned: Box<[PlannedField]>,
    /// Descriptor order shared by template rendering and row parsing.
    descriptors: Box<[&'static FormatDescriptor]>,
    /// Exact template rendered from `descriptors`.
    template: Box<str>,
    /// Transport escaping the planned version emits.
    dialect: TransportDialect,
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl FormatPlan {
    /// Build a plan from one supported detected tmux version.
    pub(crate) fn for_profile(profile: ListProfile, version: &TmuxVersion) -> Self {
        select_for_profile(profile, version, profile.supplements())
    }

    /// Build an explicit projection from trusted static descriptors.
    pub(crate) fn for_descriptors(
        profile: ListProfile,
        version: &TmuxVersion,
        requested: &[&'static FormatDescriptor],
    ) -> Result<Self, FormatCodecError> {
        let baseline = profile.baseline();
        let mut descriptors = vec![baseline];
        let mut planned = vec![PlannedField {
            descriptor: baseline,
            state: PlanFieldState::Selected { slot: 0 },
        }];
        let mut seen = std::collections::HashSet::with_capacity(requested.len());

        for descriptor in requested.iter().copied() {
            if !descriptor.profiles().contains(profile) {
                return Err(FormatCodecError::plan(
                    FormatCodecErrorKind::ScopeInapplicable,
                    descriptor,
                    profile,
                ));
            }
            if std::ptr::eq(descriptor, baseline) {
                continue;
            }
            if !seen.insert(std::ptr::from_ref(descriptor)) {
                return Err(FormatCodecError::plan(
                    FormatCodecErrorKind::DuplicateDescriptor,
                    descriptor,
                    profile,
                ));
            }

            let state = classify_field(version, descriptor, descriptors.len());
            if matches!(state, PlanFieldState::Selected { .. }) {
                descriptors.push(descriptor);
            }
            planned.push(PlannedField { descriptor, state });
        }

        Ok(Self::build(
            profile,
            PlanVersion::Detected(version.clone()),
            baseline,
            PlanPurpose::Projection,
            planned,
            descriptors,
        ))
    }

    /// Construct a descriptor-only plan for codec tests.
    #[cfg(test)]
    pub(crate) fn for_codec_test(
        descriptors: Vec<&'static FormatDescriptor>,
    ) -> Result<Self, FormatCodecError> {
        Self::for_codec_test_with(descriptors, PlanVersion::MinimumSupportedFixture)
    }

    /// Construct a descriptor-only plan whose dialect follows a real version.
    #[cfg(test)]
    pub(crate) fn for_codec_test_at(
        descriptors: Vec<&'static FormatDescriptor>,
        version: &TmuxVersion,
    ) -> Result<Self, FormatCodecError> {
        Self::for_codec_test_with(descriptors, PlanVersion::Detected(version.clone()))
    }

    /// Share codec-fixture plan construction across version evidence.
    #[cfg(test)]
    fn for_codec_test_with(
        descriptors: Vec<&'static FormatDescriptor>,
        version: PlanVersion,
    ) -> Result<Self, FormatCodecError> {
        let Some(baseline) = descriptors.first().copied() else {
            return Err(FormatCodecError::empty_plan());
        };

        Ok(Self::build(
            ListProfile::Sessions,
            version,
            baseline,
            PlanPurpose::Projection,
            descriptors
                .iter()
                .copied()
                .enumerate()
                .map(|(slot, descriptor)| PlannedField {
                    descriptor,
                    state: PlanFieldState::Selected { slot },
                })
                .collect(),
            descriptors,
        ))
    }

    /// Return the selected descriptor sequence to codec fixtures.
    #[cfg(test)]
    pub(crate) fn descriptors_for_test(&self) -> &[&'static FormatDescriptor] {
        &self.descriptors
    }

    /// Return this plan's list profile.
    pub(crate) const fn profile(&self) -> ListProfile {
        self.profile
    }

    /// Return this plan's purpose.
    pub(crate) const fn purpose(&self) -> PlanPurpose {
        self.purpose
    }

    /// Return complete planned availability evidence.
    pub(crate) fn planned(&self) -> &[PlannedField] {
        &self.planned
    }

    /// Return the exact format template passed to tmux.
    pub(crate) fn template(&self) -> &str {
        &self.template
    }

    /// Parse raw tmux stdout into immutable byte-preserving rows.
    ///
    /// NUL rejection stays at this framing boundary because [`TmuxText`]
    /// intentionally remains a general byte container outside tmux parsing.
    pub(crate) fn parse_rows(&self, stdout: &[u8]) -> Result<Vec<ParsedRow>, FormatCodecError> {
        let mut cursor = 0;
        let mut row = 0;
        let mut parsed = Vec::new();

        while cursor < stdout.len() {
            parsed.push(self.parse_row(stdout, &mut cursor, row)?);
            row += 1;
        }

        Ok(parsed)
    }

    /// Parse one complete row while advancing the shared raw cursor.
    fn parse_row(
        &self,
        stdout: &[u8],
        cursor: &mut usize,
        row: usize,
    ) -> Result<ParsedRow, FormatCodecError> {
        let mut bytes = Vec::new();
        let mut slots = Vec::with_capacity(self.descriptors.len());
        let mut final_descriptor = self.baseline;
        let mut final_field = 0;

        for (field, descriptor) in self.descriptors.iter().copied().enumerate() {
            final_descriptor = descriptor;
            final_field = field;
            slots.push(Self::parse_field(
                stdout,
                cursor,
                row,
                field,
                descriptor,
                self.dialect,
                &mut bytes,
            )?);
        }

        Self::consume_row_terminator(stdout, cursor, row, final_field, final_descriptor)?;
        Ok(ParsedRow {
            row,
            bytes: bytes.into_boxed_slice(),
            slots: slots.into_boxed_slice(),
        })
    }

    /// Parse one field into the row's shared unescaped buffer.
    fn parse_field(
        stdout: &[u8],
        cursor: &mut usize,
        row: usize,
        field: usize,
        descriptor: &'static FormatDescriptor,
        dialect: TransportDialect,
        bytes: &mut Vec<u8>,
    ) -> Result<SlotMeta, FormatCodecError> {
        let raw_start = *cursor;
        let range_start = bytes.len();

        loop {
            let Some(byte) = stdout.get(*cursor).copied() else {
                return Err(FormatCodecError::framing(
                    FormatCodecErrorKind::MissingFieldTerminator,
                    FormatCodecPhase::Field,
                    row,
                    field,
                    descriptor,
                    stdout.len(),
                ));
            };

            if byte == 0 {
                return Err(FormatCodecError::framing(
                    FormatCodecErrorKind::EmbeddedNul,
                    FormatCodecPhase::Field,
                    row,
                    field,
                    descriptor,
                    *cursor,
                ));
            }

            if byte == b'\\' {
                *cursor += 1;
                Self::decode_escape(stdout, cursor, row, field, descriptor, dialect, bytes)?;
            } else if byte == b'%' {
                *cursor += 1;
                return Ok(SlotMeta {
                    descriptor,
                    range: range_start..bytes.len(),
                    raw_start,
                });
            } else {
                bytes.push(byte);
                *cursor += 1;
            }
        }
    }

    /// Decode one escape sequence with `cursor` already past its backslash.
    ///
    /// Both dialects accept the `#{q:}` set. Only [`TransportDialect::Vis`]
    /// additionally accepts the `vis` escapes, so output from the other
    /// dialect fails here instead of decoding into different bytes.
    fn decode_escape(
        stdout: &[u8],
        cursor: &mut usize,
        row: usize,
        field: usize,
        descriptor: &'static FormatDescriptor,
        dialect: TransportDialect,
        bytes: &mut Vec<u8>,
    ) -> Result<(), FormatCodecError> {
        let framing = |kind, offset| {
            FormatCodecError::framing(
                kind,
                FormatCodecPhase::Escape,
                row,
                field,
                descriptor,
                offset,
            )
        };

        let Some(escaped) = stdout.get(*cursor).copied() else {
            return Err(framing(FormatCodecErrorKind::DanglingEscape, stdout.len()));
        };
        if escaped == 0 {
            return Err(framing(FormatCodecErrorKind::EmbeddedNul, *cursor));
        }

        if QUOTE_SHELL_SPECIALS.contains(&escaped) {
            bytes.push(escaped);
            *cursor += 1;
            return Ok(());
        }

        if dialect == TransportDialect::RawQ {
            return Err(framing(FormatCodecErrorKind::InvalidEscape, *cursor));
        }

        if let Some(control) = vis_cstyle_byte(escaped) {
            bytes.push(control);
            *cursor += 1;
            return Ok(());
        }

        // `vis` renders every remaining byte as three octal digits, so the
        // leading digit never exceeds the 0o377 upper bound of one byte.
        if !matches!(escaped, b'0'..=b'3') {
            return Err(framing(FormatCodecErrorKind::InvalidEscape, *cursor));
        }
        let Some(digits) = stdout.get(*cursor..*cursor + 3) else {
            return Err(framing(FormatCodecErrorKind::DanglingEscape, stdout.len()));
        };
        let Some(value) = decode_octal_escape(digits) else {
            return Err(framing(FormatCodecErrorKind::InvalidEscape, *cursor));
        };
        if value == 0 {
            return Err(framing(FormatCodecErrorKind::EmbeddedNul, *cursor));
        }

        bytes.push(value);
        *cursor += 3;
        Ok(())
    }

    /// Require the exact LF following the final planned field.
    fn consume_row_terminator(
        stdout: &[u8],
        cursor: &mut usize,
        row: usize,
        field: usize,
        descriptor: &'static FormatDescriptor,
    ) -> Result<(), FormatCodecError> {
        let Some(terminator) = stdout.get(*cursor).copied() else {
            return Err(FormatCodecError::framing(
                FormatCodecErrorKind::MissingRowLf,
                FormatCodecPhase::RowTerminator,
                row,
                field,
                descriptor,
                stdout.len(),
            ));
        };

        if terminator == 0 {
            return Err(FormatCodecError::framing(
                FormatCodecErrorKind::EmbeddedNul,
                FormatCodecPhase::RowTerminator,
                row,
                field,
                descriptor,
                *cursor,
            ));
        }
        if terminator != b'\n' {
            return Err(FormatCodecError::framing(
                FormatCodecErrorKind::UnexpectedRowTerminator,
                FormatCodecPhase::RowTerminator,
                row,
                field,
                descriptor,
                *cursor,
            ));
        }

        *cursor += 1;
        Ok(())
    }

    /// Store one sound ordered selection and render from that same sequence.
    fn build(
        profile: ListProfile,
        version: PlanVersion,
        baseline: &'static FormatDescriptor,
        purpose: PlanPurpose,
        planned: Vec<PlannedField>,
        descriptors: Vec<&'static FormatDescriptor>,
    ) -> Self {
        let descriptors = descriptors.into_boxed_slice();
        let mut template = String::new();
        for descriptor in &descriptors {
            template.push_str("#{q:");
            template.push_str(descriptor.name());
            template.push_str("}%");
        }

        let dialect = version.dialect();
        Self {
            profile,
            version,
            baseline,
            purpose,
            planned: planned.into_boxed_slice(),
            descriptors,
            template: template.into_boxed_str(),
            dialect,
        }
    }
}

/// Map a `VIS_CSTYLE` letter to the byte it encodes.
///
/// Only the letters reachable under tmux's flags are accepted. `VIS_NL`,
/// `VIS_TAB`, and `VIS_SP` are unset, so newline, tab, and space stay literal
/// and their `\n`, `\t`, and `\s` forms never appear. `\0` is absent because a
/// tmux format value cannot carry NUL.
const fn vis_cstyle_byte(letter: u8) -> Option<u8> {
    match letter {
        b'a' => Some(0x07),
        b'b' => Some(0x08),
        b'v' => Some(0x0b),
        b'f' => Some(0x0c),
        b'r' => Some(0x0d),
        _ => None,
    }
}

/// Decode exactly three octal digits into one byte.
fn decode_octal_escape(digits: &[u8]) -> Option<u8> {
    let mut value: u8 = 0;
    for digit in digits {
        let place = digit.checked_sub(b'0').filter(|place| *place < 8)?;
        value = value.checked_mul(8)?.checked_add(place)?;
    }
    Some(value)
}

/// Select a profile's mandatory identity and supported supplements.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
fn select_for_profile(
    profile: ListProfile,
    version: &TmuxVersion,
    supplements: &'static [&'static FormatDescriptor],
) -> FormatPlan {
    let baseline = profile.baseline();
    let mut descriptors = Vec::with_capacity(supplements.len() + 1);
    descriptors.push(baseline);
    let mut planned = Vec::with_capacity(supplements.len() + 1);
    planned.push(PlannedField {
        descriptor: baseline,
        state: PlanFieldState::Selected { slot: 0 },
    });

    for descriptor in supplements.iter().copied() {
        if std::ptr::eq(descriptor, baseline) {
            continue;
        }

        let state = classify_field(version, descriptor, descriptors.len());
        if matches!(state, PlanFieldState::Selected { .. }) {
            descriptors.push(descriptor);
        }
        planned.push(PlannedField { descriptor, state });
    }

    FormatPlan::build(
        profile,
        PlanVersion::Detected(version.clone()),
        baseline,
        PlanPurpose::Intrinsic(baseline.placement()),
        planned,
        descriptors,
    )
}

/// Classify one nonbaseline field from numbered or development evidence.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
fn classify_field(
    version: &TmuxVersion,
    descriptor: &'static FormatDescriptor,
    selected_slot: usize,
) -> PlanFieldState {
    match version.release() {
        Some(release) if *release >= descriptor.minimum_release() => PlanFieldState::Selected {
            slot: selected_slot,
        },
        Some(_) => PlanFieldState::Unsupported,
        None if descriptor.minimum_release() <= TmuxVersion::MIN_SUPPORTED => {
            PlanFieldState::Selected {
                slot: selected_slot,
            }
        }
        None => PlanFieldState::Unproven,
    }
}

/// Exercise the production profile selector with synthetic static metadata.
#[cfg(test)]
fn for_profile_selection_test(
    profile: ListProfile,
    version: &TmuxVersion,
    supplements: &'static [&'static FormatDescriptor],
) -> FormatPlan {
    select_for_profile(profile, version, supplements)
}

/// Scalar category of a private format codec failure.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FormatCodecErrorKind {
    /// A test-only descriptor plan was empty.
    EmptyPlan,
    /// A field reached EOF without an unescaped percent.
    MissingFieldTerminator,
    /// A backslash was the final raw byte, or an octal escape was truncated.
    DanglingEscape,
    /// An escape sequence is not produced by the plan's transport dialect.
    InvalidEscape,
    /// A parsed row did not end with LF.
    MissingRowLf,
    /// A byte other than LF followed the final field.
    UnexpectedRowTerminator,
    /// Raw tmux output contained NUL.
    EmbeddedNul,
    /// An ASCII descriptor contained a non-ASCII byte.
    NonAscii,
    /// Descriptor cannot be resolved by the requested list profile.
    ScopeInapplicable,
    /// Explicit projection repeated a nonbaseline descriptor.
    DuplicateDescriptor,
    /// A required selected field was empty.
    RequiredFieldEmpty,
    /// Typed decoder rejected a selected value.
    InvalidValue,
    /// A row and plan did not share the same ordered selection.
    PlanRowMismatch,
}

/// Parser or decoder phase in which a codec failure occurred.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FormatCodecPhase {
    /// Plan construction.
    Plan,
    /// Ordinary field scanning.
    Field,
    /// Byte following a field escape.
    Escape,
    /// Byte following the final field terminator.
    RowTerminator,
    /// Primitive slot decoding.
    Decode,
}

/// Payload-free diagnostic metadata for a private format codec failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FormatCodecError {
    /// Stable error category.
    kind: FormatCodecErrorKind,
    /// State in which the error was detected.
    phase: FormatCodecPhase,
    /// Zero-based row coordinate when available.
    row: Option<usize>,
    /// Zero-based field coordinate when available.
    field: Option<usize>,
    /// Trusted static descriptor name when available.
    field_name: Option<&'static str>,
    /// Expected primitive decoder for decode errors only.
    expected: Option<DecoderKind>,
    /// Absolute raw stdout offset when available.
    offset: Option<usize>,
    /// List profile for plan-level failures when available.
    profile: Option<ListProfile>,
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl FormatCodecError {
    /// Construct an empty-plan error without row metadata.
    const fn empty_plan() -> Self {
        Self {
            kind: FormatCodecErrorKind::EmptyPlan,
            phase: FormatCodecPhase::Plan,
            row: None,
            field: None,
            field_name: None,
            expected: None,
            offset: None,
            profile: None,
        }
    }

    /// Construct a trusted-static plan error.
    const fn plan(
        kind: FormatCodecErrorKind,
        descriptor: &'static FormatDescriptor,
        profile: ListProfile,
    ) -> Self {
        Self {
            kind,
            phase: FormatCodecPhase::Plan,
            row: None,
            field: None,
            field_name: Some(descriptor.name),
            expected: None,
            offset: None,
            profile: Some(profile),
        }
    }

    /// Construct a framing error from trusted coordinates only.
    const fn framing(
        kind: FormatCodecErrorKind,
        phase: FormatCodecPhase,
        row: usize,
        field: usize,
        descriptor: &'static FormatDescriptor,
        offset: usize,
    ) -> Self {
        Self {
            kind,
            phase,
            row: Some(row),
            field: Some(field),
            field_name: Some(descriptor.name),
            expected: None,
            offset: Some(offset),
            profile: None,
        }
    }

    /// Construct an ASCII decoder failure from slot coordinates only.
    const fn non_ascii(slot: &ParsedSlot<'_>) -> Self {
        Self {
            kind: FormatCodecErrorKind::NonAscii,
            phase: FormatCodecPhase::Decode,
            row: Some(slot.row),
            field: Some(slot.field),
            field_name: Some(slot.descriptor.name),
            expected: Some(DecoderKind::Ascii),
            offset: Some(slot.raw_start),
            profile: None,
        }
    }

    /// Construct a typed decoder or required-empty failure.
    pub(crate) const fn typed(kind: FormatCodecErrorKind, slot: &ParsedSlot<'_>) -> Self {
        Self {
            kind,
            phase: FormatCodecPhase::Decode,
            row: Some(slot.row),
            field: Some(slot.field),
            field_name: Some(slot.descriptor.name),
            expected: Some(slot.descriptor.decoder),
            offset: Some(slot.raw_start),
            profile: None,
        }
    }

    /// Construct a structural row mismatch from scalar coordinates.
    pub(crate) const fn row_mismatch(
        row: usize,
        field: Option<usize>,
        field_name: Option<&'static str>,
        offset: Option<usize>,
    ) -> Self {
        Self {
            kind: FormatCodecErrorKind::PlanRowMismatch,
            phase: FormatCodecPhase::Decode,
            row: Some(row),
            field,
            field_name,
            expected: None,
            offset,
            profile: None,
        }
    }

    /// Construct a purpose or placement mismatch.
    pub(crate) const fn purpose_mismatch(profile: ListProfile, row: usize) -> Self {
        Self {
            kind: FormatCodecErrorKind::PlanRowMismatch,
            phase: FormatCodecPhase::Decode,
            row: Some(row),
            field: None,
            field_name: None,
            expected: None,
            offset: None,
            profile: Some(profile),
        }
    }

    /// Return the stable error category.
    pub(crate) const fn kind(&self) -> FormatCodecErrorKind {
        self.kind
    }

    /// Return the parser or decoder phase.
    pub(crate) const fn phase(&self) -> FormatCodecPhase {
        self.phase
    }

    /// Return the zero-based row coordinate.
    pub(crate) const fn row(&self) -> Option<usize> {
        self.row
    }

    /// Return the zero-based field coordinate.
    pub(crate) const fn field(&self) -> Option<usize> {
        self.field
    }

    /// Return the trusted static field name.
    pub(crate) const fn field_name(&self) -> Option<&'static str> {
        self.field_name
    }

    /// Return the expected decoder when decoding failed.
    pub(crate) const fn expected(&self) -> Option<DecoderKind> {
        self.expected
    }

    /// Return the absolute raw stdout offset.
    pub(crate) const fn offset(&self) -> Option<usize> {
        self.offset
    }

    /// Return the implicated list profile.
    pub(crate) const fn profile(&self) -> Option<ListProfile> {
        self.profile
    }
}

impl fmt::Display for FormatCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            FormatCodecErrorKind::EmptyPlan => "empty plan",
            FormatCodecErrorKind::MissingFieldTerminator => "missing field terminator",
            FormatCodecErrorKind::DanglingEscape => "dangling escape",
            FormatCodecErrorKind::InvalidEscape => "invalid escape",
            FormatCodecErrorKind::MissingRowLf => "missing row LF",
            FormatCodecErrorKind::UnexpectedRowTerminator => "unexpected row terminator",
            FormatCodecErrorKind::EmbeddedNul => "embedded NUL",
            FormatCodecErrorKind::NonAscii => "non-ASCII field",
            FormatCodecErrorKind::ScopeInapplicable => "scope inapplicable",
            FormatCodecErrorKind::DuplicateDescriptor => "duplicate descriptor",
            FormatCodecErrorKind::RequiredFieldEmpty => "required field empty",
            FormatCodecErrorKind::InvalidValue => "invalid value",
            FormatCodecErrorKind::PlanRowMismatch => "plan row mismatch",
        };
        let phase = match self.phase {
            FormatCodecPhase::Plan => "plan",
            FormatCodecPhase::Field => "field",
            FormatCodecPhase::Escape => "escape",
            FormatCodecPhase::RowTerminator => "row terminator",
            FormatCodecPhase::Decode => "decode",
        };
        write!(formatter, "format codec {kind} in {phase} phase")
    }
}

impl std::error::Error for FormatCodecError {}

/// One parsed row backed by a single owned unescaped byte buffer.
pub(crate) struct ParsedRow {
    /// Zero-based row coordinate in raw stdout.
    row: usize,
    /// Shared unescaped payload storage for every slot in this row.
    bytes: Box<[u8]>,
    /// Descriptor, range, and raw-offset coordinates in plan order.
    slots: Box<[SlotMeta]>,
}

/// Coordinates for one slot within a parsed row.
struct SlotMeta {
    /// Trusted descriptor selected by the plan.
    descriptor: &'static FormatDescriptor,
    /// Unescaped byte range within the row's shared buffer.
    range: Range<usize>,
    /// Absolute raw stdout offset before q-unescaping shortened the field.
    raw_start: usize,
}

/// Borrowed view of one parsed format slot.
#[derive(Clone, Copy)]
pub(crate) struct ParsedSlot<'row> {
    /// Trusted descriptor selected by the plan.
    descriptor: &'static FormatDescriptor,
    /// Exact unescaped bytes borrowed from shared row storage.
    bytes: &'row [u8],
    /// Zero-based row coordinate.
    row: usize,
    /// Zero-based field coordinate.
    field: usize,
    /// Absolute raw stdout offset, distinct from the unescaped row range.
    raw_start: usize,
}

impl ParsedRow {
    /// Return the zero-based source row coordinate.
    pub(crate) const fn row(&self) -> usize {
        self.row
    }

    /// Borrow one selected slot by its monotonic coordinate.
    pub(crate) fn slot(&self, field: usize) -> Option<ParsedSlot<'_>> {
        self.slots.get(field).map(|slot| ParsedSlot {
            descriptor: slot.descriptor,
            bytes: &self.bytes[slot.range.clone()],
            row: self.row,
            field,
            raw_start: slot.raw_start,
        })
    }

    /// Return the number of selected slots.
    #[allow(
        dead_code,
        reason = "modelled and tested; only a projection of it is hydrated today"
    )]
    pub(crate) const fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Borrow every slot in plan order.
    #[allow(
        dead_code,
        reason = "modelled and tested; only a projection of it is hydrated today"
    )]
    pub(crate) fn slots(&self) -> impl ExactSizeIterator<Item = ParsedSlot<'_>> + '_ {
        self.slots
            .iter()
            .enumerate()
            .map(|(field, slot)| ParsedSlot {
                descriptor: slot.descriptor,
                bytes: &self.bytes[slot.range.clone()],
                row: self.row,
                field,
                raw_start: slot.raw_start,
            })
    }
}

impl<'row> ParsedSlot<'row> {
    /// Return the trusted descriptor for this slot.
    pub(crate) const fn descriptor(&self) -> &'static FormatDescriptor {
        self.descriptor
    }

    /// Return the exact unescaped bytes.
    pub(crate) const fn as_bytes(&self) -> &'row [u8] {
        self.bytes
    }

    /// Return the zero-based selected-slot coordinate.
    pub(crate) const fn field(&self) -> usize {
        self.field
    }

    /// Return the absolute raw slot-start offset.
    pub(crate) const fn raw_start(&self) -> usize {
        self.raw_start
    }
}

/// Decode an ASCII slot without assigning higher-level field semantics.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
pub(crate) fn decode_ascii(slot: ParsedSlot<'_>) -> Result<&str, FormatCodecError> {
    if !slot.as_bytes().is_ascii() {
        return Err(FormatCodecError::non_ascii(&slot));
    }

    match std::str::from_utf8(slot.as_bytes()) {
        Ok(text) => Ok(text),
        Err(_) => Err(FormatCodecError::non_ascii(&slot)),
    }
}

/// Copy a text slot into an exact byte-preserving public value.
pub(crate) fn decode_text(slot: ParsedSlot<'_>) -> TmuxText {
    TmuxText::from_bytes(slot.as_bytes())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::err_expect, clippy::expect_used, clippy::ok_expect)]

    #[cfg(feature = "test-support")]
    use super::decode_text;
    use super::{
        CATALOG, CLIENT_INFO_DESCRIPTORS, CLIENT_INFO_SUPPLEMENTS, CLIENT_NAME, DecoderKind,
        EmptyPolicy, FormatCodecError, FormatCodecErrorKind, FormatCodecPhase, FormatDescriptor,
        FormatPlan, GROUPED_CATALOG, InfoPlacement, ListProfile, ListProfiles, PANE_ID,
        PANE_INFO_DESCRIPTORS, PANE_INFO_SUPPLEMENTS, ParsedRow, ParsedSlot, PlanFieldState,
        PlanPurpose, PlanVersion, QUOTE_SHELL_SPECIALS, RequiredContext, SESSION_ID,
        SESSION_INFO_DESCRIPTORS, SESSION_INFO_SUPPLEMENTS, SemanticOwner, TransportDialect,
        WINDOW_ID, WINDOW_INFO_DESCRIPTORS, WINDOW_INFO_SUPPLEMENTS, for_profile_selection_test,
    };
    #[cfg(feature = "test-support")]
    use crate::Command;
    #[cfg(feature = "test-support")]
    use crate::test::TestServer;
    use crate::{ReleaseSuffix, ReleaseVersion, TmuxVersion};
    use static_assertions::assert_impl_all;

    static FIRST: FormatDescriptor = FormatDescriptor::for_codec_test("first", DecoderKind::Text);
    static SECOND: FormatDescriptor =
        FormatDescriptor::for_codec_test("second", DecoderKind::Ascii);
    static POST_BASELINE: FormatDescriptor = FormatDescriptor {
        name: "post_baseline",
        owner: SemanticOwner::Session,
        required_context: RequiredContext::Session,
        profiles: ListProfiles::all(),
        minimum_release: ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL),
        decoder: DecoderKind::Text,
        empty_policy: EmptyPolicy::Absent,
        placement: InfoPlacement::CatalogOnly,
    };
    static SESSION_SUPPLEMENTS: [&FormatDescriptor; 3] = [&SESSION_ID, &POST_BASELINE, &SESSION_ID];
    #[cfg(feature = "test-support")]
    static RAW_FORMAT_BYTES: FormatDescriptor =
        FormatDescriptor::for_codec_test("@libtmux_format_bytes", DecoderKind::Text);

    const Q_SHELL_ESCAPED: [u8; 19] = [
        0x7c, 0x26, 0x3b, 0x3c, 0x3e, 0x28, 0x29, 0x24, 0x60, 0x5c, 0x22, 0x27, 0x2a, 0x3f, 0x5b,
        0x23, 0x20, 0x3d, 0x25,
    ];
    const SHORT_SENTINEL: &str = "zot-private";
    const LONG_SENTINEL: &str =
        "quartz-private-payload-with-a-distinct-and-deliberately-long-shape";
    const CONTROL_SENTINEL: [u8; 3] = [0x02, 0x03, 0x04];
    const INVALID_UTF8_SENTINEL: [u8; 2] = [0xf5, 0xff];

    type ErrorMetadata = (
        FormatCodecErrorKind,
        FormatCodecPhase,
        Option<usize>,
        Option<usize>,
        Option<&'static str>,
        Option<DecoderKind>,
        Option<usize>,
        Option<ListProfile>,
    );

    const fn error_metadata_is_const(error: &FormatCodecError) -> ErrorMetadata {
        (
            error.kind(),
            error.phase(),
            error.row(),
            error.field(),
            error.field_name(),
            error.expected(),
            error.offset(),
            error.profile(),
        )
    }

    fn slot_descriptor_signature(slot: &ParsedSlot<'_>) -> &'static FormatDescriptor {
        ParsedSlot::descriptor(slot)
    }

    fn slot_bytes_signature<'row>(
        _lifetime: std::marker::PhantomData<&'row ()>,
        slot: &ParsedSlot<'row>,
    ) -> &'row [u8] {
        ParsedSlot::as_bytes(slot)
    }

    fn plan(descriptors: Vec<&'static FormatDescriptor>) -> FormatPlan {
        FormatPlan::for_codec_test(descriptors)
            .ok()
            .expect("non-empty static descriptor plan is valid")
    }

    fn plan_at(version: &str, descriptors: Vec<&'static FormatDescriptor>) -> FormatPlan {
        let raw = format!("tmux {version}\n");
        FormatPlan::for_codec_test_at(descriptors, &parse_version(raw.as_bytes()))
            .ok()
            .expect("non-empty static descriptor plan is valid")
    }

    /// The value tmux 3.4 was live-characterized against.
    ///
    /// It carries a field terminator, a row terminator, a `#{q:}` special, a
    /// multibyte character, and two bytes that are invalid UTF-8.
    const ADVERSARIAL_VALUE: [u8; 9] = [0x3a, 0x0a, 0x25, 0x5c, 0xe2, 0x90, 0x9e, 0x80, 0xff];

    /// The exact stdout tmux 3.2a, 3.6, and 3.7b emit for that value.
    const RAW_Q_WIRE: [u8; 13] = [
        0x3a, 0x0a, 0x5c, 0x25, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x80, 0xff, 0x25, 0x0a,
    ];

    /// The exact stdout tmux 3.4 emits for that value.
    const VIS_WIRE: [u8; 19] = [
        0x3a, 0x0a, 0x5c, 0x25, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x5c, 0x32, 0x30, 0x30, 0x5c, 0x33,
        0x37, 0x37, 0x25, 0x0a,
    ];

    #[test]
    fn transport_dialect_follows_the_upstream_vis_window() {
        for (version, expected) in [
            ("3.2a", TransportDialect::RawQ),
            ("3.3a", TransportDialect::RawQ),
            ("3.4", TransportDialect::Vis),
            ("3.5", TransportDialect::Vis),
            ("3.5a", TransportDialect::Vis),
            ("3.6", TransportDialect::RawQ),
            ("3.7b", TransportDialect::RawQ),
            // `next-3.5` is the tree between 3.4 and 3.5, so it still visually
            // encodes even though it names no released version.
            ("next-3.5", TransportDialect::Vis),
            ("next-3.8", TransportDialect::RawQ),
            ("master", TransportDialect::RawQ),
        ] {
            let raw = format!("tmux {version}\n");
            let parsed = TmuxVersion::parse_output(raw.as_bytes())
                .ok()
                .expect("fixture version parses");

            assert_eq!(
                TransportDialect::for_version(&parsed),
                expected,
                "{version} selects its documented dialect",
            );
        }
    }

    #[test]
    fn format_codec_recovers_adversarial_bytes_on_every_supported_dialect() {
        for (version, wire) in [
            ("3.2a", RAW_Q_WIRE.as_slice()),
            ("3.4", VIS_WIRE.as_slice()),
            ("3.5a", VIS_WIRE.as_slice()),
            ("3.6", RAW_Q_WIRE.as_slice()),
            ("3.7b", RAW_Q_WIRE.as_slice()),
        ] {
            let plan = plan_at(version, vec![&FIRST]);
            let parsed = rows(&plan, wire);
            let slots = parsed[0]
                .slots()
                .map(|slot| slot.as_bytes())
                .collect::<Vec<_>>();

            assert_eq!(parsed.len(), 1, "{version} frames one row");
            assert_eq!(
                slots,
                [ADVERSARIAL_VALUE.as_slice()],
                "{version} recovers exact bytes"
            );
        }
    }

    #[test]
    fn format_codec_rejects_vis_output_when_the_daemon_predates_the_probe() {
        // The version probe reads the client executable, but the daemon that
        // owns the socket decides the transport. A 3.6 client against a 3.5
        // daemon previously decoded `\200\377` into the ASCII text `200377`.
        let plan = plan_at("3.6", vec![&FIRST]);
        let error = error(plan.parse_rows(&VIS_WIRE));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::InvalidEscape,
            FormatCodecPhase::Escape,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(10),
        );
    }

    #[test]
    fn format_codec_rejects_raw_high_bytes_when_the_daemon_visually_encodes() {
        // The mirrored mismatch: a 3.4 plan reading raw output. The first
        // escape that is not a `#{q:}` special must fail rather than decode.
        let plan = plan_at("3.4", vec![&FIRST]);
        let mut wire = Vec::from(b"value\\n%".as_slice());
        wire.push(b'\n');

        let error = error(plan.parse_rows(&wire));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::InvalidEscape,
            FormatCodecPhase::Escape,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(6),
        );
    }

    #[test]
    fn format_codec_round_trips_every_byte_through_the_vis_dialect() {
        let plan = plan_at("3.5", vec![&FIRST]);

        for byte in 1..=u8::MAX {
            let mut wire = Vec::new();
            encode_like_tmux_vis(byte, &mut wire);
            wire.push(b'%');
            wire.push(b'\n');

            let parsed = rows(&plan, &wire);
            let slots = parsed[0]
                .slots()
                .map(|slot| slot.as_bytes())
                .collect::<Vec<_>>();

            assert_eq!(slots, [[byte].as_slice()], "byte {byte:#04x} round-trips");
        }
    }

    /// Reproduce tmux's `#{q:}` then `VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH` output
    /// for one single-byte value.
    fn encode_like_tmux_vis(byte: u8, wire: &mut Vec<u8>) {
        if QUOTE_SHELL_SPECIALS.contains(&byte) {
            wire.push(b'\\');
            wire.push(byte);
            return;
        }
        // `isvisible` keeps printable ASCII, space, tab, and newline literal.
        if byte.is_ascii_graphic() || matches!(byte, b' ' | b'\t' | b'\n') {
            wire.push(byte);
            return;
        }
        if let Some(letter) = [
            (0x07, b'a'),
            (0x08, b'b'),
            (0x0b, b'v'),
            (0x0c, b'f'),
            (0x0d, b'r'),
        ]
        .into_iter()
        .find_map(|(control, letter)| (byte == control).then_some(letter))
        {
            wire.push(b'\\');
            wire.push(letter);
            return;
        }
        wire.push(b'\\');
        wire.push(b'0' + (byte >> 6));
        wire.push(b'0' + ((byte >> 3) & 0o7));
        wire.push(b'0' + (byte & 0o7));
    }

    fn rows(plan: &FormatPlan, stdout: &[u8]) -> Vec<ParsedRow> {
        plan.parse_rows(stdout)
            .ok()
            .expect("fixture is a valid framed row stream")
    }

    fn error<T>(result: Result<T, FormatCodecError>) -> FormatCodecError {
        result.err().expect("fixture is rejected")
    }

    fn sensitive_prefix() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SHORT_SENTINEL.as_bytes());
        bytes.push(b'_');
        bytes.extend_from_slice(LONG_SENTINEL.as_bytes());
        bytes.push(b'_');
        bytes.extend_from_slice(&CONTROL_SENTINEL);
        bytes.push(b'_');
        bytes.extend_from_slice(&INVALID_UTF8_SENTINEL);
        bytes
    }

    fn assert_safe_diagnostic(error: &FormatCodecError) {
        assert!(std::error::Error::source(error).is_none());

        let display = error.to_string();
        let debug = format!("{error:?}");
        for rendered in [&display, &debug] {
            assert!(!rendered.contains(SHORT_SENTINEL));
            assert!(!rendered.contains(LONG_SENTINEL));
            assert!(
                !rendered
                    .as_bytes()
                    .windows(CONTROL_SENTINEL.len())
                    .any(|window| window == CONTROL_SENTINEL)
            );
            assert!(!rendered.contains("[2, 3, 4]"));
            assert!(!rendered.contains("\\x02\\x03\\x04"));
            assert!(!rendered.contains("[245, 255]"));
            assert!(!rendered.contains("2, 3, 4, 95, 245, 255"));
            assert!(!rendered.contains("0xf5"));
            assert!(!rendered.contains("0xff"));
            assert!(!rendered.contains("\\xf5"));
            assert!(!rendered.contains("\\xff"));
            assert!(!rendered.contains('\u{fffd}'));
        }
    }

    #[test]
    fn format_codec_private_metadata_signatures_are_exact() {
        const TEST_DESCRIPTOR: FormatDescriptor =
            FormatDescriptor::for_codec_test("const", DecoderKind::Ascii);

        assert_impl_all!(DecoderKind: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(SemanticOwner: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(RequiredContext: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(EmptyPolicy: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(InfoPlacement: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(ListProfile: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(ListProfiles: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(FormatDescriptor: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(PlanPurpose: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(PlanFieldState: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(FormatCodecErrorKind: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
        assert_impl_all!(FormatCodecPhase: Clone, Copy, std::fmt::Debug, Eq, PartialEq);

        const DECODER: DecoderKind =
            FormatDescriptor::for_codec_test("const", DecoderKind::Ascii).decoder();
        assert_eq!(DECODER, DecoderKind::Ascii);

        assert_eq!(TEST_DESCRIPTOR.owner(), SemanticOwner::Session);
        assert_eq!(TEST_DESCRIPTOR.required_context(), RequiredContext::Session);
        assert!(
            [
                ListProfile::Sessions,
                ListProfile::Windows,
                ListProfile::Panes,
                ListProfile::Clients,
            ]
            .into_iter()
            .all(|profile| TEST_DESCRIPTOR.profiles().contains(profile))
        );
        assert_eq!(
            TEST_DESCRIPTOR.minimum_release(),
            TmuxVersion::MIN_SUPPORTED
        );
        assert_eq!(TEST_DESCRIPTOR.empty_policy(), EmptyPolicy::Available);
        assert_eq!(TEST_DESCRIPTOR.placement(), InfoPlacement::CatalogOnly);

        let descriptor_constructor: fn(&'static str, DecoderKind) -> FormatDescriptor =
            FormatDescriptor::for_codec_test;
        let plan_constructor: fn(
            Vec<&'static FormatDescriptor>,
        ) -> Result<FormatPlan, FormatCodecError> = FormatPlan::for_codec_test;
        let template: for<'plan> fn(&'plan FormatPlan) -> &'plan str = FormatPlan::template;
        let descriptor_name: for<'descriptor> fn(&'descriptor FormatDescriptor) -> &'static str =
            FormatDescriptor::name;
        let descriptor_decoder: fn(&FormatDescriptor) -> DecoderKind = FormatDescriptor::decoder;
        let slot_descriptor: fn(&ParsedSlot<'static>) -> &'static FormatDescriptor =
            ParsedSlot::descriptor;
        let slot_bytes: fn(&ParsedSlot<'static>) -> &'static [u8] = ParsedSlot::as_bytes;
        let generic_slot_descriptor: fn(&ParsedSlot<'_>) -> &'static FormatDescriptor =
            slot_descriptor_signature;
        let generic_slot_bytes: for<'row> fn(
            std::marker::PhantomData<&'row ()>,
            &ParsedSlot<'row>,
        ) -> &'row [u8] = slot_bytes_signature;
        let error_metadata: fn(&FormatCodecError) -> ErrorMetadata = error_metadata_is_const;
        let error_kind: fn(&FormatCodecError) -> FormatCodecErrorKind = FormatCodecError::kind;
        let error_phase: fn(&FormatCodecError) -> FormatCodecPhase = FormatCodecError::phase;
        let error_row: fn(&FormatCodecError) -> Option<usize> = FormatCodecError::row;
        let error_field: fn(&FormatCodecError) -> Option<usize> = FormatCodecError::field;
        let error_field_name: fn(&FormatCodecError) -> Option<&'static str> =
            FormatCodecError::field_name;
        let error_expected: fn(&FormatCodecError) -> Option<DecoderKind> =
            FormatCodecError::expected;
        let error_offset: fn(&FormatCodecError) -> Option<usize> = FormatCodecError::offset;
        let error_profile: fn(&FormatCodecError) -> Option<ListProfile> = FormatCodecError::profile;

        let _ = (
            descriptor_constructor,
            plan_constructor,
            template,
            descriptor_name,
            descriptor_decoder,
            slot_descriptor,
            slot_bytes,
            generic_slot_descriptor,
            generic_slot_bytes,
            error_metadata,
            error_kind,
            error_phase,
            error_row,
            error_field,
            error_field_name,
            error_expected,
            error_offset,
            error_profile,
        );
        assert!(!std::mem::needs_drop::<FormatCodecError>());
    }

    #[test]
    fn format_codec_normal_profiles_store_exact_identity_and_version_evidence() {
        let cases: [(
            ListProfile,
            &'static FormatDescriptor,
            SemanticOwner,
            RequiredContext,
            DecoderKind,
            usize,
        ); 4] = [
            (
                ListProfile::Sessions,
                &SESSION_ID,
                SemanticOwner::Session,
                RequiredContext::Session,
                DecoderKind::SessionId,
                9,
            ),
            (
                ListProfile::Windows,
                &WINDOW_ID,
                SemanticOwner::Window,
                RequiredContext::Window,
                DecoderKind::WindowId,
                11,
            ),
            (
                ListProfile::Panes,
                &PANE_ID,
                SemanticOwner::Pane,
                RequiredContext::Pane,
                DecoderKind::PaneId,
                57,
            ),
            (
                ListProfile::Clients,
                &CLIENT_NAME,
                SemanticOwner::Client,
                RequiredContext::Client,
                DecoderKind::Text,
                22,
            ),
        ];

        for (profile, baseline, owner, required_context, decoder, selected_count) in cases {
            let plan = {
                let version = TmuxVersion::parse_output(b"tmux 3.3\n")
                    .ok()
                    .expect("fixture version is valid");
                FormatPlan::for_profile(profile, &version)
            };

            assert_eq!(plan.profile, profile);
            let detected = match &plan.version {
                PlanVersion::Detected(version) => Some(version),
                PlanVersion::MinimumSupportedFixture => None,
            };
            assert_eq!(detected.map(TmuxVersion::raw), Some("3.3"));
            assert!(std::ptr::eq(plan.baseline, baseline));
            assert!(std::ptr::eq(plan.descriptors[0], baseline));
            assert_eq!(
                plan.descriptors
                    .iter()
                    .filter(|descriptor| std::ptr::eq(**descriptor, baseline))
                    .count(),
                1
            );
            assert_eq!(baseline.owner(), owner);
            assert_eq!(baseline.required_context(), required_context);
            assert_eq!(baseline.minimum_release(), TmuxVersion::MIN_SUPPORTED);
            assert_eq!(baseline.decoder(), decoder);
            assert_eq!(baseline.empty_policy(), EmptyPolicy::Required);
            assert_eq!(plan.descriptors.len(), selected_count);
            assert_eq!(plan.template().matches("#{q:").count(), selected_count);
            assert!(
                plan.template()
                    .starts_with(&format!("#{{q:{}}}%", baseline.name()))
            );
            assert_eq!(plan.purpose, PlanPurpose::Intrinsic(baseline.placement()));
        }
    }

    #[test]
    fn format_codec_selector_filters_versions_and_deduplicates_baseline_identity() {
        assert_eq!(POST_BASELINE.owner(), SemanticOwner::Session);
        assert_eq!(POST_BASELINE.required_context(), RequiredContext::Session);
        assert!(POST_BASELINE.profiles().contains(ListProfile::Sessions));
        assert_eq!(
            POST_BASELINE.minimum_release(),
            ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL)
        );
        assert_eq!(POST_BASELINE.decoder(), DecoderKind::Text);
        assert_eq!(POST_BASELINE.empty_policy(), EmptyPolicy::Absent);
        assert_eq!(POST_BASELINE.placement(), InfoPlacement::CatalogOnly);

        let cases: &[(&[u8], &[&FormatDescriptor], &str)] = &[
            (b"tmux 3.2a\n", &[&SESSION_ID], "#{q:session_id}%"),
            (
                b"tmux 3.3\n",
                &[&SESSION_ID, &POST_BASELINE],
                "#{q:session_id}%#{q:post_baseline}%",
            ),
            (b"tmux master\n", &[&SESSION_ID], "#{q:session_id}%"),
            (b"tmux next-3.4\n", &[&SESSION_ID], "#{q:session_id}%"),
        ];

        for (raw, expected, template) in cases {
            let version = TmuxVersion::parse_output(raw)
                .ok()
                .expect("fixture version is valid");
            let plan =
                for_profile_selection_test(ListProfile::Sessions, &version, &SESSION_SUPPLEMENTS);

            assert_eq!(plan.descriptors.len(), expected.len());
            for (actual, expected) in plan.descriptors.iter().zip(*expected) {
                assert!(std::ptr::eq(*actual, *expected));
            }
            assert_eq!(plan.template(), *template);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_codec_error(
        error: &FormatCodecError,
        kind: FormatCodecErrorKind,
        phase: FormatCodecPhase,
        row: Option<usize>,
        field: Option<usize>,
        field_name: Option<&'static str>,
        expected: Option<DecoderKind>,
        offset: Option<usize>,
    ) {
        assert_eq!(error.kind(), kind);
        assert_eq!(error.phase(), phase);
        assert_eq!(error.row(), row);
        assert_eq!(error.field(), field);
        assert_eq!(error.field_name(), field_name);
        assert_eq!(error.expected(), expected);
        assert_eq!(error.offset(), offset);
        assert_eq!(error.profile(), None);
        assert_safe_diagnostic(error);
    }

    #[test]
    fn format_codec_empty_plan_is_rejected_before_rendering_or_parsing() {
        let error = error(FormatPlan::for_codec_test(Vec::new()));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::EmptyPlan,
            FormatCodecPhase::Plan,
            None,
            None,
            None,
            None,
            None,
        );
    }

    #[test]
    fn format_codec_template_has_one_expansion_token_per_descriptor() {
        let plan = plan(vec![&FIRST, &SECOND]);
        let template = plan.template();

        assert_eq!(FIRST.name(), "first");
        assert_eq!(FIRST.decoder(), DecoderKind::Text);
        assert_eq!(SECOND.name(), "second");
        assert_eq!(SECOND.decoder(), DecoderKind::Ascii);
        assert_eq!(template, "#{q:first}%#{q:second}%");
        assert_eq!(template.matches("#{q:first}").count(), 1);
        assert_eq!(template.matches("#{q:second}").count(), 1);
        assert_eq!(template.matches('%').count(), 2);
        assert!(!template.contains('\n'));
        assert!(!template.contains("length"));
        assert!(!template.contains("separator"));
    }

    #[test]
    fn format_codec_parser_signature_owns_descriptor_order_in_the_plan() {
        let parse_rows: for<'plan, 'stdout> fn(
            &'plan FormatPlan,
            &'stdout [u8],
        )
            -> Result<Vec<ParsedRow>, FormatCodecError> = FormatPlan::parse_rows;

        let parsed = parse_rows(&plan(vec![&FIRST]), b"value%\n")
            .ok()
            .expect("exact parser function accepts raw stdout only");
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn format_codec_empty_stdout_and_empty_field_have_distinct_cardinality() {
        let plan = plan(vec![&FIRST]);

        assert!(rows(&plan, b"").is_empty());

        let parsed = rows(&plan, b"%\n");
        assert_eq!(parsed.len(), 1);
        let mut slots = parsed[0].slots();
        assert_eq!(slots.len(), 1);
        let slot = slots.next().expect("one empty slot exists");
        assert!(std::ptr::eq(
            std::ptr::from_ref(slot.descriptor()),
            std::ptr::addr_of!(FIRST)
        ));
        assert_eq!(slot.as_bytes(), b"");
        assert!(slots.next().is_none());
    }

    #[test]
    fn format_codec_ordinary_punctuation_and_separator_bytes_are_payload() {
        let plan = plan(vec![&FIRST]);
        let stdout = b":,/]\xe2\x90\x9e%\n";
        let parsed = rows(&plan, stdout);
        let slot = parsed[0].slots().next().expect("one slot exists");

        assert_eq!(slot.as_bytes(), b":,/]\xe2\x90\x9e");
    }

    #[test]
    fn format_codec_controls_and_non_utf8_bytes_remain_exact_payload() {
        let plan = plan(vec![&FIRST]);
        let cases: &[(&[u8], &[u8])] = &[
            (b"a\nb\rc%\n", b"a\nb\rc"),
            (b"\x80\xff%\n", b"\x80\xff"),
            (b"tail\n%\n", b"tail\n"),
        ];

        for (stdout, expected) in cases {
            let parsed = rows(&plan, stdout);
            let slot = parsed[0].slots().next().expect("one slot exists");
            assert_eq!(slot.as_bytes(), *expected);
        }
    }

    #[test]
    fn format_codec_backslash_consumes_exactly_one_q_special() {
        let plan = plan(vec![&FIRST]);
        let cases: &[(&[u8], &[u8])] = &[
            (b"\\\\%\n", b"\\"),
            (b"\\%%\n", b"%"),
            (b"\\|\\[%\n", b"|["),
        ];

        for (stdout, expected) in cases {
            let parsed = rows(&plan, stdout);
            let slot = parsed[0].slots().next().expect("one slot exists");
            assert_eq!(slot.as_bytes(), *expected);
        }
    }

    #[test]
    fn format_codec_rejects_escapes_tmux_never_emits() {
        // `#{q:}` escapes a closed set. A backslash before anything else means
        // the output did not come from the dialect the plan selected, so it
        // must fail rather than silently drop the backslash.
        let plan = plan(vec![&FIRST]);

        for stdout in [
            b"\\:%\n".as_slice(),
            b"\\]%\n".as_slice(),
            b"\\z%\n".as_slice(),
        ] {
            let error = error(plan.parse_rows(stdout));

            assert_codec_error(
                &error,
                FormatCodecErrorKind::InvalidEscape,
                FormatCodecPhase::Escape,
                Some(0),
                Some(0),
                Some("first"),
                None,
                Some(1),
            );
        }
    }

    #[test]
    fn production_q_escape_set_matches_the_documented_tmux_set() {
        assert_eq!(QUOTE_SHELL_SPECIALS, Q_SHELL_ESCAPED);
    }

    #[test]
    fn format_codec_tmux_3_2a_q_escape_set_round_trips_exactly() {
        assert_eq!(
            Q_SHELL_ESCAPED,
            [
                0x7c, 0x26, 0x3b, 0x3c, 0x3e, 0x28, 0x29, 0x24, 0x60, 0x5c, 0x22, 0x27, 0x2a, 0x3f,
                0x5b, 0x23, 0x20, 0x3d, 0x25,
            ]
        );

        let mut stdout = Vec::with_capacity(Q_SHELL_ESCAPED.len() * 2 + 2);
        for byte in Q_SHELL_ESCAPED {
            stdout.extend_from_slice(&[b'\\', byte]);
        }
        stdout.extend_from_slice(b"%\n");

        let plan = plan(vec![&FIRST]);
        let parsed = rows(&plan, &stdout);
        let slot = parsed[0].slots().next().expect("one slot exists");
        assert_eq!(slot.as_bytes(), Q_SHELL_ESCAPED);
    }

    #[test]
    fn format_codec_multiple_rows_and_fields_preserve_plan_order() {
        let plan = plan(vec![&FIRST, &SECOND]);
        let parsed = rows(&plan, b"left%right%\nnext%last%\n");

        let pairs = parsed
            .iter()
            .map(|row| {
                row.slots()
                    .map(|slot| (slot.descriptor().name(), slot.as_bytes().to_vec()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pairs,
            vec![
                vec![("first", b"left".to_vec()), ("second", b"right".to_vec())],
                vec![("first", b"next".to_vec()), ("second", b"last".to_vec())],
            ]
        );

        let mut first_row_slots = parsed[0].slots();
        let first_slot = first_row_slots.next().expect("first slot exists");
        let second_slot = first_row_slots.next().expect("second slot exists");
        assert!(std::ptr::eq(
            std::ptr::from_ref(first_slot.descriptor()),
            std::ptr::addr_of!(FIRST)
        ));
        assert!(std::ptr::eq(
            std::ptr::from_ref(second_slot.descriptor()),
            std::ptr::addr_of!(SECOND)
        ));
    }

    #[test]
    fn format_codec_slots_are_contiguous_ranges_of_one_row_buffer() {
        let plan = plan(vec![&FIRST, &SECOND]);
        let parsed = rows(&plan, b"a\\%b%tail%\n");
        let mut slots = parsed[0].slots();
        let first = slots.next().expect("first slot exists");
        let second = slots.next().expect("second slot exists");

        assert_eq!(first.as_bytes(), b"a%b");
        assert_eq!(second.as_bytes(), b"tail");
        assert_eq!(
            first.as_bytes().as_ptr() as usize + first.as_bytes().len(),
            second.as_bytes().as_ptr() as usize
        );
    }

    #[test]
    fn format_codec_reversing_descriptors_reverses_template_and_slot_identity() {
        let forward = plan(vec![&FIRST, &SECOND]);
        let reverse = plan(vec![&SECOND, &FIRST]);

        assert_eq!(forward.template(), "#{q:first}%#{q:second}%");
        assert_eq!(reverse.template(), "#{q:second}%#{q:first}%");

        let forward_rows = rows(&forward, b"a%b%\n");
        let reverse_rows = rows(&reverse, b"a%b%\n");
        let forward_pairs = forward_rows[0]
            .slots()
            .map(|slot| (slot.descriptor().name(), slot.as_bytes()))
            .collect::<Vec<_>>();
        let reverse_pairs = reverse_rows[0]
            .slots()
            .map(|slot| (slot.descriptor().name(), slot.as_bytes()))
            .collect::<Vec<_>>();

        assert_eq!(
            forward_pairs,
            [("first", b"a".as_slice()), ("second", b"b".as_slice())]
        );
        assert_eq!(
            reverse_pairs,
            [("second", b"a".as_slice()), ("first", b"b".as_slice())]
        );
    }

    #[test]
    fn format_codec_embedded_lf_in_a_later_field_is_not_row_cardinality() {
        let plan = plan(vec![&FIRST, &SECOND]);
        let parsed = rows(&plan, b"a%\nb%\n");
        let slots = parsed[0]
            .slots()
            .map(|slot| slot.as_bytes())
            .collect::<Vec<_>>();

        assert_eq!(parsed.len(), 1);
        assert_eq!(slots, [b"a".as_slice(), b"\nb".as_slice()]);
    }

    #[test]
    fn format_codec_dangling_escape_reports_raw_eof() {
        let mut stdout = sensitive_prefix();
        stdout.push(b'\\');
        let offset = stdout.len();
        let plan = plan(vec![&FIRST]);
        let error = error(plan.parse_rows(&stdout));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::DanglingEscape,
            FormatCodecPhase::Escape,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(offset),
        );
    }

    #[test]
    fn format_codec_missing_field_terminator_reports_raw_eof() {
        let stdout = sensitive_prefix();
        let offset = stdout.len();
        let plan = plan(vec![&FIRST]);
        let error = error(plan.parse_rows(&stdout));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::MissingFieldTerminator,
            FormatCodecPhase::Field,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(offset),
        );
    }

    #[test]
    fn format_codec_final_field_without_row_lf_reports_raw_eof() {
        let mut stdout = sensitive_prefix();
        stdout.push(b'%');
        let offset = stdout.len();
        let plan = plan(vec![&FIRST]);
        let error = error(plan.parse_rows(&stdout));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::MissingRowLf,
            FormatCodecPhase::RowTerminator,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(offset),
        );
    }

    #[test]
    fn format_codec_crlf_and_extra_slots_are_unexpected_row_terminators() {
        let plan = plan(vec![&FIRST]);
        let mut crlf = sensitive_prefix();
        crlf.extend_from_slice(b"%\r\n");
        let cr_offset = crlf.len() - 2;
        let cr_error = error(plan.parse_rows(&crlf));
        assert_codec_error(
            &cr_error,
            FormatCodecErrorKind::UnexpectedRowTerminator,
            FormatCodecPhase::RowTerminator,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(cr_offset),
        );

        let mut extra = sensitive_prefix();
        extra.extend_from_slice(b"%extra%\n");
        let extra_offset = extra.len() - b"extra%\n".len();
        let extra_error = error(plan.parse_rows(&extra));
        assert_codec_error(
            &extra_error,
            FormatCodecErrorKind::UnexpectedRowTerminator,
            FormatCodecPhase::RowTerminator,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(extra_offset),
        );
    }

    #[test]
    fn format_codec_valid_row_then_partial_row_reports_later_coordinates() {
        let mut stdout = b"ok%\n".to_vec();
        stdout.extend_from_slice(&sensitive_prefix());
        let offset = stdout.len();
        let plan = plan(vec![&FIRST]);
        let error = error(plan.parse_rows(&stdout));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::MissingFieldTerminator,
            FormatCodecPhase::Field,
            Some(1),
            Some(0),
            Some("first"),
            None,
            Some(offset),
        );
    }

    #[test]
    fn format_codec_multifield_underflow_and_overflow_report_plan_coordinates() {
        let plan = plan(vec![&FIRST, &SECOND]);
        let mut underflow = sensitive_prefix();
        underflow.push(b'%');
        let underflow_offset = underflow.len();
        let underflow_error = error(plan.parse_rows(&underflow));
        assert_codec_error(
            &underflow_error,
            FormatCodecErrorKind::MissingFieldTerminator,
            FormatCodecPhase::Field,
            Some(0),
            Some(1),
            Some("second"),
            None,
            Some(underflow_offset),
        );

        let mut overflow = sensitive_prefix();
        overflow.extend_from_slice(b"%second%extra%\n");
        let overflow_offset = overflow.len() - b"extra%\n".len();
        let overflow_error = error(plan.parse_rows(&overflow));
        assert_codec_error(
            &overflow_error,
            FormatCodecErrorKind::UnexpectedRowTerminator,
            FormatCodecPhase::RowTerminator,
            Some(0),
            Some(1),
            Some("second"),
            None,
            Some(overflow_offset),
        );
    }

    #[test]
    fn format_codec_nul_is_rejected_at_the_earliest_raw_boundary() {
        let plan = plan(vec![&FIRST]);
        let mut ordinary = sensitive_prefix();
        ordinary.extend_from_slice(b"\0%\n");
        let ordinary_offset = ordinary.len() - 3;
        let ordinary_error = error(plan.parse_rows(&ordinary));
        assert_codec_error(
            &ordinary_error,
            FormatCodecErrorKind::EmbeddedNul,
            FormatCodecPhase::Field,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(ordinary_offset),
        );

        let mut escaped = sensitive_prefix();
        escaped.extend_from_slice(b"\\\0%\n");
        let escaped_offset = escaped.len() - 3;
        let escaped_error = error(plan.parse_rows(&escaped));
        assert_codec_error(
            &escaped_error,
            FormatCodecErrorKind::EmbeddedNul,
            FormatCodecPhase::Escape,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(escaped_offset),
        );
    }

    #[test]
    fn format_codec_bare_lf_is_not_an_empty_row() {
        let plan = plan(vec![&FIRST]);
        let error = error(plan.parse_rows(b"\n"));

        assert_codec_error(
            &error,
            FormatCodecErrorKind::MissingFieldTerminator,
            FormatCodecPhase::Field,
            Some(0),
            Some(0),
            Some("first"),
            None,
            Some(1),
        );
    }

    #[test]
    fn format_codec_row_terminator_precedence_is_local_to_reached_state() {
        let plan = plan(vec![&FIRST]);
        let cases = [
            (b"value%\0".as_slice(), FormatCodecErrorKind::EmbeddedNul, 6),
            (
                b"value%\\\0".as_slice(),
                FormatCodecErrorKind::UnexpectedRowTerminator,
                6,
            ),
            (
                b"value%x\0".as_slice(),
                FormatCodecErrorKind::UnexpectedRowTerminator,
                6,
            ),
        ];

        for (stdout, kind, offset) in cases {
            let error = error(plan.parse_rows(stdout));
            assert_codec_error(
                &error,
                kind,
                FormatCodecPhase::RowTerminator,
                Some(0),
                Some(0),
                Some("first"),
                None,
                Some(offset),
            );
        }
    }

    #[derive(Debug)]
    struct CheckedCatalogRow<'fixture> {
        field: &'fixture str,
        owner: &'fixture str,
        context: &'fixture str,
        profiles: &'fixture str,
        minimum: &'fixture str,
        decoder: &'fixture str,
        empty: &'fixture str,
        placement: &'fixture str,
    }

    fn checked_catalog_rows() -> Vec<CheckedCatalogRow<'static>> {
        const DOCUMENT: &str = include_str!("../docs/parity.md");
        const BEGIN: &str = "<!-- BEGIN CHECKED FORMAT CATALOG -->";
        const END: &str = "<!-- END CHECKED FORMAT CATALOG -->";
        const HEADER: &str =
            "| field | owner | context | profiles | minimum | decoder | empty | placement |";
        const SEPARATOR: &str = "| --- | --- | --- | --- | --- | --- | --- | --- |";

        assert_eq!(DOCUMENT.matches(BEGIN).count(), 1);
        assert_eq!(DOCUMENT.matches(END).count(), 1);
        let (_, after_begin) = DOCUMENT
            .split_once(BEGIN)
            .expect("checked catalog begin marker exists");
        let (region, _) = after_begin
            .split_once(END)
            .expect("checked catalog end marker exists");
        let mut lines = region.trim_matches('\n').lines();
        assert_eq!(lines.next(), Some(HEADER));
        assert_eq!(lines.next(), Some(SEPARATOR));

        let rows = lines
            .map(|line| {
                assert!(line.starts_with("| "));
                assert!(line.ends_with(" |"));
                let cells = line
                    .trim_matches('|')
                    .split('|')
                    .map(str::trim)
                    .collect::<Vec<_>>();
                assert_eq!(cells.len(), 8);
                assert!(cells.iter().all(|cell| !cell.is_empty()));
                let field = cells[0]
                    .strip_prefix('`')
                    .and_then(|value| value.strip_suffix('`'))
                    .expect("field cell is one code span");
                CheckedCatalogRow {
                    field,
                    owner: cells[1],
                    context: cells[2],
                    profiles: cells[3],
                    minimum: cells[4],
                    decoder: cells[5],
                    empty: cells[6],
                    placement: cells[7],
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 178);
        rows
    }

    const fn owner_token(owner: SemanticOwner) -> &'static str {
        match owner {
            SemanticOwner::Server => "server",
            SemanticOwner::ListRow => "list-row",
            SemanticOwner::Mode => "mode",
            SemanticOwner::Buffer => "buffer",
            SemanticOwner::Client => "client",
            SemanticOwner::ClientAttachment => "client-attachment",
            SemanticOwner::ClientWindowView => "client-window-view",
            SemanticOwner::Session => "session",
            SemanticOwner::Window => "window",
            SemanticOwner::WindowLink => "window-link",
            SemanticOwner::Pane => "pane",
            SemanticOwner::Command => "command",
            SemanticOwner::Config => "config",
            SemanticOwner::CopyMode => "copy-mode",
        }
    }

    const fn context_token(context: RequiredContext) -> &'static str {
        match context {
            RequiredContext::None => "none",
            RequiredContext::FormatType => "format-type",
            RequiredContext::ListRow => "list-row",
            RequiredContext::Buffer => "buffer",
            RequiredContext::Client => "client",
            RequiredContext::Session => "session",
            RequiredContext::Window => "window",
            RequiredContext::WindowLink => "window-link",
            RequiredContext::Pane => "pane",
            RequiredContext::Command => "command",
            RequiredContext::Config => "config",
            RequiredContext::CopyMode => "copy-mode",
        }
    }

    fn profiles_token(profiles: ListProfiles) -> &'static str {
        let admission = [
            profiles.contains(ListProfile::Sessions),
            profiles.contains(ListProfile::Windows),
            profiles.contains(ListProfile::Panes),
            profiles.contains(ListProfile::Clients),
        ];
        match admission {
            [true, true, true, true] => "all",
            [false, false, false, true] => "clients",
            [false, false, false, false] => "none",
            _ => "noncanonical",
        }
    }

    fn minimum_token(minimum: ReleaseVersion) -> &'static str {
        if minimum == TmuxVersion::MIN_SUPPORTED {
            "3.2a"
        } else if minimum == ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL) {
            "3.3"
        } else if minimum == ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL) {
            "3.7"
        } else {
            "unexpected"
        }
    }

    const fn decoder_token(decoder: DecoderKind) -> &'static str {
        match decoder {
            DecoderKind::Ascii => "ascii",
            DecoderKind::Text => "text",
            DecoderKind::Bool => "bool",
            DecoderKind::U8 => "u8",
            DecoderKind::U32 => "u32",
            DecoderKind::U64 => "u64",
            DecoderKind::I32 => "i32",
            DecoderKind::Timestamp => "timestamp",
            DecoderKind::SessionId => "session-id",
            DecoderKind::WindowId => "window-id",
            DecoderKind::PaneId => "pane-id",
            DecoderKind::PaneProgress => "pane-progress",
            DecoderKind::PaneProgressState => "pane-progress-state",
        }
    }

    const fn empty_token(policy: EmptyPolicy) -> &'static str {
        match policy {
            EmptyPolicy::Required => "required",
            EmptyPolicy::Absent => "absent",
            EmptyPolicy::Available => "available",
        }
    }

    const fn placement_token(placement: InfoPlacement) -> &'static str {
        match placement {
            InfoPlacement::CatalogOnly => "catalog-only",
            InfoPlacement::SessionInfo => "session-info",
            InfoPlacement::WindowInfo => "window-info",
            InfoPlacement::PaneInfo => "pane-info",
            InfoPlacement::ClientInfo => "client-info",
        }
    }

    fn descriptor(name: &str) -> &'static FormatDescriptor {
        CATALOG
            .iter()
            .copied()
            .find(|descriptor| descriptor.name() == name)
            .expect("checked field has one trusted descriptor")
    }

    fn parse_version(raw: &[u8]) -> TmuxVersion {
        TmuxVersion::parse_output(raw)
            .ok()
            .expect("catalog fixture version is valid")
    }

    fn descriptor_names(descriptors: &'static [&'static FormatDescriptor]) -> Vec<&'static str> {
        descriptors
            .iter()
            .map(|descriptor| descriptor.name())
            .collect()
    }

    fn expected_info_names(
        rows: &[CheckedCatalogRow<'static>],
        placement: &str,
        identity: &'static str,
    ) -> Vec<&'static str> {
        let mut names = rows
            .iter()
            .filter(|row| row.placement == placement && row.field != identity)
            .map(|row| row.field)
            .collect::<Vec<_>>();
        names.insert(0, identity);
        names
    }

    fn count_tokens<'value>(
        values: impl IntoIterator<Item = &'value str>,
    ) -> std::collections::BTreeMap<&'value str, usize> {
        let mut counts = std::collections::BTreeMap::new();
        for value in values {
            *counts.entry(value).or_insert(0) += 1;
        }
        counts
    }

    fn descriptor_address(descriptor: &FormatDescriptor) -> usize {
        std::ptr::from_ref(descriptor) as usize
    }

    #[test]
    fn format_catalog_checked_parity_is_an_exact_sorted_bijection() {
        let rows = checked_catalog_rows();
        assert_eq!(CATALOG.len(), 178);
        assert_eq!(GROUPED_CATALOG.len(), 178);

        let mut names = std::collections::BTreeSet::new();
        let mut pointers = std::collections::BTreeSet::new();
        for (index, (row, descriptor)) in rows.iter().zip(CATALOG).enumerate() {
            assert!(names.insert(descriptor.name()));
            assert!(pointers.insert(descriptor_address(descriptor)));
            if let Some(previous) = index.checked_sub(1) {
                assert!(CATALOG[previous].name() < descriptor.name());
            }
            assert_eq!(descriptor.name(), row.field);
            assert_eq!(owner_token(descriptor.owner()), row.owner);
            assert_eq!(context_token(descriptor.required_context()), row.context);
            assert_eq!(profiles_token(descriptor.profiles()), row.profiles);
            assert_eq!(minimum_token(descriptor.minimum_release()), row.minimum);
            assert_eq!(decoder_token(descriptor.decoder()), row.decoder);
            assert_eq!(empty_token(descriptor.empty_policy()), row.empty);
            assert_eq!(placement_token(descriptor.placement()), row.placement);
        }

        let grouped_pointers = GROUPED_CATALOG
            .iter()
            .map(|descriptor| descriptor_address(descriptor))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(grouped_pointers.len(), 178);
        assert_eq!(grouped_pointers, pointers);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive parity partition is one checked contract"
    )]
    fn format_catalog_checked_parity_partitions_are_exact() {
        let rows = checked_catalog_rows();
        assert_eq!(
            count_tokens(rows.iter().map(|row| row.minimum)),
            std::collections::BTreeMap::from([("3.2a", 159), ("3.3", 8), ("3.7", 11)])
        );
        assert_eq!(
            count_tokens(rows.iter().map(|row| row.profiles)),
            std::collections::BTreeMap::from([("all", 134), ("clients", 27), ("none", 17)])
        );
        assert_eq!(
            count_tokens(rows.iter().map(|row| row.empty)),
            std::collections::BTreeMap::from([
                ("absent", 30),
                ("available", 25),
                ("required", 123),
            ])
        );
        assert_eq!(
            count_tokens(rows.iter().map(|row| row.placement)),
            std::collections::BTreeMap::from([
                ("catalog-only", 68),
                ("client-info", 22),
                ("pane-info", 68),
                ("session-info", 9),
                ("window-info", 11),
            ])
        );
        assert_eq!(
            count_tokens(rows.iter().map(|row| row.decoder)),
            std::collections::BTreeMap::from([
                ("bool", 49),
                ("i32", 14),
                ("pane-id", 1),
                ("pane-progress", 1),
                ("pane-progress-state", 1),
                ("session-id", 2),
                ("text", 55),
                ("timestamp", 8),
                ("u32", 41),
                ("u64", 4),
                ("u8", 1),
                ("window-id", 1),
            ])
        );
        assert_eq!(
            count_tokens(rows.iter().map(|row| row.context)),
            std::collections::BTreeMap::from([
                ("buffer", 3),
                ("client", 27),
                ("command", 3),
                ("config", 1),
                ("copy-mode", 10),
                ("format-type", 3),
                ("list-row", 1),
                ("none", 9),
                ("pane", 69),
                ("session", 22),
                ("window", 11),
                ("window-link", 19),
            ])
        );

        let fields_3_3 = rows
            .iter()
            .filter(|row| row.minimum == "3.3")
            .map(|row| row.field)
            .collect::<Vec<_>>();
        assert_eq!(
            fields_3_3,
            [
                "client_uid",
                "client_user",
                "next_session_id",
                "pane_dead_signal",
                "pane_dead_time",
                "pane_start_path",
                "uid",
                "user",
            ]
        );
        let fields_3_7 = rows
            .iter()
            .filter(|row| row.minimum == "3.7")
            .map(|row| row.field)
            .collect::<Vec<_>>();
        assert_eq!(
            fields_3_7,
            [
                "bracket_paste_flag",
                "pane_flags",
                "pane_floating_flag",
                "pane_pb_progress",
                "pane_pb_state",
                "pane_pipe_pid",
                "pane_x",
                "pane_y",
                "pane_z",
                "pane_zoomed_flag",
                "synchronized_output_flag",
            ]
        );
        assert!(
            rows.iter()
                .all(|row| !matches!(row.minimum, "3.4" | "3.5" | "3.6"))
        );
        assert!(
            rows.iter()
                .filter(|row| row.profiles == "none")
                .all(|row| row.placement == "catalog-only")
        );

        let mode = rows
            .iter()
            .find(|row| row.field == "client_mode_format")
            .expect("mode descriptor is checked");
        assert_eq!(
            (
                mode.owner,
                mode.context,
                mode.profiles,
                mode.minimum,
                mode.decoder,
                mode.placement,
            ),
            ("mode", "none", "all", "3.2a", "text", "catalog-only")
        );
    }

    #[test]
    fn format_catalog_info_orders_and_supplements_are_exact() {
        let rows = checked_catalog_rows();
        let cases = [
            (
                "session-info",
                "session_id",
                SESSION_INFO_DESCRIPTORS,
                SESSION_INFO_SUPPLEMENTS,
            ),
            (
                "window-info",
                "window_id",
                WINDOW_INFO_DESCRIPTORS,
                WINDOW_INFO_SUPPLEMENTS,
            ),
            (
                "pane-info",
                "pane_id",
                PANE_INFO_DESCRIPTORS,
                PANE_INFO_SUPPLEMENTS,
            ),
            (
                "client-info",
                "client_name",
                CLIENT_INFO_DESCRIPTORS,
                CLIENT_INFO_SUPPLEMENTS,
            ),
        ];

        for (placement, identity, complete, supplements) in cases {
            let expected = expected_info_names(&rows, placement, identity);
            assert_eq!(descriptor_names(complete), expected);
            assert_eq!(descriptor_names(supplements), expected[1..]);
            assert!(std::ptr::eq(complete[0], descriptor(identity)));
            assert!(
                supplements
                    .iter()
                    .all(|item| !std::ptr::eq(*item, complete[0]))
            );
        }
    }

    #[test]
    fn format_catalog_profile_plans_are_baseline_first_and_exact_once() {
        let versions = [
            (b"tmux 3.2a\n".as_slice(), [9, 11, 54, 20]),
            (b"tmux 3.3\n".as_slice(), [9, 11, 57, 22]),
            (b"tmux 3.6\n".as_slice(), [9, 11, 57, 22]),
            (b"tmux 3.7\n".as_slice(), [9, 11, 68, 22]),
            (b"tmux 3.8\n".as_slice(), [9, 11, 68, 22]),
            (b"tmux master\n".as_slice(), [9, 11, 54, 20]),
            (b"tmux next-3.8\n".as_slice(), [9, 11, 54, 20]),
        ];
        let profiles = [
            (ListProfile::Sessions, SESSION_INFO_DESCRIPTORS),
            (ListProfile::Windows, WINDOW_INFO_DESCRIPTORS),
            (ListProfile::Panes, PANE_INFO_DESCRIPTORS),
            (ListProfile::Clients, CLIENT_INFO_DESCRIPTORS),
        ];

        for (raw, expected_counts) in versions {
            let version = parse_version(raw);
            for ((profile, complete), selected_count) in profiles.into_iter().zip(expected_counts) {
                let plan = FormatPlan::for_profile(profile, &version);
                assert_eq!(plan.planned.len(), complete.len());
                assert_eq!(plan.descriptors.len(), selected_count);
                assert!(std::ptr::eq(plan.baseline, complete[0]));
                assert!(std::ptr::eq(plan.descriptors[0], complete[0]));
                assert_eq!(
                    plan.descriptors
                        .iter()
                        .filter(|item| std::ptr::eq(**item, complete[0]))
                        .count(),
                    1
                );
                assert_eq!(
                    plan.purpose,
                    PlanPurpose::Intrinsic(complete[0].placement())
                );

                let mut next_slot = 0;
                for (planned, expected) in plan.planned.iter().zip(complete) {
                    assert!(std::ptr::eq(planned.descriptor, *expected));
                    if let PlanFieldState::Selected { slot } = planned.state {
                        assert_eq!(slot, next_slot);
                        assert!(std::ptr::eq(plan.descriptors[slot], planned.descriptor));
                        next_slot += 1;
                    }
                }
                assert_eq!(next_slot, selected_count);
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the exhaustive version matrix is one availability contract"
    )]
    fn format_catalog_numbered_and_development_availability_is_exact() {
        let version_3_2a = parse_version(b"tmux 3.2a\n");
        let version_3_3 = parse_version(b"tmux 3.3\n");
        let version_3_6 = parse_version(b"tmux 3.6\n");
        let version_3_7 = parse_version(b"tmux 3.7\n");
        let development = [
            parse_version(b"tmux master\n"),
            parse_version(b"tmux next-3.8\n"),
        ];
        let corrected = [
            "client_uid",
            "client_user",
            "next_session_id",
            "pane_start_path",
            "uid",
            "user",
        ];

        for descriptor in CATALOG
            .iter()
            .copied()
            .filter(|descriptor| descriptor.minimum_release() > TmuxVersion::MIN_SUPPORTED)
        {
            let profile = if descriptor.profiles().contains(ListProfile::Sessions) {
                ListProfile::Sessions
            } else {
                ListProfile::Clients
            };
            let (lower, supported) = if descriptor.minimum_release()
                == ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL)
            {
                (&version_3_2a, &version_3_3)
            } else {
                (&version_3_6, &version_3_7)
            };
            let lower_plan = FormatPlan::for_descriptors(profile, lower, &[descriptor])
                .ok()
                .expect("post-baseline descriptor is in scope");
            assert_eq!(lower_plan.descriptors.len(), 1);
            assert_eq!(lower_plan.planned.len(), 2);
            assert_eq!(lower_plan.planned[1].state, PlanFieldState::Unsupported);

            let supported_plan = FormatPlan::for_descriptors(profile, supported, &[descriptor])
                .ok()
                .expect("post-baseline descriptor is in scope");
            assert_eq!(supported_plan.descriptors.len(), 2);
            assert_eq!(
                supported_plan.planned[1].state,
                PlanFieldState::Selected { slot: 1 }
            );

            for version in &development {
                let plan = FormatPlan::for_descriptors(profile, version, &[descriptor])
                    .ok()
                    .expect("post-baseline descriptor is in scope");
                assert_eq!(plan.descriptors.len(), 1);
                assert_eq!(plan.planned[1].state, PlanFieldState::Unproven);
            }
        }

        for descriptor in CATALOG.iter().copied().filter(|descriptor| {
            descriptor.minimum_release() == TmuxVersion::MIN_SUPPORTED
                && profiles_token(descriptor.profiles()) != "none"
        }) {
            let profile = if descriptor.profiles().contains(ListProfile::Sessions) {
                ListProfile::Sessions
            } else {
                ListProfile::Clients
            };
            for version in &development {
                let plan = FormatPlan::for_descriptors(profile, version, &[descriptor])
                    .ok()
                    .expect("baseline descriptor is in scope");
                assert!(
                    plan.planned.iter().any(|field| {
                        std::ptr::eq(field.descriptor, descriptor)
                            && matches!(field.state, PlanFieldState::Selected { .. })
                    }),
                    "baseline descriptor must remain selected: {}",
                    descriptor.name()
                );
            }
        }

        for name in corrected {
            let descriptor = descriptor(name);
            assert_eq!(
                descriptor.minimum_release(),
                ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL)
            );
            let profile = if descriptor.profiles().contains(ListProfile::Sessions) {
                ListProfile::Sessions
            } else {
                ListProfile::Clients
            };
            let below = FormatPlan::for_descriptors(profile, &version_3_2a, &[descriptor])
                .ok()
                .expect("corrected descriptor is in scope");
            let at_floor = FormatPlan::for_descriptors(profile, &version_3_3, &[descriptor])
                .ok()
                .expect("corrected descriptor is in scope");
            assert_eq!(below.planned[1].state, PlanFieldState::Unsupported);
            assert_eq!(
                at_floor.planned[1].state,
                PlanFieldState::Selected { slot: 1 }
            );
        }

        for version in development {
            let unproven = [
                ListProfile::Sessions,
                ListProfile::Windows,
                ListProfile::Panes,
                ListProfile::Clients,
            ]
            .into_iter()
            .map(|profile| {
                FormatPlan::for_profile(profile, &version)
                    .planned
                    .iter()
                    .filter(|field| field.state == PlanFieldState::Unproven)
                    .count()
            })
            .sum::<usize>();
            assert_eq!(unproven, 16);
        }
    }

    #[test]
    fn format_catalog_static_requests_reject_scope_and_duplicates_first() {
        let version_3_2a = parse_version(b"tmux 3.2a\n");
        let client = descriptor("client_uid");
        let buffer = descriptor("buffer_name");
        let post_baseline = descriptor("next_session_id");

        for (profile, requested, name) in [
            (ListProfile::Sessions, client, "client_uid"),
            (ListProfile::Clients, buffer, "buffer_name"),
        ] {
            let error = FormatPlan::for_descriptors(profile, &version_3_2a, &[requested])
                .err()
                .expect("out-of-scope descriptor is rejected");
            assert_eq!(error.kind(), FormatCodecErrorKind::ScopeInapplicable);
            assert_eq!(error.phase(), FormatCodecPhase::Plan);
            assert_eq!(error.field_name(), Some(name));
            assert_eq!(error.profile(), Some(profile));
            assert_eq!(error.row(), None);
            assert_eq!(error.field(), None);
            assert_eq!(error.expected(), None);
            assert_eq!(error.offset(), None);
            assert_safe_diagnostic(&error);
        }

        let duplicate = FormatPlan::for_descriptors(
            ListProfile::Sessions,
            &version_3_2a,
            &[post_baseline, post_baseline],
        )
        .err()
        .expect("repeated post-baseline supplement is rejected");
        assert_eq!(duplicate.kind(), FormatCodecErrorKind::DuplicateDescriptor);
        assert_eq!(duplicate.phase(), FormatCodecPhase::Plan);
        assert_eq!(duplicate.field_name(), Some("next_session_id"));
        assert_eq!(duplicate.profile(), Some(ListProfile::Sessions));
        assert_eq!(duplicate.row(), None);
        assert_eq!(duplicate.field(), None);
        assert_eq!(duplicate.expected(), None);
        assert_eq!(duplicate.offset(), None);
        assert_safe_diagnostic(&duplicate);

        let repeated_baseline = FormatPlan::for_descriptors(
            ListProfile::Sessions,
            &version_3_2a,
            &[&SESSION_ID, &SESSION_ID],
        )
        .ok()
        .expect("baseline inputs are idempotent");
        assert_eq!(repeated_baseline.descriptors.len(), 1);
        assert_eq!(repeated_baseline.planned.len(), 1);
        assert_eq!(repeated_baseline.purpose, PlanPurpose::Projection);
    }

    #[test]
    fn format_catalog_static_requests_preserve_caller_order() {
        let version = parse_version(b"tmux 3.7\n");
        let activity = descriptor("session_activity");
        let windows = descriptor("session_windows");
        assert!(activity.name() < windows.name());

        let plan =
            FormatPlan::for_descriptors(ListProfile::Sessions, &version, &[windows, activity])
                .ok()
                .expect("same-profile supplements are accepted");
        assert_eq!(plan.purpose, PlanPurpose::Projection);
        assert_eq!(plan.planned.len(), 3);
        assert_eq!(plan.descriptors.len(), 3);
        for (index, expected) in [&SESSION_ID, windows, activity].into_iter().enumerate() {
            assert!(std::ptr::eq(plan.planned[index].descriptor, expected));
            assert_eq!(
                plan.planned[index].state,
                PlanFieldState::Selected { slot: index }
            );
            assert!(std::ptr::eq(plan.descriptors[index], expected));
        }
        assert_eq!(
            plan.template(),
            "#{q:session_id}%#{q:session_windows}%#{q:session_activity}%"
        );
    }

    #[test]
    fn format_catalog_global_mode_field_is_universal_but_never_intrinsic() {
        let version = parse_version(b"tmux 3.2a\n");
        let descriptor = descriptor("client_mode_format");
        assert_eq!(descriptor.owner(), SemanticOwner::Mode);
        assert_eq!(descriptor.required_context(), RequiredContext::None);
        assert_eq!(descriptor.placement(), InfoPlacement::CatalogOnly);
        for profile in [
            ListProfile::Sessions,
            ListProfile::Windows,
            ListProfile::Panes,
            ListProfile::Clients,
        ] {
            let plan = FormatPlan::for_descriptors(profile, &version, &[descriptor])
                .ok()
                .expect("global mode field is admitted by every list profile");
            assert_eq!(plan.descriptors.len(), 2);
            assert_eq!(plan.planned.len(), 2);
            assert_eq!(plan.planned[1].state, PlanFieldState::Selected { slot: 1 });
            assert_eq!(plan.purpose, PlanPurpose::Projection);
        }
        assert!(
            [
                SESSION_INFO_DESCRIPTORS,
                WINDOW_INFO_DESCRIPTORS,
                PANE_INFO_DESCRIPTORS,
                CLIENT_INFO_DESCRIPTORS,
            ]
            .into_iter()
            .flatten()
            .all(|item| !std::ptr::eq(*item, descriptor))
        );
    }

    #[cfg(feature = "test-support")]
    async fn format_compat_test_server() -> (TestServer, std::ffi::OsString) {
        let executable = std::env::var_os("LIBTMUX_TEST_TMUX")
            .unwrap_or_else(|| std::ffi::OsString::from("tmux"));
        let guard = TestServer::builder()
            .tmux_executable(executable.clone())
            .start()
            .await
            .expect("the explicitly selected tmux starts");
        assert!(
            guard.server().tmux_executable() == executable.as_os_str(),
            "TestServer retains the selected tmux executable",
        );
        (guard, executable)
    }

    #[cfg(feature = "test-support")]
    async fn format_compat_run(server: &crate::Server, command: Command) {
        let result = server
            .cmd(command)
            .await
            .expect("tmux fixture command executes");
        assert!(result.success(), "tmux fixture command succeeds");
        assert!(result.stderr().is_empty(), "tmux fixture stderr is empty");
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn real_tmux_compat_format_q_matches_versioned_adversarial_option_transport() {
        use std::os::unix::ffi::OsStringExt as _;

        const OPTION_BYTES: [u8; 9] = [0x3a, 0x0a, 0x25, 0x5c, 0xe2, 0x90, 0x9e, 0x80, 0xff];
        const EXPECTED_RAW_STDOUT: [u8; 13] = [
            0x3a, 0x0a, 0x5c, 0x25, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x80, 0xff, 0x25, 0x0a,
        ];
        const EXPECTED_VIS_STDOUT: [u8; 19] = [
            0x3a, 0x0a, 0x5c, 0x25, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x5c, 0x32, 0x30, 0x30, 0x5c,
            0x33, 0x37, 0x37, 0x25, 0x0a,
        ];

        let (guard, executable) = format_compat_test_server().await;
        let server = guard.server();
        if let Some(expected) = std::env::var_os("LIBTMUX_TEST_TMUX") {
            assert!(
                executable == expected,
                "the selected executable matches LIBTMUX_TEST_TMUX",
            );
        }
        assert!(
            server.tmux_executable() == executable.as_os_str(),
            "the Server retains the selected tmux executable",
        );

        format_compat_run(
            server,
            Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg("format-bytes")
                .arg("sleep 120"),
        )
        .await;
        format_compat_run(
            server,
            Command::new("set-option")
                .arg("-g")
                .arg(RAW_FORMAT_BYTES.name())
                .sensitive_arg(std::ffi::OsString::from_vec(OPTION_BYTES.to_vec())),
        )
        .await;

        let plan = plan(vec![&RAW_FORMAT_BYTES]);
        assert_eq!(plan.template(), "#{q:@libtmux_format_bytes}%");
        assert_eq!(
            plan.template()
                .matches("#{q:@libtmux_format_bytes}")
                .count(),
            1,
        );
        let result = server
            .cmd(Command::new("list-sessions").arg("-F").arg(plan.template()))
            .await
            .expect("the raw format listing executes");
        assert!(result.success(), "the raw format listing succeeds");
        assert!(result.stderr().is_empty(), "the raw format stderr is empty");
        let version = server
            .capabilities()
            .await
            .expect("tmux capabilities are detected")
            .tmux_version();
        match TransportDialect::for_version(version) {
            TransportDialect::Vis => {
                assert_eq!(result.stdout(), EXPECTED_VIS_STDOUT);
                // The visual encoding makes the transport valid UTF-8 even
                // though the underlying value is not.
                assert!(result.stdout_utf8().is_ok());
            }
            TransportDialect::RawQ => {
                assert_eq!(result.stdout(), EXPECTED_RAW_STDOUT);
                assert!(result.stdout_utf8().is_err());
            }
        }

        // The decoded value is the same on every dialect. This is the claim
        // the crate makes to callers, so it is asserted on the live transport
        // rather than only on the raw-q lane.
        let versioned = FormatPlan::for_codec_test_at(vec![&RAW_FORMAT_BYTES], version)
            .ok()
            .expect("a plan exists for the detected version");
        let parsed = versioned
            .parse_rows(result.stdout())
            .ok()
            .expect("the owning plan parses borrowed live stdout");
        assert_eq!(parsed.len(), 1);
        let mut slots = parsed[0].slots();
        assert_eq!(slots.len(), 1);
        let slot = slots.next().expect("the one format slot exists");
        assert!(std::ptr::eq(
            std::ptr::from_ref(slot.descriptor()),
            std::ptr::addr_of!(RAW_FORMAT_BYTES),
        ));
        assert_eq!(decode_text(slot).as_bytes(), OPTION_BYTES);
        assert!(slots.next().is_none());

        guard.shutdown().await.expect("tmux fixture shuts down");
    }
}
