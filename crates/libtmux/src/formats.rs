//! Byte-preserving tmux text values.

use crate::version::{ReleaseSuffix, ReleaseVersion, TmuxVersion};

mod plan;
mod row;
mod text;

pub(crate) use plan::{FormatPlan, PlanFieldState, PlanPurpose};
#[cfg(test)]
use plan::{PlanVersion, TransportDialect, for_profile_selection_test};
#[cfg(test)]
use row::QUOTE_SHELL_SPECIALS;
pub(crate) use row::{FormatCodecError, FormatCodecErrorKind, ParsedRow, ParsedSlot, decode_text};
#[cfg(test)]
pub(crate) use row::{FormatCodecPhase, decode_ascii};
pub use text::TmuxText;

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

/// Which list commands can resolve a field.
///
/// A set of [`ListProfile`], and deliberately not named for it: the two sat
/// one letter apart in this file and a reader took the busy one for the
/// catalogue-only one.
#[allow(
    dead_code,
    reason = "catalog-only metadata is verified by the checked parity fixture"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProfileSet(u8);

#[allow(
    dead_code,
    reason = "catalog-only metadata is verified by the checked parity fixture"
)]
impl ProfileSet {
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
    profiles: ProfileSet,
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
    pub(crate) const fn profiles(&self) -> ProfileSet {
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
            profiles: ProfileSet::all(),
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
                    (SESSION_NAME, session_name, "session_name", Session, Session, All, V3_2A, Text, Available),
                    (SESSION_PATH, session_path, "session_path", Session, Session, All, V3_2A, Text, Available),
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
                    (PANE_UNSEEN_CHANGES, pane_unseen_changes, "pane_unseen_changes", Pane, Pane, All, V3_4, Bool, Required),
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
                    (WINDOW_LINKED_SESSIONS_LIST, window_linked_sessions_list, "window_linked_sessions_list", Window, WindowLink, All, V3_2A, Text, Available),
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
        ProfileSet::all()
    };
    (Clients) => {
        ProfileSet::clients()
    };
    (None) => {
        ProfileSet::none()
    };
}

macro_rules! catalog_floor {
    (V3_2A) => {
        TmuxVersion::MIN_SUPPORTED
    };
    (V3_3) => {
        ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL)
    };
    (V3_4) => {
        ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL)
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

/// Tmux list command profile represented by a format plan.
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
#[cfg(test)]
mod tests;
