#![allow(clippy::err_expect, clippy::expect_used, clippy::ok_expect)]

#[cfg(feature = "query")]
use super::{
    ClientFields, ClientInfo, PaneFields, PaneProgressState, SessionFields, SessionInfo,
    WindowFields,
};
#[cfg(any(feature = "query", feature = "test-support"))]
use crate::PaneId;
#[cfg(feature = "query")]
use crate::formats::{CLIENT_INFO_DESCRIPTORS, CLIENT_LAST_SESSION, CLIENT_SESSION};
use crate::formats::{
    CLIENT_MODE_FORMAT, DecoderKind, FormatCodecError, FormatCodecErrorKind, FormatCodecPhase,
    FormatDescriptor, FormatPlan, ListProfile, PANE_INFO_DESCRIPTORS, PANE_INFO_SUPPLEMENTS,
    ParsedRow, ParsedSlot, PlanFieldState, PlanPurpose, SESSION_GROUP_LIST, SESSION_ID,
    WINDOW_ACTIVE, WINDOW_ACTIVE_CLIENTS, WINDOW_ACTIVE_CLIENTS_LIST, WINDOW_ACTIVE_SESSIONS,
    WINDOW_ACTIVE_SESSIONS_LIST, WINDOW_ACTIVITY_FLAG, WINDOW_BELL_FLAG, WINDOW_END_FLAG,
    WINDOW_FLAGS, WINDOW_ID, WINDOW_INDEX, WINDOW_INFO_DESCRIPTORS, WINDOW_INFO_SUPPLEMENTS,
    WINDOW_LAST_FLAG, WINDOW_LINKED, WINDOW_LINKED_SESSIONS, WINDOW_LINKED_SESSIONS_LIST,
    WINDOW_MARKED_FLAG, WINDOW_RAW_FLAGS, WINDOW_SILENCE_FLAG, WINDOW_STACK_INDEX,
    WINDOW_START_FLAG, decode_ascii, decode_text,
};
#[cfg(all(feature = "query", feature = "serde"))]
use crate::query::FilterExpr;
#[cfg(feature = "query")]
use crate::query::{BoolField, EnumField, FilterEnum, Filterable, IntegerField, TextField};
use crate::target::WindowLinkIdentity;
#[cfg(feature = "test-support")]
use crate::test::TestServer;
#[cfg(feature = "test-support")]
use crate::{Command, ReleaseSuffix, ReleaseVersion};
use crate::{ServerIdentity, SessionId, TmuxText, TmuxVersion, WindowId};
use static_assertions::{assert_impl_all, assert_not_impl_any};

#[cfg(feature = "test-support")]
use super::hydrate_session_infos_from_stdout;
use super::{
    Availability, PaneInfo, PaneProjection, PointSelectionError, WindowInfo, WindowLink,
    WindowProjection, holder_session_ids, hydrate_client_info, hydrate_pane_info,
    hydrate_pane_projection, hydrate_pane_projections, hydrate_pane_projections_from_stdout,
    hydrate_session_info, hydrate_window_info, hydrate_window_projection,
    hydrate_window_projections, hydrate_window_projections_from_stdout, pane_projection_plan,
    select_local_window_projection, validate_planned_row_for_test, window_projection_plan,
};

static ASCII: FormatDescriptor = FormatDescriptor::for_codec_test("ascii", DecoderKind::Ascii);
static TEXT: FormatDescriptor = FormatDescriptor::for_codec_test("text", DecoderKind::Text);
static FIRST_TEXT: FormatDescriptor = FormatDescriptor::for_codec_test("first", DecoderKind::Text);
static SECOND_ASCII: FormatDescriptor =
    FormatDescriptor::for_codec_test("second", DecoderKind::Ascii);

const SHORT_SENTINEL: &str = "zot-private";
const LONG_SENTINEL: &str = "quartz-private-payload-with-a-distinct-and-deliberately-long-shape";
const CONTROL_SENTINEL: [u8; 3] = [0x02, 0x03, 0x04];
const INVALID_UTF8_SENTINEL: [u8; 2] = [0xf5, 0xff];

fn plan(descriptors: Vec<&'static FormatDescriptor>) -> FormatPlan {
    FormatPlan::for_codec_test(descriptors)
        .ok()
        .expect("non-empty static descriptor plan is valid")
}

fn rows(plan: &FormatPlan, stdout: &[u8]) -> Vec<ParsedRow> {
    plan.parse_rows(stdout)
        .ok()
        .expect("fixture is a valid framed row stream")
}

fn error<T>(result: Result<T, FormatCodecError>) -> FormatCodecError {
    result.err().expect("fixture is rejected")
}

fn decode_ascii_signature<'row>(
    _lifetime: std::marker::PhantomData<&'row ()>,
    slot: ParsedSlot<'row>,
) -> Result<&'row str, FormatCodecError> {
    decode_ascii(slot)
}

fn decode_text_signature(slot: ParsedSlot<'_>) -> TmuxText {
    decode_text(slot)
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

fn assert_non_ascii_error(
    error: &FormatCodecError,
    row: usize,
    field: usize,
    field_name: &'static str,
    offset: usize,
) {
    assert_eq!(error.kind(), FormatCodecErrorKind::NonAscii);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(row));
    assert_eq!(error.field(), Some(field));
    assert_eq!(error.field_name(), Some(field_name));
    assert_eq!(error.expected(), Some(DecoderKind::Ascii));
    assert_eq!(error.offset(), Some(offset));
    assert_eq!(error.profile(), None);
    assert_safe_diagnostic(error);
}

fn version(raw: &[u8]) -> TmuxVersion {
    TmuxVersion::parse_output(raw)
        .ok()
        .expect("snapshot fixture version is valid")
}

fn default_value(descriptor: &FormatDescriptor) -> &'static [u8] {
    match descriptor.name() {
        "session_id" => b"$1",
        "window_id" => b"@1",
        "pane_id" => b"%1",
        "client_name" => b"client",
        _ => match descriptor.decoder() {
            DecoderKind::Ascii => b"ascii",
            DecoderKind::Text => b"text",
            DecoderKind::Bool
            | DecoderKind::U8
            | DecoderKind::U32
            | DecoderKind::U64
            | DecoderKind::I32
            | DecoderKind::Timestamp
            | DecoderKind::PaneProgress => b"0",
            DecoderKind::SessionId => b"$1",
            DecoderKind::WindowId => b"@1",
            DecoderKind::PaneId => b"%1",
            DecoderKind::PaneProgressState => b"normal",
        },
    }
}

fn push_q_field(stdout: &mut Vec<u8>, value: &[u8]) {
    for byte in value {
        if matches!(*byte, b'\\' | b'%') {
            stdout.push(b'\\');
        }
        stdout.push(*byte);
    }
    stdout.push(b'%');
}

fn framed_row(
    plan: &FormatPlan,
    overrides: &[(&str, &[u8])],
) -> (Vec<u8>, std::collections::BTreeMap<&'static str, usize>) {
    let mut stdout = Vec::new();
    let mut offsets = std::collections::BTreeMap::new();
    for descriptor in plan.descriptors_for_test() {
        offsets.insert(descriptor.name(), stdout.len());
        let value = overrides
            .iter()
            .find_map(|(name, value)| (*name == descriptor.name()).then_some(*value))
            .unwrap_or_else(|| default_value(descriptor));
        push_q_field(&mut stdout, value);
    }
    stdout.push(b'\n');
    (stdout, offsets)
}

fn parsed_row(
    plan: &FormatPlan,
    overrides: &[(&str, &[u8])],
) -> (ParsedRow, std::collections::BTreeMap<&'static str, usize>) {
    let (stdout, offsets) = framed_row(plan, overrides);
    let mut rows = plan
        .parse_rows(&stdout)
        .ok()
        .expect("snapshot fixture is framed by the real parser");
    assert_eq!(rows.len(), 1);
    (rows.remove(0), offsets)
}

#[cfg(feature = "query")]
fn session_fixture(
    raw_version: &[u8],
    overrides: &[(&str, &[u8])],
) -> Result<SessionInfo, FormatCodecError> {
    let plan = FormatPlan::for_profile(ListProfile::Sessions, &version(raw_version));
    let (row, _) = parsed_row(&plan, overrides);
    hydrate_session_info(&plan, &row)
}

#[cfg(feature = "query")]
fn window_fixture(
    raw_version: &[u8],
    overrides: &[(&str, &[u8])],
) -> Result<WindowInfo, FormatCodecError> {
    let plan = FormatPlan::for_profile(ListProfile::Windows, &version(raw_version));
    let (row, _) = parsed_row(&plan, overrides);
    hydrate_window_info(&plan, &row)
}

fn pane_fixture(
    raw_version: &[u8],
    overrides: &[(&str, &[u8])],
) -> Result<PaneInfo, FormatCodecError> {
    let plan = FormatPlan::for_profile(ListProfile::Panes, &version(raw_version));
    let (row, _) = parsed_row(&plan, overrides);
    hydrate_pane_info(&plan, &row)
}

#[cfg(feature = "query")]
fn client_fixture(
    raw_version: &[u8],
    overrides: &[(&str, &[u8])],
) -> Result<ClientInfo, FormatCodecError> {
    let plan = FormatPlan::for_profile(ListProfile::Clients, &version(raw_version));
    let (row, _) = parsed_row(&plan, overrides);
    hydrate_client_info(&plan, &row)
}

fn assert_invalid_value(
    error: &FormatCodecError,
    plan: &FormatPlan,
    offsets: &std::collections::BTreeMap<&'static str, usize>,
    field_name: &'static str,
    expected: DecoderKind,
) {
    let field = plan
        .descriptors_for_test()
        .iter()
        .position(|descriptor| descriptor.name() == field_name)
        .expect("tested field is selected");
    assert_eq!(error.kind(), FormatCodecErrorKind::InvalidValue);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), Some(field));
    assert_eq!(error.field_name(), Some(field_name));
    assert_eq!(error.expected(), Some(expected));
    assert_eq!(error.offset(), offsets.get(field_name).copied());
    assert_eq!(error.profile(), None);
    assert_safe_diagnostic(error);
}

#[test]
fn format_codec_decoder_function_signatures_consume_parsed_slots() {
    let ascii: fn(ParsedSlot<'static>) -> Result<&'static str, FormatCodecError> = decode_ascii;
    let text: fn(ParsedSlot<'static>) -> TmuxText = decode_text;
    let generic_ascii: for<'row> fn(
        std::marker::PhantomData<&'row ()>,
        ParsedSlot<'row>,
    ) -> Result<&'row str, FormatCodecError> = decode_ascii_signature;
    let generic_text: fn(ParsedSlot<'_>) -> TmuxText = decode_text_signature;

    let _ = (ascii, text, generic_ascii, generic_text);
}

#[test]
fn format_codec_ascii_accepts_controls_and_ascii_boundaries() {
    let plan = plan(vec![&ASCII]);
    let parsed = rows(&plan, b"\x01A\x7f%\n");
    let slot = parsed[0].slots().next().expect("one slot exists");
    let decoded = decode_ascii(slot)
        .ok()
        .expect("all ASCII bytes are accepted");

    assert_eq!(decoded.as_bytes(), b"\x01A\x7f");
}

#[test]
fn format_codec_ascii_rejects_raw_and_utf8_non_ascii_bytes() {
    let plan = plan(vec![&ASCII]);
    for stdout in [b"\x80%\n".as_slice(), b"\xc3\xa9%\n".as_slice()] {
        let parsed = rows(&plan, stdout);
        let slot = parsed[0].slots().next().expect("one slot exists");
        let error = error(decode_ascii(slot));
        assert_non_ascii_error(&error, 0, 0, "ascii", 0);
    }
}

#[test]
fn format_codec_ascii_error_diagnostics_do_not_retain_slot_payload() {
    let mut stdout = Vec::new();
    stdout.extend_from_slice(SHORT_SENTINEL.as_bytes());
    stdout.push(b'_');
    stdout.extend_from_slice(LONG_SENTINEL.as_bytes());
    stdout.push(b'_');
    stdout.extend_from_slice(&CONTROL_SENTINEL);
    stdout.push(b'_');
    stdout.extend_from_slice(&INVALID_UTF8_SENTINEL);
    stdout.extend_from_slice(b"%\n");

    let plan = plan(vec![&ASCII]);
    let parsed = rows(&plan, &stdout);
    let slot = parsed[0].slots().next().expect("one slot exists");
    let error = error(decode_ascii(slot));
    assert_non_ascii_error(&error, 0, 0, "ascii", 0);
}

#[test]
fn format_codec_ascii_error_uses_later_raw_row_field_and_slot_offset() {
    let plan = plan(vec![&FIRST_TEXT, &SECOND_ASCII]);
    let parsed = rows(&plan, b"x%y%\na\\%b%\x80%\n");
    let slot = parsed[1]
        .slots()
        .nth(1)
        .expect("later row has a second slot");
    let error = error(decode_ascii(slot));

    assert_non_ascii_error(&error, 1, 1, "second", 10);
}

#[test]
fn format_codec_text_preserves_empty_controls_and_non_utf8_bytes() {
    let plan = plan(vec![&TEXT]);
    let cases: &[(&[u8], &[u8])] = &[
        (b"%\n", b""),
        (b"\n%\n", b"\n"),
        (b"\r%\n", b"\r"),
        (b"\x01\x7f%\n", b"\x01\x7f"),
        (b"\x80\xff%\n", b"\x80\xff"),
    ];

    for (stdout, expected) in cases {
        let parsed = rows(&plan, stdout);
        let slot = parsed[0].slots().next().expect("one slot exists");
        let decoded = decode_text(slot);
        assert_eq!(decoded.as_bytes(), *expected);
    }
}

/// Assert a field's stored shape, which follows its declared policy.
///
/// `flat` is for a field that cannot be absent, and `evidence` for one
/// that can. Splitting them here is what keeps the derivation honest: a
/// field whose policy changes fails to compile until this says so.
#[cfg(feature = "query")]
macro_rules! assert_stored_fields {
    ($value:expr, $type:ty, flat, [$($field:ident),+ $(,)?]) => {
        $(
            let _: &$type = &$value.$field;
        )+
    };
    ($value:expr, $type:ty, evidence, [$($field:ident),+ $(,)?]) => {
        $(
            let _: &Availability<$type> = &$value.$field;
        )+
    };
}

#[cfg(feature = "query")]
macro_rules! assert_text_handles {
    (
        $value:expr,
        $info:ty,
        $fixture:ident,
        $raw:expr,
        $expected:expr,
        [$($field:ident),+ $(,)?]
    ) => {
        $(
            let handle: &TextField<$info> = &$value.$field;
            assert_eq!(
                *handle,
                crate::query::__private::text_field::<$info>(
                    <$info as Filterable>::FILTER_TARGET,
                    stringify!($field),
                )
            );
            let candidate = $fixture(b"tmux 3.7\n", &[(stringify!($field), $raw)])
                .ok()
                .expect(concat!("distinct ", stringify!($field), " fixture hydrates"));
            assert!((*handle).eq($expected).matches(&candidate));
        )+
    };
}

#[cfg(feature = "query")]
macro_rules! assert_bool_handles {
    ($value:expr, $info:ty, $fixture:ident, [$($field:ident),+ $(,)?]) => {
        $(
            let handle: &BoolField<$info> = &$value.$field;
            assert_eq!(
                *handle,
                crate::query::__private::bool_field::<$info>(
                    <$info as Filterable>::FILTER_TARGET,
                    stringify!($field),
                )
            );
            let candidate = $fixture(b"tmux 3.7\n", &[(stringify!($field), b"1")])
                .ok()
                .expect(concat!("distinct ", stringify!($field), " fixture hydrates"));
            assert!((*handle).eq(true).matches(&candidate));
        )+
    };
}

#[cfg(feature = "query")]
macro_rules! assert_integer_handles {
    (
        $value:expr,
        $info:ty,
        $fixture:ident,
        $type:ty,
        $raw:expr,
        $expected:expr,
        [$($field:ident),+ $(,)?]
    ) => {
        $(
            let handle: &IntegerField<$info, $type> = &$value.$field;
            assert_eq!(
                *handle,
                crate::query::__private::integer_field::<$info, $type>(
                    <$info as Filterable>::FILTER_TARGET,
                    stringify!($field),
                )
            );
            let candidate = $fixture(b"tmux 3.7\n", &[(stringify!($field), $raw)])
                .ok()
                .expect(concat!("distinct ", stringify!($field), " fixture hydrates"));
            assert!((*handle).eq($expected).matches(&candidate));
        )+
    };
}

#[cfg(feature = "query")]
macro_rules! assert_enum_handles {
    (
        $value:expr,
        $info:ty,
        $fixture:ident,
        $enum_type:ty,
        $raw:expr,
        $expected:expr,
        [$($field:ident),+ $(,)?]
    ) => {
        $(
            let handle: &EnumField<$info, $enum_type> = &$value.$field;
            assert_eq!(
                *handle,
                crate::query::__private::enum_field::<$info, $enum_type>(
                    <$info as Filterable>::FILTER_TARGET,
                    stringify!($field),
                )
            );
            let candidate = $fixture(b"tmux 3.7\n", &[(stringify!($field), $raw)])
                .ok()
                .expect(concat!("distinct ", stringify!($field), " fixture hydrates"));
            assert!((*handle).eq($expected).matches(&candidate));
        )+
    };
}

#[cfg(feature = "query")]
macro_rules! assert_exact_info_and_fields_shape {
    (
        $info_type:ident,
        $info:expr,
        $fields_type:ident,
        $fields:expr,
        [$($field:ident),+ $(,)?]
    ) => {
        let $info_type { $($field: _,)+ } = &$info;
        let $fields_type { $($field: _,)+ } = &$fields;
    };
}

#[cfg(feature = "query")]
fn assert_snapshot_traits<T: Clone + std::fmt::Debug + Eq + PartialEq>() {}

#[cfg(feature = "query")]
fn assert_progress_traits<T: Clone + Copy + std::fmt::Debug + Eq + PartialEq>() {}

#[allow(
    clippy::match_same_arms,
    clippy::needless_pass_by_value,
    reason = "owned exhaustive matching locks the closed availability shape"
)]
#[cfg(feature = "query")]
fn assert_availability_variants_are_exact<T>(value: Availability<T>) {
    match value {
        Availability::Unsupported => {}
        Availability::Unproven => {}
        Availability::Absent => {}
        Availability::Available(_) => {}
    }
}

#[allow(
    clippy::match_same_arms,
    reason = "exhaustive matching locks the closed progress-state shape"
)]
#[cfg(feature = "query")]
fn assert_progress_variants_are_exact(value: PaneProgressState) {
    match value {
        PaneProgressState::Hidden => {}
        PaneProgressState::Normal => {}
        PaneProgressState::Error => {}
        PaneProgressState::Indeterminate => {}
        PaneProgressState::Paused => {}
    }
}

#[cfg(feature = "query")]
fn generated_fields<T: Filterable>() -> T::Fields {
    T::filter_fields()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the generated 110-field schema is one exhaustive shape contract"
)]
#[cfg(feature = "query")]
#[cfg(feature = "query")]
fn snapshot_catalog_info_and_scalar_handle_shapes_are_exact() {
    assert_snapshot_traits::<Availability<TmuxText>>();
    assert_snapshot_traits::<SessionInfo>();
    assert_snapshot_traits::<WindowInfo>();
    assert_snapshot_traits::<PaneInfo>();
    assert_snapshot_traits::<ClientInfo>();
    assert_progress_traits::<PaneProgressState>();
    assert_not_impl_any!(SessionInfo: Default);
    assert_not_impl_any!(WindowInfo: Default);
    assert_not_impl_any!(PaneInfo: Default);
    assert_not_impl_any!(ClientInfo: Default);
    assert_availability_variants_are_exact::<TmuxText>(Availability::Unsupported);
    assert_progress_variants_are_exact(PaneProgressState::Hidden);
    assert_eq!(
        PaneProgressState::FILTER_VARIANTS,
        ["hidden", "normal", "error", "indeterminate", "paused"]
    );

    let session = session_fixture(b"tmux 3.7\n", &[])
        .ok()
        .expect("complete SessionInfo hydrates");
    let window = window_fixture(b"tmux 3.7\n", &[])
        .ok()
        .expect("complete WindowInfo hydrates");
    let pane = pane_fixture(b"tmux 3.7\n", &[])
        .ok()
        .expect("complete PaneInfo hydrates");
    let client = client_fixture(b"tmux 3.7\n", &[])
        .ok()
        .expect("complete ClientInfo hydrates");

    let _: &SessionId = &session.session_id;
    assert_stored_fields!(session, i64, flat, [session_activity, session_created]);
    assert_stored_fields!(session, i64, evidence, [session_last_attached]);
    assert_stored_fields!(session, u32, flat, [session_attached, session_windows]);
    assert_stored_fields!(session, bool, flat, [session_many_attached]);
    assert_stored_fields!(session, TmuxText, flat, [session_name, session_path]);

    let _: &WindowId = &window.window_id;
    assert_stored_fields!(window, i64, flat, [window_activity]);
    assert_stored_fields!(
        window,
        u32,
        flat,
        [
            window_cell_height,
            window_cell_width,
            window_height,
            window_panes,
            window_width
        ]
    );
    assert_stored_fields!(window, bool, flat, [window_zoomed_flag]);
    assert_stored_fields!(
        window,
        TmuxText,
        flat,
        [window_layout, window_name, window_visible_layout]
    );

    let _: &PaneId = &pane.pane_id;
    assert_stored_fields!(
        pane,
        i32,
        flat,
        [pane_bottom, pane_left, pane_right, pane_top]
    );
    assert_stored_fields!(pane, i32, evidence, [pane_x, pane_y]);
    assert_stored_fields!(pane, u8, evidence, [pane_dead_status, pane_pb_progress]);
    assert_stored_fields!(
        pane,
        u32,
        flat,
        [
            alternate_saved_x,
            alternate_saved_y,
            cursor_x,
            cursor_y,
            history_limit,
            history_size,
            pane_height,
            pane_in_mode,
            pane_index,
            pane_pid,
            pane_width,
            scroll_region_lower,
            scroll_region_upper
        ]
    );
    assert_stored_fields!(pane, u32, evidence, [pane_pipe_pid, pane_z]);
    assert_stored_fields!(pane, u64, flat, [history_bytes]);
    assert_stored_fields!(pane, i64, evidence, [pane_dead_time]);
    assert_stored_fields!(
        pane,
        bool,
        flat,
        [
            cursor_flag,
            insert_flag,
            keypad_cursor_flag,
            keypad_flag,
            mouse_all_flag,
            mouse_any_flag,
            mouse_button_flag,
            mouse_sgr_flag,
            mouse_standard_flag,
            origin_flag,
            pane_active,
            pane_at_bottom,
            pane_at_left,
            pane_at_right,
            pane_at_top,
            pane_dead,
            pane_input_off,
            pane_last,
            pane_marked,
            pane_pipe,
            pane_synchronized,
            wrap_flag
        ]
    );
    assert_stored_fields!(
        pane,
        bool,
        evidence,
        [
            bracket_paste_flag,
            pane_floating_flag,
            pane_unseen_changes,
            pane_zoomed_flag,
            synchronized_output_flag
        ]
    );
    assert_stored_fields!(
        pane,
        TmuxText,
        flat,
        [
            pane_bg,
            pane_fg,
            pane_path,
            pane_search_string,
            pane_start_command,
            pane_tabs,
            pane_title,
            pane_tty
        ]
    );
    assert_stored_fields!(
        pane,
        TmuxText,
        evidence,
        [
            cursor_character,
            pane_current_command,
            pane_current_path,
            pane_dead_signal,
            pane_flags,
            pane_mode,
            pane_start_path
        ]
    );
    assert_stored_fields!(pane, PaneProgressState, evidence, [pane_pb_state]);

    let _: &TmuxText = &client.client_name;
    assert_stored_fields!(client, i64, flat, [client_activity, client_created]);
    assert_stored_fields!(client, u32, flat, [client_pid, client_width]);
    assert_stored_fields!(
        client,
        u32,
        evidence,
        [
            client_cell_height,
            client_cell_width,
            client_height,
            client_uid
        ]
    );
    assert_stored_fields!(client, u64, flat, [client_discarded, client_written]);
    assert_stored_fields!(
        client,
        bool,
        flat,
        [
            client_control_mode,
            client_prefix,
            client_readonly,
            client_utf8
        ]
    );
    assert_stored_fields!(
        client,
        TmuxText,
        flat,
        [
            client_flags,
            client_key_table,
            client_termfeatures,
            client_termname,
            client_termtype,
            client_tty
        ]
    );
    assert_stored_fields!(client, TmuxText, evidence, [client_user]);

    let session_fields: SessionFields<SessionInfo> = generated_fields::<SessionInfo>();
    let window_fields: WindowFields<WindowInfo> = generated_fields::<WindowInfo>();
    let pane_fields: PaneFields<PaneInfo> = generated_fields::<PaneInfo>();
    let client_fields: ClientFields<ClientInfo> = generated_fields::<ClientInfo>();
    assert_eq!(SessionInfo::FILTER_TARGET, "session");
    assert_eq!(WindowInfo::FILTER_TARGET, "window");
    assert_eq!(PaneInfo::FILTER_TARGET, "pane");
    assert_eq!(ClientInfo::FILTER_TARGET, "client");

    assert_exact_info_and_fields_shape!(
        SessionInfo,
        session,
        SessionFields,
        session_fields,
        [
            session_id,
            session_activity,
            session_attached,
            session_created,
            session_last_attached,
            session_many_attached,
            session_name,
            session_path,
            session_windows,
        ]
    );
    assert_exact_info_and_fields_shape!(
        WindowInfo,
        window,
        WindowFields,
        window_fields,
        [
            window_id,
            window_activity,
            window_cell_height,
            window_cell_width,
            window_height,
            window_layout,
            window_name,
            window_panes,
            window_visible_layout,
            window_width,
            window_zoomed_flag,
        ]
    );
    assert_exact_info_and_fields_shape!(
        PaneInfo,
        pane,
        PaneFields,
        pane_fields,
        [
            pane_id,
            alternate_saved_x,
            alternate_saved_y,
            bracket_paste_flag,
            cursor_character,
            cursor_flag,
            cursor_x,
            cursor_y,
            history_bytes,
            history_limit,
            history_size,
            insert_flag,
            keypad_cursor_flag,
            keypad_flag,
            mouse_all_flag,
            mouse_any_flag,
            mouse_button_flag,
            mouse_sgr_flag,
            mouse_standard_flag,
            origin_flag,
            pane_active,
            pane_at_bottom,
            pane_at_left,
            pane_at_right,
            pane_at_top,
            pane_bg,
            pane_bottom,
            pane_current_command,
            pane_current_path,
            pane_dead,
            pane_dead_signal,
            pane_dead_status,
            pane_dead_time,
            pane_fg,
            pane_flags,
            pane_floating_flag,
            pane_height,
            pane_in_mode,
            pane_index,
            pane_input_off,
            pane_last,
            pane_left,
            pane_marked,
            pane_mode,
            pane_path,
            pane_pb_progress,
            pane_pb_state,
            pane_pid,
            pane_pipe,
            pane_pipe_pid,
            pane_right,
            pane_search_string,
            pane_start_command,
            pane_start_path,
            pane_synchronized,
            pane_tabs,
            pane_title,
            pane_top,
            pane_tty,
            pane_unseen_changes,
            pane_width,
            pane_x,
            pane_y,
            pane_z,
            pane_zoomed_flag,
            scroll_region_lower,
            scroll_region_upper,
            synchronized_output_flag,
            wrap_flag,
        ]
    );
    assert_exact_info_and_fields_shape!(
        ClientInfo,
        client,
        ClientFields,
        client_fields,
        [
            client_name,
            client_activity,
            client_cell_height,
            client_cell_width,
            client_control_mode,
            client_created,
            client_discarded,
            client_flags,
            client_height,
            client_key_table,
            client_pid,
            client_prefix,
            client_readonly,
            client_termfeatures,
            client_termname,
            client_termtype,
            client_tty,
            client_uid,
            client_user,
            client_utf8,
            client_width,
            client_written,
        ]
    );

    assert_text_handles!(
        session_fields,
        SessionInfo,
        session_fixture,
        b"$7",
        "$7",
        [session_id]
    );
    assert_text_handles!(
        session_fields,
        SessionInfo,
        session_fixture,
        b"distinct-text",
        "distinct-text",
        [session_name, session_path]
    );
    assert_integer_handles!(
        session_fields,
        SessionInfo,
        session_fixture,
        i64,
        b"-7",
        -7_i64,
        [session_activity, session_created, session_last_attached]
    );
    assert_integer_handles!(
        session_fields,
        SessionInfo,
        session_fixture,
        u32,
        b"7",
        7_u32,
        [session_attached, session_windows]
    );
    assert_bool_handles!(
        session_fields,
        SessionInfo,
        session_fixture,
        [session_many_attached]
    );

    assert_text_handles!(
        window_fields,
        WindowInfo,
        window_fixture,
        b"@7",
        "@7",
        [window_id]
    );
    assert_text_handles!(
        window_fields,
        WindowInfo,
        window_fixture,
        b"distinct-text",
        "distinct-text",
        [window_layout, window_name, window_visible_layout]
    );
    assert_integer_handles!(
        window_fields,
        WindowInfo,
        window_fixture,
        i64,
        b"-7",
        -7_i64,
        [window_activity]
    );
    assert_integer_handles!(
        window_fields,
        WindowInfo,
        window_fixture,
        u32,
        b"7",
        7_u32,
        [
            window_cell_height,
            window_cell_width,
            window_height,
            window_panes,
            window_width,
        ]
    );
    assert_bool_handles!(
        window_fields,
        WindowInfo,
        window_fixture,
        [window_zoomed_flag]
    );

    assert_text_handles!(pane_fields, PaneInfo, pane_fixture, b"%7", "%7", [pane_id]);
    assert_text_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        b"distinct-text",
        "distinct-text",
        [
            cursor_character,
            pane_bg,
            pane_current_command,
            pane_current_path,
            pane_dead_signal,
            pane_fg,
            pane_flags,
            pane_mode,
            pane_path,
            pane_search_string,
            pane_start_command,
            pane_start_path,
            pane_tabs,
            pane_title,
            pane_tty,
        ]
    );
    assert_integer_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        i32,
        b"-7",
        -7_i32,
        [pane_bottom, pane_left, pane_right, pane_top, pane_x, pane_y]
    );
    assert_integer_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        u8,
        b"7",
        7_u8,
        [pane_dead_status, pane_pb_progress]
    );
    assert_integer_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        u32,
        b"7",
        7_u32,
        [
            alternate_saved_x,
            alternate_saved_y,
            cursor_x,
            cursor_y,
            history_limit,
            history_size,
            pane_height,
            pane_in_mode,
            pane_index,
            pane_pid,
            pane_pipe_pid,
            pane_width,
            pane_z,
            scroll_region_lower,
            scroll_region_upper,
        ]
    );
    assert_integer_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        u64,
        b"7",
        7_u64,
        [history_bytes]
    );
    assert_integer_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        i64,
        b"-7",
        -7_i64,
        [pane_dead_time]
    );
    assert_bool_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        [
            bracket_paste_flag,
            cursor_flag,
            insert_flag,
            keypad_cursor_flag,
            keypad_flag,
            mouse_all_flag,
            mouse_any_flag,
            mouse_button_flag,
            mouse_sgr_flag,
            mouse_standard_flag,
            origin_flag,
            pane_active,
            pane_at_bottom,
            pane_at_left,
            pane_at_right,
            pane_at_top,
            pane_dead,
            pane_floating_flag,
            pane_input_off,
            pane_last,
            pane_marked,
            pane_pipe,
            pane_synchronized,
            pane_unseen_changes,
            pane_zoomed_flag,
            synchronized_output_flag,
            wrap_flag,
        ]
    );
    assert_enum_handles!(
        pane_fields,
        PaneInfo,
        pane_fixture,
        PaneProgressState,
        b"paused",
        PaneProgressState::Paused,
        [pane_pb_state]
    );

    assert_text_handles!(
        client_fields,
        ClientInfo,
        client_fixture,
        b"distinct-text",
        "distinct-text",
        [
            client_name,
            client_flags,
            client_key_table,
            client_termfeatures,
            client_termname,
            client_termtype,
            client_tty,
            client_user,
        ]
    );
    assert_integer_handles!(
        client_fields,
        ClientInfo,
        client_fixture,
        i64,
        b"-7",
        -7_i64,
        [client_activity, client_created]
    );
    assert_integer_handles!(
        client_fields,
        ClientInfo,
        client_fixture,
        u32,
        b"7",
        7_u32,
        [
            client_cell_height,
            client_cell_width,
            client_height,
            client_pid,
            client_uid,
            client_width,
        ]
    );
    assert_integer_handles!(
        client_fields,
        ClientInfo,
        client_fixture,
        u64,
        b"7",
        7_u64,
        [client_discarded, client_written]
    );
    assert_bool_handles!(
        client_fields,
        ClientInfo,
        client_fixture,
        [
            client_control_mode,
            client_prefix,
            client_readonly,
            client_utf8,
        ]
    );
}

#[cfg(feature = "query")]
fn assert_available<T: std::fmt::Debug + PartialEq>(value: &Availability<T>, expected: T) {
    assert_eq!(value, &Availability::Available(expected));
}

/// The same check for a field stored flat, which carries no evidence.
fn assert_stored<T: std::fmt::Debug + PartialEq>(value: &T, expected: &T) {
    assert_eq!(value, expected);
}

fn assert_pane_invalid(field: &'static str, input: &[u8], expected: DecoderKind) {
    let plan = FormatPlan::for_profile(ListProfile::Panes, &version(b"tmux 3.7\n"));
    let (row, offsets) = parsed_row(&plan, &[(field, input)]);
    let error = hydrate_pane_info(&plan, &row)
        .err()
        .expect("invalid typed Pane field is rejected");
    assert_invalid_value(&error, &plan, &offsets, field, expected);
}

fn assert_identity_invalid(profile: ListProfile, field: &'static str, input: &[u8]) {
    let plan = FormatPlan::for_profile(profile, &version(b"tmux 3.7\n"));
    let (row, offsets) = parsed_row(&plan, &[(field, input)]);
    let result = match profile {
        ListProfile::Sessions => hydrate_session_info(&plan, &row).map(|_| ()),
        ListProfile::Windows => hydrate_window_info(&plan, &row).map(|_| ()),
        ListProfile::Panes => hydrate_pane_info(&plan, &row).map(|_| ()),
        ListProfile::Clients => hydrate_client_info(&plan, &row).map(|_| ()),
    };
    let expected = match profile {
        ListProfile::Sessions => DecoderKind::SessionId,
        ListProfile::Windows => DecoderKind::WindowId,
        ListProfile::Panes => DecoderKind::PaneId,
        ListProfile::Clients => DecoderKind::Text,
    };
    let error = result.err().expect("invalid identity is rejected");
    assert_invalid_value(&error, &plan, &offsets, field, expected);
}

#[test]
fn snapshot_catalog_empty_policy_distinguishes_all_three_states() {
    let optional_numeric = pane_fixture(b"tmux 3.7\n", &[("pane_pipe_pid", b"")])
        .ok()
        .expect("empty optional numeric hydrates");
    assert_eq!(optional_numeric.pane_pipe_pid, Availability::Absent);

    let optional_text = pane_fixture(b"tmux 3.7\n", &[("pane_mode", b"")])
        .ok()
        .expect("empty optional text hydrates");
    assert_eq!(optional_text.pane_mode, Availability::Absent);

    let available_text = pane_fixture(b"tmux 3.7\n", &[("pane_path", b"")])
        .ok()
        .expect("empty available text hydrates");
    assert_stored(
        &available_text.pane_path,
        &TmuxText::from_bytes(Vec::<u8>::new()),
    );

    let unsupported = pane_fixture(b"tmux 3.6\n", &[])
        .ok()
        .expect("numbered older snapshot hydrates");
    assert_eq!(unsupported.pane_pipe_pid, Availability::Unsupported);

    let unproven = pane_fixture(b"tmux master\n", &[])
        .ok()
        .expect("development snapshot hydrates");
    assert_eq!(unproven.pane_pipe_pid, Availability::Unproven);

    let plan = FormatPlan::for_profile(ListProfile::Panes, &version(b"tmux 3.7\n"));
    let (row, offsets) = parsed_row(&plan, &[("pane_width", b"")]);
    let error = hydrate_pane_info(&plan, &row)
        .err()
        .expect("empty required numeric is rejected");
    let field = plan
        .descriptors_for_test()
        .iter()
        .position(|descriptor| descriptor.name() == "pane_width")
        .expect("pane_width is selected");
    assert_eq!(error.kind(), FormatCodecErrorKind::RequiredFieldEmpty);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), Some(field));
    assert_eq!(error.field_name(), Some("pane_width"));
    assert_eq!(error.expected(), Some(DecoderKind::U32));
    assert_eq!(error.offset(), offsets.get("pane_width").copied());
    assert_eq!(error.profile(), None);
    assert_safe_diagnostic(&error);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "typed decoder boundaries are one exhaustive grammar contract"
)]
#[cfg(feature = "query")]
fn snapshot_catalog_typed_decoders_accept_exact_boundaries() {
    for (input, expected) in [(b"0".as_slice(), false), (b"1".as_slice(), true)] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("cursor_flag", input)])
            .ok()
            .expect("canonical Bool hydrates");
        assert_stored(&pane.cursor_flag, &expected);
    }

    for (input, expected) in [(b"0".as_slice(), 0_u8), (b"255".as_slice(), u8::MAX)] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("pane_dead_status", input)])
            .ok()
            .expect("bounded u8 hydrates");
        assert_available(&pane.pane_dead_status, expected);
    }

    for (input, expected) in [
        (b"0".as_slice(), 0_u32),
        (b"4294967295".as_slice(), u32::MAX),
    ] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("pane_width", input)])
            .ok()
            .expect("bounded u32 hydrates");
        assert_stored(&pane.pane_width, &expected);
    }

    for (input, expected) in [
        (b"0".as_slice(), 0_u64),
        (b"18446744073709551615".as_slice(), u64::MAX),
    ] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("history_bytes", input)])
            .ok()
            .expect("bounded u64 hydrates");
        assert_stored(&pane.history_bytes, &expected);
    }

    for (input, expected) in [
        (b"-2147483648".as_slice(), i32::MIN),
        (b"-1".as_slice(), -1_i32),
        (b"0".as_slice(), 0_i32),
        (b"2147483647".as_slice(), i32::MAX),
    ] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("pane_left", input)])
            .ok()
            .expect("bounded i32 hydrates");
        assert_stored(&pane.pane_left, &expected);
    }

    for (input, expected) in [
        (b"-9223372036854775808".as_slice(), i64::MIN),
        (b"-1".as_slice(), -1_i64),
        (b"0".as_slice(), 0_i64),
        (b"9223372036854775807".as_slice(), i64::MAX),
    ] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("pane_dead_time", input)])
            .ok()
            .expect("bounded timestamp hydrates");
        assert_available(&pane.pane_dead_time, expected);
    }

    for (input, expected) in [(b"0".as_slice(), 0_u8), (b"100".as_slice(), 100_u8)] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("pane_pb_progress", input)])
            .ok()
            .expect("bounded pane progress hydrates");
        assert_available(&pane.pane_pb_progress, expected);
    }

    for (input, expected) in [
        (b"hidden".as_slice(), PaneProgressState::Hidden),
        (b"normal".as_slice(), PaneProgressState::Normal),
        (b"error".as_slice(), PaneProgressState::Error),
        (
            b"indeterminate".as_slice(),
            PaneProgressState::Indeterminate,
        ),
        (b"paused".as_slice(), PaneProgressState::Paused),
    ] {
        let pane = pane_fixture(b"tmux 3.7\n", &[("pane_pb_state", input)])
            .ok()
            .expect("known pane progress state hydrates");
        assert_available(&pane.pane_pb_state, expected);
        assert_eq!(
            expected.filter_name().as_bytes(),
            input,
            "filter spelling equals decoder spelling"
        );
    }

    let sessions = session_fixture(b"tmux 3.7\n", &[("session_id", b"$001")])
        .ok()
        .expect("SessionId accepts leading zeroes");
    let windows = window_fixture(b"tmux 3.7\n", &[("window_id", b"@001")])
        .ok()
        .expect("WindowId accepts leading zeroes");
    let panes = pane_fixture(b"tmux 3.7\n", &[("pane_id", b"%001")])
        .ok()
        .expect("PaneId accepts leading zeroes");
    assert_eq!(sessions.session_id.as_ref(), "$1");
    assert_eq!(windows.window_id.as_ref(), "@1");
    assert_eq!(panes.pane_id.as_ref(), "%1");

    let sessions = session_fixture(b"tmux 3.7\n", &[("session_id", b"$4294967295")])
        .ok()
        .expect("SessionId accepts u32 maximum");
    let windows = window_fixture(b"tmux 3.7\n", &[("window_id", b"@0")])
        .ok()
        .expect("WindowId accepts zero");
    let panes = pane_fixture(b"tmux 3.7\n", &[("pane_id", b"%0")])
        .ok()
        .expect("PaneId accepts zero");
    assert_eq!(sessions.session_id.as_ref(), "$4294967295");
    assert_eq!(windows.window_id.as_ref(), "@0");
    assert_eq!(panes.pane_id.as_ref(), "%0");
}

#[test]
fn snapshot_catalog_typed_decoders_reject_noncanonical_or_overflowing_values() {
    for input in [
        b"2".as_slice(),
        b"00",
        b"-1",
        b"+1",
        b" 1",
        b"1 ",
        b"\xc3\xa9",
    ] {
        assert_pane_invalid("cursor_flag", input, DecoderKind::Bool);
    }
    for input in [b"256".as_slice(), b"-1", b"01", b"+1", b" 1", b"\xc3\xa9"] {
        assert_pane_invalid("pane_dead_status", input, DecoderKind::U8);
    }
    for input in [
        b"4294967296".as_slice(),
        b"-1",
        b"01",
        b"+1",
        b"1 ",
        b"\xc3\xa9",
    ] {
        assert_pane_invalid("pane_width", input, DecoderKind::U32);
    }
    for input in [
        b"18446744073709551616".as_slice(),
        b"-1",
        b"01",
        b"+1",
        b"\t1",
        b"\xc3\xa9",
    ] {
        assert_pane_invalid("history_bytes", input, DecoderKind::U64);
    }
    for input in [
        b"-2147483649".as_slice(),
        b"2147483648",
        b"-0",
        b"-01",
        b"01",
        b"+1",
        b" 1",
        b"\xc3\xa9",
    ] {
        assert_pane_invalid("pane_left", input, DecoderKind::I32);
    }
    for input in [
        b"-9223372036854775809".as_slice(),
        b"9223372036854775808",
        b"-0",
        b"-01",
        b"01",
        b"+1",
        b"1\n",
        b"\xc3\xa9",
    ] {
        assert_pane_invalid("pane_dead_time", input, DecoderKind::Timestamp);
    }
    for input in [
        b"101".as_slice(),
        b"256",
        b"-1",
        b"01",
        b"+1",
        b" 1",
        b"1 ",
        b"\xc3\xa9",
    ] {
        assert_pane_invalid("pane_pb_progress", input, DecoderKind::PaneProgress);
    }
    for input in [b"Hidden".as_slice(), b"NORMAL", b"unknown", b"\xc3\xa9"] {
        assert_pane_invalid("pane_pb_state", input, DecoderKind::PaneProgressState);
    }

    for (profile, field, invalid) in [
        (ListProfile::Sessions, "session_id", b"$".as_slice()),
        (ListProfile::Sessions, "session_id", b"$x"),
        (ListProfile::Sessions, "session_id", b"@1"),
        (ListProfile::Sessions, "session_id", b"$4294967296"),
        (ListProfile::Sessions, "session_id", b"$\xc3\xa9"),
        (ListProfile::Windows, "window_id", b"@"),
        (ListProfile::Windows, "window_id", b"@x"),
        (ListProfile::Windows, "window_id", b"$1"),
        (ListProfile::Windows, "window_id", b"@4294967296"),
        (ListProfile::Windows, "window_id", b"@\xc3\xa9"),
        (ListProfile::Panes, "pane_id", b"%"),
        (ListProfile::Panes, "pane_id", b"%x"),
        (ListProfile::Panes, "pane_id", b"@1"),
        (ListProfile::Panes, "pane_id", b"%4294967296"),
        (ListProfile::Panes, "pane_id", b"%\xc3\xa9"),
    ] {
        assert_identity_invalid(profile, field, invalid);
    }
}

#[test]
fn snapshot_catalog_typed_error_metadata_is_payload_free() {
    let mut payload = Vec::new();
    payload.extend_from_slice(SHORT_SENTINEL.as_bytes());
    payload.push(b'_');
    payload.extend_from_slice(LONG_SENTINEL.as_bytes());
    payload.push(b'_');
    payload.extend_from_slice(&CONTROL_SENTINEL);
    payload.push(b'_');
    payload.extend_from_slice(&INVALID_UTF8_SENTINEL);

    let plan = FormatPlan::for_profile(ListProfile::Panes, &version(b"tmux 3.7\n"));
    let (row, offsets) = parsed_row(&plan, &[("pane_width", &payload)]);
    let error = hydrate_pane_info(&plan, &row)
        .err()
        .expect("sensitive typed payload is invalid");
    assert_invalid_value(&error, &plan, &offsets, "pane_width", DecoderKind::U32);
    assert!(!std::mem::needs_drop::<FormatCodecError>());
}

#[test]
fn snapshot_catalog_plan_row_mismatch_coordinates_and_precedence_are_local() {
    let forward = plan(vec![&FIRST_TEXT, &SECOND_ASCII]);
    let reverse = plan(vec![&SECOND_ASCII, &FIRST_TEXT]);
    let (row, _) = parsed_row(&forward, &[("first", b"left"), ("second", b"right")]);
    let error = validate_planned_row_for_test(&reverse, &row)
        .err()
        .expect("reversed descriptors are rejected");
    assert_eq!(error.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), Some(0));
    assert_eq!(error.field_name(), Some("second"));
    assert_eq!(error.expected(), None);
    assert_eq!(error.offset(), Some(0));
    assert_eq!(error.profile(), None);
    assert_safe_diagnostic(&error);

    let short_plan = plan(vec![&FIRST_TEXT]);
    let long_plan = plan(vec![&FIRST_TEXT, &SECOND_ASCII]);
    let (short_row, _) = parsed_row(&short_plan, &[("first", b"x")]);
    let underflow = validate_planned_row_for_test(&long_plan, &short_row)
        .err()
        .expect("shared-prefix underflow is rejected");
    assert_eq!(underflow.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(underflow.phase(), FormatCodecPhase::Decode);
    assert_eq!(underflow.row(), Some(0));
    assert_eq!(underflow.field(), Some(1));
    assert_eq!(underflow.field_name(), Some("second"));
    assert_eq!(underflow.expected(), None);
    assert_eq!(underflow.offset(), None);
    assert_eq!(underflow.profile(), None);
    assert_safe_diagnostic(&underflow);

    let (long_row, offsets) = parsed_row(&long_plan, &[("first", b"x"), ("second", b"y")]);
    let overflow = validate_planned_row_for_test(&short_plan, &long_row)
        .err()
        .expect("trailing actual slot is rejected");
    assert_eq!(overflow.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(overflow.phase(), FormatCodecPhase::Decode);
    assert_eq!(overflow.row(), Some(0));
    assert_eq!(overflow.field(), Some(1));
    assert_eq!(overflow.field_name(), Some("second"));
    assert_eq!(overflow.expected(), None);
    assert_eq!(overflow.offset(), offsets.get("second").copied());
    assert_eq!(overflow.profile(), None);
    assert_safe_diagnostic(&overflow);

    let source = FormatPlan::for_descriptors(
        ListProfile::Sessions,
        &version(b"tmux 3.7\n"),
        &[session_descriptor("session_activity")],
    )
    .ok()
    .expect("source projection is in scope");
    let mismatched = FormatPlan::for_descriptors(
        ListProfile::Sessions,
        &version(b"tmux 3.7\n"),
        &[session_descriptor("session_created")],
    )
    .ok()
    .expect("mismatched projection is in scope");
    let (row, offsets) = parsed_row(&source, &[("session_id", b"invalid")]);
    let invalid_first = validate_planned_row_for_test(&mismatched, &row)
        .err()
        .expect("earlier invalid typed field wins");
    assert_invalid_value(
        &invalid_first,
        &mismatched,
        &offsets,
        "session_id",
        DecoderKind::SessionId,
    );
}

#[test]
fn snapshot_catalog_intrinsic_hydration_stops_first_and_reaches_last() {
    let plan = FormatPlan::for_profile(ListProfile::Panes, &version(b"tmux 3.7\n"));
    let (row, offsets) = parsed_row(&plan, &[("cursor_flag", b"2"), ("pane_width", b"invalid")]);
    let error = hydrate_pane_info(&plan, &row)
        .err()
        .expect("the earlier invalid selected slot is returned");
    assert_invalid_value(&error, &plan, &offsets, "cursor_flag", DecoderKind::Bool);

    assert_eq!(
        plan.descriptors_for_test()
            .last()
            .map(|descriptor| descriptor.name()),
        Some("wrap_flag")
    );
    let (row, _) = parsed_row(&plan, &[("wrap_flag", b"1")]);
    let pane = hydrate_pane_info(&plan, &row)
        .ok()
        .expect("the final selected slot is hydrated");
    assert_stored(&pane.wrap_flag, &true);
}

fn session_descriptor(name: &str) -> &'static FormatDescriptor {
    crate::formats::SESSION_INFO_DESCRIPTORS
        .iter()
        .copied()
        .find(|descriptor| descriptor.name() == name)
        .expect("SessionInfo descriptor exists")
}

#[test]
fn snapshot_catalog_intrinsic_hydration_rejects_projection_or_wrong_placement() {
    let version = version(b"tmux 3.7\n");
    let projection = FormatPlan::for_descriptors(
        ListProfile::Sessions,
        &version,
        crate::formats::SESSION_INFO_SUPPLEMENTS,
    )
    .ok()
    .expect("complete SessionInfo descriptor projection is valid");
    let (row, _) = parsed_row(&projection, &[]);
    let error = hydrate_session_info(&projection, &row)
        .err()
        .expect("projection cannot manufacture intrinsic Info");
    assert_eq!(error.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), None);
    assert_eq!(error.field_name(), None);
    assert_eq!(error.expected(), None);
    assert_eq!(error.offset(), None);
    assert_eq!(error.profile(), Some(ListProfile::Sessions));
    assert_safe_diagnostic(&error);

    let windows = FormatPlan::for_profile(ListProfile::Windows, &version);
    let (row, _) = parsed_row(&windows, &[]);
    let error = hydrate_session_info(&windows, &row)
        .err()
        .expect("wrong intrinsic placement is rejected");
    assert_eq!(error.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), None);
    assert_eq!(error.field_name(), None);
    assert_eq!(error.expected(), None);
    assert_eq!(error.offset(), None);
    assert_eq!(error.profile(), Some(ListProfile::Windows));
    assert_safe_diagnostic(&error);
}

#[test]
fn snapshot_catalog_comma_grammars_remain_one_opaque_text_value() {
    let projection = FormatPlan::for_descriptors(
        ListProfile::Sessions,
        &version(b"tmux 3.7\n"),
        &[&SESSION_GROUP_LIST],
    )
    .ok()
    .expect("session group list is admitted by Session listings");
    let expected_groups = b"alpha,beta,gamma";
    let (row, _) = parsed_row(&projection, &[("session_group_list", expected_groups)]);
    let slot = row.slots().nth(1).expect("projection has one supplement");
    assert_eq!(slot.descriptor().name(), "session_group_list");
    assert_eq!(decode_text(slot).as_bytes(), expected_groups);

    let expected_tabs = b"1,4,8,16";
    let pane = pane_fixture(b"tmux 3.7\n", &[("pane_tabs", expected_tabs)])
        .ok()
        .expect("numeric comma text hydrates");
    // pane_tabs is stored flat, so there is no state to match on: the
    // hydration that produced this snapshot already proved it present.
    assert_eq!(pane.pane_tabs.as_bytes(), expected_tabs);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "all unavailable states and scalar operators form one contract"
)]
#[cfg(feature = "query")]
fn snapshot_catalog_scalar_handles_match_only_available_values() {
    let available = pane_fixture(
        b"tmux 3.7\n",
        &[
            ("cursor_flag", b"1"),
            ("history_bytes", b"9"),
            ("pane_dead_status", b"7"),
            ("pane_dead_time", b"-2"),
            ("pane_left", b"-3"),
            ("pane_path", b"/tmp/pane"),
            ("pane_pb_state", b"paused"),
            ("pane_pipe_pid", b"42"),
            ("pane_width", b"80"),
        ],
    )
    .ok()
    .expect("available scalar fixture hydrates");
    let absent = pane_fixture(b"tmux 3.7\n", &[("pane_pipe_pid", b"")])
        .ok()
        .expect("absent scalar fixture hydrates");
    let unsupported = pane_fixture(b"tmux 3.6\n", &[])
        .ok()
        .expect("unsupported scalar fixture hydrates");
    let unproven = pane_fixture(b"tmux master\n", &[])
        .ok()
        .expect("unproven scalar fixture hydrates");

    assert!(
        PaneInfo::filter_fields()
            .pane_id
            .eq("%1")
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .pane_path
            .eq("/tmp/pane")
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .cursor_flag
            .eq(true)
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .pane_dead_status
            .eq(7)
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .pane_width
            .eq(80)
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .history_bytes
            .eq(9)
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .pane_left
            .eq(-3)
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .pane_dead_time
            .eq(-2)
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .pane_pb_state
            .eq(PaneProgressState::Paused)
            .matches(&available)
    );
    assert!(
        PaneInfo::filter_fields()
            .pane_pipe_pid
            .eq(42)
            .matches(&available)
    );

    for candidate in [&absent, &unsupported, &unproven] {
        assert!(
            !PaneInfo::filter_fields()
                .pane_pipe_pid
                .eq(42)
                .matches(candidate)
        );
        assert!(
            !PaneInfo::filter_fields()
                .pane_pipe_pid
                .is_in([42])
                .matches(candidate)
        );
        assert!(
            !PaneInfo::filter_fields()
                .pane_pipe_pid
                .not_in([])
                .matches(candidate)
        );
    }

    let absent_text = pane_fixture(b"tmux 3.7\n", &[("pane_mode", b"")])
        .ok()
        .expect("absent text fixture hydrates");
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .eq("copy")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .eq_ignore_case("copy")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .contains("op")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .contains_ignore_case("op")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .starts_with("co")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .starts_with_ignore_case("co")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .ends_with("py")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .ends_with_ignore_case("py")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .is_in(["copy"])
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .not_in(std::iter::empty::<&str>())
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .regex("^copy$")
            .ok()
            .expect("regex is valid")
            .matches(&absent_text)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_mode
            .regex_ignore_case("^copy$")
            .ok()
            .expect("regex is valid")
            .matches(&absent_text)
    );

    assert!(
        !PaneInfo::filter_fields()
            .bracket_paste_flag
            .not_in([])
            .matches(&unsupported)
    );
    assert!(
        !PaneInfo::filter_fields()
            .pane_pb_state
            .not_in(std::iter::empty::<PaneProgressState>())
            .matches(&unsupported)
    );
}

#[cfg(feature = "query")]
#[test]
fn snapshot_catalog_strict_text_matching_and_debug_keep_bytes_private() {
    let invalid = [0xff, 0xfe];
    let pane = pane_fixture(b"tmux 3.7\n", &[("pane_path", &invalid)])
        .ok()
        .expect("non-UTF-8 text hydrates losslessly");
    // Stored flat, so hydration already proved it present: the assertion
    // is about the bytes surviving, not about which state it is in.
    assert_eq!(pane.pane_path.as_bytes(), invalid);
    assert!(
        !PaneInfo::filter_fields()
            .pane_path
            .eq("text")
            .matches(&pane)
    );

    let session = session_fixture(
        b"tmux 3.7\n",
        &[("session_name", SHORT_SENTINEL.as_bytes())],
    )
    .ok()
    .expect("SessionInfo sentinel hydrates");
    let window = window_fixture(b"tmux 3.7\n", &[("window_name", SHORT_SENTINEL.as_bytes())])
        .ok()
        .expect("WindowInfo sentinel hydrates");
    let pane = pane_fixture(b"tmux 3.7\n", &[("pane_title", SHORT_SENTINEL.as_bytes())])
        .ok()
        .expect("PaneInfo sentinel hydrates");
    let client = client_fixture(b"tmux 3.7\n", &[("client_name", SHORT_SENTINEL.as_bytes())])
        .ok()
        .expect("ClientInfo sentinel hydrates");
    let nested = Availability::Available(TmuxText::from(SHORT_SENTINEL));

    for debug in [
        format!("{session:?}"),
        format!("{window:?}"),
        format!("{pane:?}"),
        format!("{client:?}"),
        format!("{nested:?}"),
    ] {
        assert!(!debug.contains(SHORT_SENTINEL));
        assert!(!debug.contains(LONG_SENTINEL));
        assert!(!debug.contains('\u{fffd}'));
    }
}

#[cfg(feature = "serde")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "authored field methods produce owned expressions for serialization"
)]
#[cfg(feature = "query")]
fn assert_serde_scalar<T>(
    authored: FilterExpr<T>,
    wire: serde_json::Value,
    wrong_family: serde_json::Value,
    candidate: &T,
) where
    T: Filterable,
    FilterExpr<T>: serde::Serialize + serde::de::DeserializeOwned,
{
    assert_eq!(
        serde_json::to_value(&authored)
            .ok()
            .expect("authored predicate serializes"),
        wire
    );
    let decoded = serde_json::from_value::<FilterExpr<T>>(wire)
        .ok()
        .expect("canonical predicate validates for generated family");
    assert!(decoded.matches(candidate));
    assert!(serde_json::from_value::<FilterExpr<T>>(wrong_family).is_err());
}

#[cfg(feature = "query")]
#[cfg(feature = "serde")]
#[test]
fn snapshot_catalog_serde_dispatch_covers_every_generated_scalar_family() {
    use serde_json::json;

    let pane = pane_fixture(
        b"tmux 3.7\n",
        &[
            ("cursor_flag", b"1"),
            ("history_bytes", b"9"),
            ("pane_dead_status", b"7"),
            ("pane_dead_time", b"-2"),
            ("pane_left", b"-3"),
            ("pane_path", b"/tmp/pane"),
            ("pane_pb_state", b"paused"),
            ("pane_width", b"80"),
        ],
    )
    .ok()
    .expect("serde candidate hydrates");

    let envelope = |field: &str, value: serde_json::Value| {
        json!({
            "version": 1,
            "target": "pane",
            "expr": {"op": "eq", "field": field, "value": value}
        })
    };
    assert_serde_scalar(
        PaneInfo::filter_fields().pane_path.eq("/tmp/pane"),
        envelope("pane_path", json!("/tmp/pane")),
        envelope("pane_path", json!(true)),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields().cursor_flag.eq(true),
        envelope("cursor_flag", json!(true)),
        envelope("cursor_flag", json!("true")),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields().pane_dead_status.eq(7),
        envelope("pane_dead_status", json!("7")),
        envelope("pane_dead_status", json!(true)),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields().pane_width.eq(80),
        envelope("pane_width", json!("80")),
        envelope("pane_width", json!(true)),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields().history_bytes.eq(9),
        envelope("history_bytes", json!("9")),
        envelope("history_bytes", json!(true)),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields().pane_left.eq(-3),
        envelope("pane_left", json!("-3")),
        envelope("pane_left", json!(true)),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields().pane_dead_time.eq(-2),
        envelope("pane_dead_time", json!("-2")),
        envelope("pane_dead_time", json!(true)),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields().pane_id.eq("%1"),
        envelope("pane_id", json!("%1")),
        envelope("pane_id", json!(true)),
        &pane,
    );
    assert_serde_scalar(
        PaneInfo::filter_fields()
            .pane_pb_state
            .eq(PaneProgressState::Paused),
        envelope("pane_pb_state", json!("paused")),
        envelope("pane_pb_state", json!(true)),
        &pane,
    );
}

fn projection_rows(plan: &FormatPlan, row_overrides: &[Vec<(&str, &[u8])>]) -> Vec<ParsedRow> {
    let mut stdout = Vec::new();
    for overrides in row_overrides {
        let (row, _) = framed_row(plan, overrides);
        stdout.extend_from_slice(&row);
    }
    plan.parse_rows(&stdout)
        .ok()
        .expect("projection fixtures use the production framing parser")
}

fn projection_server_identity(endpoint: &str) -> ServerIdentity {
    ServerIdentity::from_socket_path(std::path::PathBuf::from(endpoint))
}

fn window_projection_fixture(
    raw_version: &[u8],
    endpoint: &str,
    overrides: &[(&str, &[u8])],
) -> WindowProjection {
    let plan = window_projection_plan(&version(raw_version))
        .ok()
        .expect("Window projection plan is valid");
    let (row, _) = parsed_row(&plan, overrides);
    hydrate_window_projection(&projection_server_identity(endpoint), &plan, &row)
        .ok()
        .expect("Window projection fixture hydrates")
}

fn pane_projection_fixture(
    raw_version: &[u8],
    endpoint: &str,
    overrides: &[(&str, &[u8])],
) -> PaneProjection {
    let plan = pane_projection_plan(&version(raw_version))
        .ok()
        .expect("Pane projection plan is valid");
    let (row, _) = parsed_row(&plan, overrides);
    hydrate_pane_projection(&projection_server_identity(endpoint), &plan, &row)
        .ok()
        .expect("Pane projection fixture hydrates")
}

fn window_projection_suffix() -> [&'static FormatDescriptor; 20] {
    [
        &SESSION_ID,
        &WINDOW_INDEX,
        &WINDOW_ACTIVE,
        &WINDOW_ACTIVITY_FLAG,
        &WINDOW_BELL_FLAG,
        &WINDOW_END_FLAG,
        &WINDOW_FLAGS,
        &WINDOW_LAST_FLAG,
        &WINDOW_LINKED,
        &WINDOW_MARKED_FLAG,
        &WINDOW_RAW_FLAGS,
        &WINDOW_SILENCE_FLAG,
        &WINDOW_STACK_INDEX,
        &WINDOW_START_FLAG,
        &WINDOW_ACTIVE_CLIENTS,
        &WINDOW_ACTIVE_CLIENTS_LIST,
        &WINDOW_ACTIVE_SESSIONS,
        &WINDOW_ACTIVE_SESSIONS_LIST,
        &WINDOW_LINKED_SESSIONS,
        &WINDOW_LINKED_SESSIONS_LIST,
    ]
}

fn pane_projection_suffix() -> [&'static FormatDescriptor; 3] {
    [&SESSION_ID, &WINDOW_ID, &WINDOW_INDEX]
}

fn window_projection_requested() -> Vec<&'static FormatDescriptor> {
    WINDOW_INFO_SUPPLEMENTS
        .iter()
        .copied()
        .chain(window_projection_suffix())
        .collect()
}

fn pane_projection_requested() -> Vec<&'static FormatDescriptor> {
    PANE_INFO_SUPPLEMENTS
        .iter()
        .copied()
        .chain(pane_projection_suffix())
        .collect()
}

fn assert_descriptor_sequence(
    actual: &[&'static FormatDescriptor],
    expected: &[&'static FormatDescriptor],
) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.name(), expected.name());
        assert!(std::ptr::eq(*actual, *expected));
    }
}

fn descriptor_template(descriptors: &[&'static FormatDescriptor]) -> String {
    use std::fmt::Write as _;

    let mut template = String::new();
    for descriptor in descriptors {
        write!(&mut template, "#{{q:{}}}%", descriptor.name())
            .expect("writing to a String cannot fail");
    }
    template
}

const fn const_window_link_identity(link: &WindowLink) -> &WindowLinkIdentity {
    link.identity()
}

const fn const_window_projection_window(projection: &WindowProjection) -> &WindowInfo {
    projection.window()
}

const fn const_window_projection_link(projection: &WindowProjection) -> &WindowLink {
    projection.link()
}

const fn const_pane_projection_pane(projection: &PaneProjection) -> &PaneInfo {
    projection.pane()
}

const fn const_pane_projection_link_identity(projection: &PaneProjection) -> &WindowLinkIdentity {
    projection.link_identity()
}

#[test]
#[allow(
    clippy::too_many_lines,
    clippy::type_complexity,
    reason = "one exhaustive contract locks projection values and private function signatures"
)]
fn snapshot_projection_value_shapes_accessors_and_traits_are_exact() {
    let window = window_projection_fixture(
        b"tmux 3.7\n",
        "/private/projection-shape-endpoint",
        &[("session_id", b"$7"), ("window_index", b"-3")],
    );
    let pane = pane_projection_fixture(
        b"tmux 3.7\n",
        "/private/projection-shape-endpoint",
        &[
            ("session_id", b"$7"),
            ("window_id", b"@1"),
            ("window_index", b"-3"),
        ],
    );

    assert_impl_all!(WindowLink: Clone, std::fmt::Debug, Eq, PartialEq);
    assert_impl_all!(WindowProjection: Clone, std::fmt::Debug, Eq, PartialEq);
    assert_impl_all!(PaneProjection: Clone, std::fmt::Debug, Eq, PartialEq);
    assert_not_impl_any!(WindowLink: std::hash::Hash);
    assert_not_impl_any!(WindowProjection: std::hash::Hash);
    assert_not_impl_any!(PaneProjection: std::hash::Hash);

    let identity_accessor: for<'a> fn(&'a WindowLink) -> &'a WindowLinkIdentity =
        WindowLink::identity;
    let window_accessor: for<'a> fn(&'a WindowProjection) -> &'a WindowInfo =
        WindowProjection::window;
    let link_accessor: for<'a> fn(&'a WindowProjection) -> &'a WindowLink = WindowProjection::link;
    let pane_accessor: for<'a> fn(&'a PaneProjection) -> &'a PaneInfo = PaneProjection::pane;
    let pane_link_accessor: for<'a> fn(&'a PaneProjection) -> &'a WindowLinkIdentity =
        PaneProjection::link_identity;
    let window_plan: fn(&TmuxVersion) -> Result<FormatPlan, FormatCodecError> =
        window_projection_plan;
    let pane_plan: fn(&TmuxVersion) -> Result<FormatPlan, FormatCodecError> = pane_projection_plan;
    let hydrate_window: fn(
        &ServerIdentity,
        &FormatPlan,
        &ParsedRow,
    ) -> Result<WindowProjection, FormatCodecError> = hydrate_window_projection;
    let hydrate_windows: fn(
        &ServerIdentity,
        &FormatPlan,
        &[ParsedRow],
    ) -> Result<Vec<WindowProjection>, FormatCodecError> = hydrate_window_projections;
    let hydrate_pane: fn(
        &ServerIdentity,
        &FormatPlan,
        &ParsedRow,
    ) -> Result<PaneProjection, FormatCodecError> = hydrate_pane_projection;
    let hydrate_panes: fn(
        &ServerIdentity,
        &FormatPlan,
        &[ParsedRow],
    ) -> Result<Vec<PaneProjection>, FormatCodecError> = hydrate_pane_projections;
    let local_selector: for<'a> fn(
        &'a [WindowProjection],
    )
        -> Result<Option<&'a WindowProjection>, PointSelectionError> =
        select_local_window_projection;
    let holders: fn(&[WindowProjection], &ServerIdentity, &WindowId) -> Vec<SessionId> =
        holder_session_ids;

    let WindowProjection {
        window: window_info,
        link,
        window_active_clients,
        window_active_clients_list,
        window_active_sessions,
        window_active_sessions_list,
        window_linked_sessions,
        window_linked_sessions_list,
    } = &window;
    let _: &WindowInfo = window_info;
    let _: &u32 = window_active_clients;
    let _: &TmuxText = window_active_clients_list;
    let _: &u32 = window_active_sessions;
    let _: &TmuxText = window_active_sessions_list;
    let _: &u32 = window_linked_sessions;
    let _: &TmuxText = window_linked_sessions_list;
    let WindowLink {
        identity,
        window_active,
        window_activity_flag,
        window_bell_flag,
        window_end_flag,
        window_flags,
        window_last_flag,
        window_linked,
        window_marked_flag,
        window_raw_flags,
        window_silence_flag,
        window_stack_index,
        window_start_flag,
    } = link;
    let _: &WindowLinkIdentity = identity;
    for value in [
        window_active,
        window_activity_flag,
        window_bell_flag,
        window_end_flag,
        window_last_flag,
        window_linked,
        window_marked_flag,
        window_silence_flag,
        window_start_flag,
    ] {
        let _: &bool = value;
    }
    let _: &TmuxText = window_flags;
    let _: &TmuxText = window_raw_flags;
    let _: &u32 = window_stack_index;

    let PaneProjection {
        pane: pane_info,
        link_identity,
    } = &pane;
    let _: &PaneInfo = pane_info;
    let _: &WindowLinkIdentity = link_identity;
    assert!(std::ptr::eq(identity_accessor(link), identity));
    assert!(std::ptr::eq(window_accessor(&window), window_info));
    assert!(std::ptr::eq(link_accessor(&window), link));
    assert!(std::ptr::eq(pane_accessor(&pane), pane_info));
    assert!(std::ptr::eq(pane_link_accessor(&pane), link_identity));
    assert!(std::ptr::eq(const_window_link_identity(link), identity));
    assert!(std::ptr::eq(
        const_window_projection_window(&window),
        window_info,
    ));
    assert!(std::ptr::eq(const_window_projection_link(&window), link,));
    assert!(std::ptr::eq(const_pane_projection_pane(&pane), pane_info));
    assert!(std::ptr::eq(
        const_pane_projection_link_identity(&pane),
        link_identity,
    ));
    let _ = (
        window_plan,
        pane_plan,
        hydrate_window,
        hydrate_windows,
        hydrate_pane,
        hydrate_panes,
        local_selector,
        holders,
    );
}

#[test]
fn snapshot_projection_plans_have_exact_order_state_and_templates() {
    let cases = [
        (b"tmux 3.2a\n".as_slice(), 57, 15, 0),
        (b"tmux 3.3\n".as_slice(), 60, 12, 0),
        (b"tmux 3.6\n".as_slice(), 61, 11, 0),
        (b"tmux 3.7\n".as_slice(), 72, 0, 0),
        (b"tmux master\n".as_slice(), 57, 0, 15),
        (b"tmux next-3.8\n".as_slice(), 57, 0, 15),
    ];
    let expected_window: Vec<_> = WINDOW_INFO_DESCRIPTORS
        .iter()
        .copied()
        .chain(window_projection_suffix())
        .collect();
    let expected_pane: Vec<_> = PANE_INFO_DESCRIPTORS
        .iter()
        .copied()
        .chain(pane_projection_suffix())
        .collect();

    for (raw, pane_selected, unsupported, unproven) in cases {
        let version = version(raw);
        let window_plan = window_projection_plan(&version)
            .ok()
            .expect("Window projection plan is valid");
        assert_eq!(window_plan.profile(), ListProfile::Windows);
        assert_eq!(window_plan.purpose(), PlanPurpose::Projection);
        assert_eq!(window_plan.planned().len(), 31);
        assert_eq!(window_plan.descriptors_for_test().len(), 31);
        assert_descriptor_sequence(window_plan.descriptors_for_test(), &expected_window);
        assert_eq!(
            window_plan.template(),
            descriptor_template(&expected_window)
        );
        for (planned, expected) in window_plan.planned().iter().zip(&expected_window) {
            assert!(std::ptr::eq(planned.descriptor, *expected));
            assert!(matches!(planned.state, PlanFieldState::Selected { .. }));
        }

        let pane_plan = pane_projection_plan(&version)
            .ok()
            .expect("Pane projection plan is valid");
        let intrinsic = FormatPlan::for_profile(ListProfile::Panes, &version);
        let expected_selected: Vec<_> = intrinsic
            .descriptors_for_test()
            .iter()
            .copied()
            .chain(pane_projection_suffix())
            .collect();
        assert_eq!(pane_plan.profile(), ListProfile::Panes);
        assert_eq!(pane_plan.purpose(), PlanPurpose::Projection);
        assert_eq!(pane_plan.planned().len(), 72);
        assert_eq!(pane_plan.descriptors_for_test().len(), pane_selected);
        assert_descriptor_sequence(pane_plan.descriptors_for_test(), &expected_selected);
        assert_eq!(
            pane_plan.template(),
            descriptor_template(&expected_selected)
        );
        assert_eq!(
            &pane_plan.planned()[..PANE_INFO_DESCRIPTORS.len()],
            intrinsic.planned(),
        );
        assert_descriptor_sequence(
            &pane_plan
                .planned()
                .iter()
                .map(|planned| planned.descriptor)
                .collect::<Vec<_>>(),
            &expected_pane,
        );
        assert_eq!(
            pane_plan
                .planned()
                .iter()
                .filter(|field| matches!(field.state, PlanFieldState::Unsupported))
                .count(),
            unsupported,
        );
        assert_eq!(
            pane_plan
                .planned()
                .iter()
                .filter(|field| matches!(field.state, PlanFieldState::Unproven))
                .count(),
            unproven,
        );
        for planned in &pane_plan.planned()[PANE_INFO_DESCRIPTORS.len()..] {
            assert!(matches!(planned.state, PlanFieldState::Selected { .. }));
        }
    }
}

#[test]
fn snapshot_projection_window_rows_preserve_repeated_links_in_order() {
    let plan = window_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Window projection plan is valid");
    let rows = projection_rows(
        &plan,
        &[
            vec![
                ("session_id", b"$1"),
                ("window_index", b"1"),
                ("window_active", b"1"),
            ],
            vec![
                ("session_id", b"$1"),
                ("window_index", b"5"),
                ("window_active", b"0"),
            ],
        ],
    );
    let server_identity = projection_server_identity("/private/window-repeat-endpoint");
    let projections = hydrate_window_projections(&server_identity, &plan, &rows)
        .ok()
        .expect("two Window rows hydrate");

    assert_eq!(projections.len(), 2);
    assert_eq!(projections[0].window(), projections[1].window());
    assert_ne!(
        projections[0].link().identity(),
        projections[1].link().identity(),
    );
    assert_eq!(projections[0].link().identity().window_index(), 1);
    assert_eq!(projections[1].link().identity().window_index(), 5);
    assert_eq!(
        projections[0].link().identity().window_id(),
        projections[1].link().identity().window_id(),
    );
}

#[test]
fn snapshot_projection_pane_rows_preserve_each_winlink_edge_in_order() {
    let plan = pane_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Pane projection plan is valid");
    let rows = projection_rows(
        &plan,
        &[
            vec![
                ("session_id", b"$1"),
                ("window_id", b"@7"),
                ("window_index", b"1"),
            ],
            vec![
                ("session_id", b"$1"),
                ("window_id", b"@7"),
                ("window_index", b"5"),
            ],
        ],
    );
    let server_identity = projection_server_identity("/private/pane-repeat-endpoint");
    let projections = hydrate_pane_projections(&server_identity, &plan, &rows)
        .ok()
        .expect("two Pane rows hydrate");

    assert_eq!(projections.len(), 2);
    assert_eq!(projections[0].pane(), projections[1].pane());
    assert_ne!(
        projections[0].link_identity(),
        projections[1].link_identity(),
    );
    assert_eq!(projections[0].link_identity().window_index(), 1);
    assert_eq!(projections[1].link_identity().window_index(), 5);
    assert_eq!(
        projections[0].link_identity().server_identity(),
        &server_identity,
    );
    assert_eq!(
        projections[1].link_identity().server_identity(),
        &server_identity,
    );
    assert_eq!(
        projections[0].link_identity().window_id(),
        projections[1].link_identity().window_id(),
    );
}

#[test]
fn snapshot_projection_duplicate_rows_retain_each_intrinsic_observation() {
    let window_plan = window_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Window projection plan is valid");
    let window_rows = projection_rows(
        &window_plan,
        &[
            vec![
                ("session_id", b"$1"),
                ("window_id", b"@7"),
                ("window_index", b"1"),
                ("window_activity", b"11"),
            ],
            vec![
                ("session_id", b"$1"),
                ("window_id", b"@7"),
                ("window_index", b"5"),
                ("window_activity", b"22"),
            ],
        ],
    );
    let server_identity = projection_server_identity("/private/divergent-intrinsic-endpoint");
    let windows = hydrate_window_projections(&server_identity, &window_plan, &window_rows)
        .ok()
        .expect("divergent Window observations hydrate");
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].window.window_id, windows[1].window.window_id);
    assert_eq!(
        windows[0].link().identity().window_id(),
        &windows[0].window().window_id,
    );
    assert_eq!(
        windows[1].link().identity().window_id(),
        &windows[1].window().window_id,
    );
    assert_eq!(windows[0].link().identity().window_id().as_ref(), "@7");
    assert_eq!(windows[1].link().identity().window_id().as_ref(), "@7");
    assert_eq!(windows[0].window.window_activity, 11);
    assert_eq!(windows[1].window.window_activity, 22);
    assert_ne!(windows[0].window(), windows[1].window());
    assert_eq!(windows[0].link.identity.window_index(), 1);
    assert_eq!(windows[1].link.identity.window_index(), 5);

    let pane_plan = pane_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Pane projection plan is valid");
    let pane_rows = projection_rows(
        &pane_plan,
        &[
            vec![
                ("pane_id", b"%9"),
                ("session_id", b"$1"),
                ("window_id", b"@7"),
                ("window_index", b"1"),
                ("pane_width", b"80"),
            ],
            vec![
                ("pane_id", b"%9"),
                ("session_id", b"$1"),
                ("window_id", b"@7"),
                ("window_index", b"5"),
                ("pane_width", b"120"),
            ],
        ],
    );
    let panes = hydrate_pane_projections(&server_identity, &pane_plan, &pane_rows)
        .ok()
        .expect("divergent Pane observations hydrate");
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0].pane.pane_id, panes[1].pane.pane_id);
    assert_eq!(panes[0].pane.pane_width, 80);
    assert_eq!(panes[1].pane.pane_width, 120);
    assert_ne!(panes[0].pane(), panes[1].pane());
    assert_eq!(panes[0].link_identity.window_index(), 1);
    assert_eq!(panes[1].link_identity.window_index(), 5);
}

#[test]
fn snapshot_projection_enumeration_does_not_apply_local_point_selection() {
    let plan = window_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Window projection plan is valid");
    let rows = projection_rows(
        &plan,
        &[
            vec![("session_id", b"$1"), ("window_index", b"5")],
            vec![
                ("session_id", b"$2"),
                ("window_index", b"1"),
                ("window_active", b"1"),
            ],
        ],
    );
    let projections = hydrate_window_projections(
        &projection_server_identity("/private/enumeration-endpoint"),
        &plan,
        &rows,
    )
    .ok()
    .expect("enumeration retains structurally valid mixed-Session rows");

    assert_eq!(projections.len(), 2);
    assert_eq!(
        select_local_window_projection(&projections),
        Err(PointSelectionError::MixedSession),
    );
}

fn selection_projection(session_id: &str, window_index: i32, active: bool) -> WindowProjection {
    let rendered_index = window_index.to_string();
    window_projection_fixture(
        b"tmux 3.7\n",
        "/private/selection-endpoint",
        &[
            ("session_id", session_id.as_bytes()),
            ("window_index", rendered_index.as_bytes()),
            ("window_active", if active { b"1" } else { b"0" }),
        ],
    )
}

#[test]
fn snapshot_projection_local_selection_covers_empty_active_and_minimum_cases() {
    assert_eq!(select_local_window_projection(&[]), Ok(None));

    let only = vec![selection_projection("$1", 8, false)];
    assert_eq!(
        std::ptr::from_ref(
            select_local_window_projection(&only)
                .ok()
                .flatten()
                .expect("one candidate is selected"),
        ),
        std::ptr::from_ref(&only[0]),
    );

    let active_low = vec![
        selection_projection("$1", 2, true),
        selection_projection("$1", 8, false),
    ];
    assert_eq!(
        std::ptr::from_ref(
            select_local_window_projection(&active_low)
                .ok()
                .flatten()
                .expect("active candidate is selected"),
        ),
        std::ptr::from_ref(&active_low[0]),
    );

    let active_high = vec![
        selection_projection("$1", -4, false),
        selection_projection("$1", 9, true),
    ];
    assert_eq!(
        std::ptr::from_ref(
            select_local_window_projection(&active_high)
                .ok()
                .flatten()
                .expect("active candidate outranks the minimum index"),
        ),
        std::ptr::from_ref(&active_high[1]),
    );

    let two_active = vec![
        selection_projection("$1", 9, true),
        selection_projection("$1", -4, true),
    ];
    assert_eq!(
        std::ptr::from_ref(
            select_local_window_projection(&two_active)
                .ok()
                .flatten()
                .expect("first active candidate is stable"),
        ),
        std::ptr::from_ref(&two_active[0]),
    );

    let reversed_inactive = vec![
        selection_projection("$1", 7, false),
        selection_projection("$1", -3, false),
    ];
    assert_eq!(
        std::ptr::from_ref(
            select_local_window_projection(&reversed_inactive)
                .ok()
                .flatten()
                .expect("lowest signed index is selected"),
        ),
        std::ptr::from_ref(&reversed_inactive[1]),
    );

    let equal_minimum = vec![
        selection_projection("$1", -3, false),
        selection_projection("$1", -3, false),
        selection_projection("$1", 4, false),
    ];
    assert_eq!(
        std::ptr::from_ref(
            select_local_window_projection(&equal_minimum)
                .ok()
                .flatten()
                .expect("first equal minimum is stable"),
        ),
        std::ptr::from_ref(&equal_minimum[0]),
    );
}

#[test]
fn snapshot_projection_local_selection_rejects_mixed_sessions_before_active_choice() {
    let mixed = vec![
        selection_projection("$1", -20, false),
        selection_projection("$2", 20, true),
    ];

    assert_eq!(
        select_local_window_projection(&mixed),
        Err(PointSelectionError::MixedSession),
    );
    assert_eq!(
        PointSelectionError::MixedSession.to_string(),
        "mixed Session candidates"
    );
    assert!(std::error::Error::source(&PointSelectionError::MixedSession).is_none());
    assert_impl_all!(PointSelectionError: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
    let PointSelectionError::MixedSession = PointSelectionError::MixedSession;
}

#[test]
fn snapshot_projection_hydration_rejects_wrong_profile_and_purpose_before_decoding() {
    let server_identity = projection_server_identity("/private/wrong-plan-endpoint");
    let pane_plan = pane_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Pane projection plan is valid");
    let (pane_row, _) = parsed_row(&pane_plan, &[]);
    let wrong_profile = error(hydrate_window_projection(
        &server_identity,
        &pane_plan,
        &pane_row,
    ));
    assert_eq!(wrong_profile.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(wrong_profile.phase(), FormatCodecPhase::Decode);
    assert_eq!(wrong_profile.row(), Some(0));
    assert_eq!(wrong_profile.field(), None);
    assert_eq!(wrong_profile.field_name(), None);
    assert_eq!(wrong_profile.offset(), None);
    assert_eq!(wrong_profile.profile(), Some(ListProfile::Panes));
    assert_safe_diagnostic(&wrong_profile);

    let intrinsic = FormatPlan::for_profile(ListProfile::Windows, &version(b"tmux 3.7\n"));
    let (intrinsic_row, _) = parsed_row(&intrinsic, &[]);
    let wrong_purpose = error(hydrate_window_projection(
        &server_identity,
        &intrinsic,
        &intrinsic_row,
    ));
    assert_eq!(wrong_purpose.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(wrong_purpose.phase(), FormatCodecPhase::Decode);
    assert_eq!(wrong_purpose.row(), Some(0));
    assert_eq!(wrong_purpose.field(), None);
    assert_eq!(wrong_purpose.field_name(), None);
    assert_eq!(wrong_purpose.offset(), None);
    assert_eq!(wrong_purpose.profile(), Some(ListProfile::Windows));
    assert_safe_diagnostic(&wrong_purpose);
}

fn assert_projection_guard_error(error: &FormatCodecError, profile: ListProfile) {
    assert_eq!(error.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), None);
    assert_eq!(error.field_name(), None);
    assert_eq!(error.expected(), None);
    assert_eq!(error.offset(), None);
    assert_eq!(error.profile(), Some(profile));
    assert_safe_diagnostic(error);
}

#[test]
fn snapshot_projection_pane_hydration_and_lists_guard_profile_and_purpose_first() {
    let server_identity = projection_server_identity("/private/pane-guard-endpoint");
    let wrong_profile = window_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Window projection plan is valid");
    let (window_row, _) = parsed_row(&wrong_profile, &[]);
    let single_profile = error(hydrate_pane_projection(
        &server_identity,
        &wrong_profile,
        &window_row,
    ));
    let list_profile = error(hydrate_pane_projections(
        &server_identity,
        &wrong_profile,
        std::slice::from_ref(&window_row),
    ));
    assert_projection_guard_error(&single_profile, ListProfile::Windows);
    assert_projection_guard_error(&list_profile, ListProfile::Windows);

    let wrong_purpose = FormatPlan::for_profile(ListProfile::Panes, &version(b"tmux 3.7\n"));
    let (pane_row, _) = parsed_row(&wrong_purpose, &[]);
    let single_purpose = error(hydrate_pane_projection(
        &server_identity,
        &wrong_purpose,
        &pane_row,
    ));
    let list_purpose = error(hydrate_pane_projections(
        &server_identity,
        &wrong_purpose,
        std::slice::from_ref(&pane_row),
    ));
    assert_projection_guard_error(&single_purpose, ListProfile::Panes);
    assert_projection_guard_error(&list_purpose, ListProfile::Panes);
}

#[test]
fn snapshot_projection_hydration_rejects_reordered_underflow_and_trailing_plans() {
    let detected = version(b"tmux 3.7\n");
    let server_identity = projection_server_identity("/private/plan-shape-endpoint");

    let mut reordered_request = window_projection_requested();
    let suffix_start = WINDOW_INFO_SUPPLEMENTS.len();
    reordered_request.swap(suffix_start, suffix_start + 1);
    let reordered =
        FormatPlan::for_descriptors(ListProfile::Windows, &detected, &reordered_request)
            .ok()
            .expect("reordered trusted-static projection is structurally constructible");
    let (row, _) = parsed_row(&reordered, &[]);
    let reordered_error = error(hydrate_window_projection(
        &server_identity,
        &reordered,
        &row,
    ));
    assert_eq!(
        reordered_error.kind(),
        FormatCodecErrorKind::PlanRowMismatch
    );
    assert_eq!(reordered_error.phase(), FormatCodecPhase::Decode);
    assert_eq!(reordered_error.row(), Some(0));
    assert_eq!(reordered_error.field(), Some(WINDOW_INFO_DESCRIPTORS.len()));
    assert_eq!(reordered_error.field_name(), Some("session_id"));
    assert!(reordered_error.offset().is_some());
    assert_eq!(reordered_error.profile(), None);
    assert_safe_diagnostic(&reordered_error);

    let underflow =
        FormatPlan::for_descriptors(ListProfile::Windows, &detected, WINDOW_INFO_SUPPLEMENTS)
            .ok()
            .expect("shared-prefix-only projection is constructible");
    let (row, _) = parsed_row(&underflow, &[]);
    let underflow_error = error(hydrate_window_projection(
        &server_identity,
        &underflow,
        &row,
    ));
    assert_eq!(
        underflow_error.kind(),
        FormatCodecErrorKind::PlanRowMismatch
    );
    assert_eq!(underflow_error.row(), Some(0));
    assert_eq!(underflow_error.field(), Some(WINDOW_INFO_DESCRIPTORS.len()));
    assert_eq!(underflow_error.field_name(), Some("session_id"));
    assert_eq!(underflow_error.offset(), None);
    assert_eq!(underflow_error.profile(), None);
    assert_safe_diagnostic(&underflow_error);

    let mut trailing_request = window_projection_requested();
    trailing_request.push(&CLIENT_MODE_FORMAT);
    let trailing = FormatPlan::for_descriptors(ListProfile::Windows, &detected, &trailing_request)
        .ok()
        .expect("projection with a trailing catalog field is constructible");
    let (row, _) = parsed_row(&trailing, &[]);
    let trailing_error = error(hydrate_window_projection(&server_identity, &trailing, &row));
    assert_eq!(trailing_error.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(trailing_error.row(), Some(0));
    assert_eq!(trailing_error.field(), Some(31));
    assert_eq!(trailing_error.field_name(), Some("client_mode_format"));
    assert_eq!(trailing_error.offset(), None);
    assert_eq!(trailing_error.profile(), None);
    assert_safe_diagnostic(&trailing_error);
}

#[test]
fn snapshot_projection_typed_edge_errors_and_local_precedence_are_exact() {
    let detected = version(b"tmux 3.7\n");
    let server_identity = projection_server_identity("/private/typed-edge-endpoint");
    let exact = window_projection_plan(&detected)
        .ok()
        .expect("Window projection plan is valid");

    let (row, offsets) = parsed_row(&exact, &[("window_index", SHORT_SENTINEL.as_bytes())]);
    let invalid_index = error(hydrate_window_projection(&server_identity, &exact, &row));
    assert_invalid_value(
        &invalid_index,
        &exact,
        &offsets,
        "window_index",
        DecoderKind::I32,
    );

    let (row, offsets) = parsed_row(&exact, &[("session_id", b"")]);
    let empty_session = error(hydrate_window_projection(&server_identity, &exact, &row));
    let session_field = exact
        .descriptors_for_test()
        .iter()
        .position(|descriptor| descriptor.name() == "session_id")
        .expect("session edge is selected");
    assert_eq!(
        empty_session.kind(),
        FormatCodecErrorKind::RequiredFieldEmpty
    );
    assert_eq!(empty_session.phase(), FormatCodecPhase::Decode);
    assert_eq!(empty_session.row(), Some(0));
    assert_eq!(empty_session.field(), Some(session_field));
    assert_eq!(empty_session.field_name(), Some("session_id"));
    assert_eq!(empty_session.expected(), Some(DecoderKind::SessionId));
    assert_eq!(empty_session.offset(), offsets.get("session_id").copied());
    assert_eq!(empty_session.profile(), None);
    assert_safe_diagnostic(&empty_session);

    let mut reordered_request = window_projection_requested();
    let suffix_start = WINDOW_INFO_SUPPLEMENTS.len();
    reordered_request.swap(suffix_start, suffix_start + 1);
    let reordered =
        FormatPlan::for_descriptors(ListProfile::Windows, &detected, &reordered_request)
            .ok()
            .expect("reordered projection is constructible");
    let (row, offsets) = parsed_row(&reordered, &[("window_activity", b"01")]);
    let first_error = error(hydrate_window_projection(
        &server_identity,
        &reordered,
        &row,
    ));
    assert_invalid_value(
        &first_error,
        &reordered,
        &offsets,
        "window_activity",
        DecoderKind::Timestamp,
    );
}

#[test]
fn snapshot_projection_pane_edge_omission_is_a_local_row_mismatch() {
    let detected = version(b"tmux 3.7\n");
    let mut requested = pane_projection_requested();
    assert_eq!(
        std::ptr::from_ref(requested.pop().expect("Window index suffix exists")),
        std::ptr::from_ref(&WINDOW_INDEX),
    );
    let omitted = FormatPlan::for_descriptors(ListProfile::Panes, &detected, &requested)
        .ok()
        .expect("Pane plan without the final edge remains constructible");
    let (row, _) = parsed_row(&omitted, &[]);
    let error = error(hydrate_pane_projection(
        &projection_server_identity("/private/pane-omission-endpoint"),
        &omitted,
        &row,
    ));

    assert_eq!(error.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), Some(PANE_INFO_DESCRIPTORS.len() + 2));
    assert_eq!(error.field_name(), Some("window_index"));
    assert_eq!(error.offset(), None);
    assert_eq!(error.profile(), None);
    assert_safe_diagnostic(&error);
}

#[test]
fn snapshot_projection_pane_trailing_descriptor_requires_finish() {
    let detected = version(b"tmux 3.7\n");
    let mut requested = pane_projection_requested();
    requested.push(&CLIENT_MODE_FORMAT);
    let trailing = FormatPlan::for_descriptors(ListProfile::Panes, &detected, &requested)
        .ok()
        .expect("Pane projection with one trailing field is constructible");
    let (row, _) = parsed_row(&trailing, &[]);
    let error = error(hydrate_pane_projection(
        &projection_server_identity("/private/pane-finish-endpoint"),
        &trailing,
        &row,
    ));

    assert_eq!(trailing.planned().len(), 73);
    assert_eq!(trailing.descriptors_for_test().len(), 73);
    assert_eq!(error.kind(), FormatCodecErrorKind::PlanRowMismatch);
    assert_eq!(error.phase(), FormatCodecPhase::Decode);
    assert_eq!(error.row(), Some(0));
    assert_eq!(error.field(), Some(72));
    assert_eq!(error.field_name(), Some("client_mode_format"));
    assert_eq!(error.expected(), None);
    assert_eq!(error.offset(), None);
    assert_eq!(error.profile(), None);
    assert_safe_diagnostic(&error);
}

#[test]
fn snapshot_projection_list_hydration_returns_no_partial_value_on_later_error() {
    let server_identity = projection_server_identity("/private/list-error-endpoint");
    let window_plan = window_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Window projection plan is valid");
    let window_rows = projection_rows(
        &window_plan,
        &[
            vec![("window_index", b"1")],
            vec![("window_index", SHORT_SENTINEL.as_bytes())],
        ],
    );
    let window_error = error(hydrate_window_projections(
        &server_identity,
        &window_plan,
        &window_rows,
    ));
    assert_eq!(window_error.kind(), FormatCodecErrorKind::InvalidValue);
    assert_eq!(window_error.row(), Some(1));
    assert_eq!(window_error.field_name(), Some("window_index"));
    assert_safe_diagnostic(&window_error);

    let pane_plan = pane_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Pane projection plan is valid");
    let pane_rows = projection_rows(
        &pane_plan,
        &[
            vec![("window_index", b"1")],
            vec![("window_index", SHORT_SENTINEL.as_bytes())],
        ],
    );
    let pane_error = error(hydrate_pane_projections(
        &server_identity,
        &pane_plan,
        &pane_rows,
    ));
    assert_eq!(pane_error.kind(), FormatCodecErrorKind::InvalidValue);
    assert_eq!(pane_error.row(), Some(1));
    assert_eq!(pane_error.field_name(), Some("window_index"));
    assert_safe_diagnostic(&pane_error);
}

#[test]
#[allow(
    clippy::type_complexity,
    reason = "exact private composition signatures are part of this contract"
)]
fn snapshot_projection_stdout_composition_signatures_are_exact() {
    let windows: fn(
        &ServerIdentity,
        &FormatPlan,
        &[u8],
    ) -> Result<Vec<WindowProjection>, FormatCodecError> = hydrate_window_projections_from_stdout;
    let panes: fn(
        &ServerIdentity,
        &FormatPlan,
        &[u8],
    ) -> Result<Vec<PaneProjection>, FormatCodecError> = hydrate_pane_projections_from_stdout;

    let _ = (windows, panes);
}

#[test]
fn snapshot_projection_stdout_composition_preserves_rows_and_empty_listings() {
    let server_identity = projection_server_identity("/private/stdout-composition-endpoint");
    let detected = version(b"tmux 3.7\n");
    let window_plan = window_projection_plan(&detected)
        .ok()
        .expect("Window projection plan is valid");
    let mut window_stdout = Vec::new();
    for overrides in [
        [
            ("window_id", b"@7".as_slice()),
            ("session_id", b"$3".as_slice()),
            ("window_index", b"-2".as_slice()),
        ],
        [
            ("window_id", b"@7".as_slice()),
            ("session_id", b"$3".as_slice()),
            ("window_index", b"5".as_slice()),
        ],
    ] {
        window_stdout.extend_from_slice(&framed_row(&window_plan, &overrides).0);
    }
    let windows =
        hydrate_window_projections_from_stdout(&server_identity, &window_plan, &window_stdout)
            .ok()
            .expect("raw Window rows parse and hydrate");
    assert_eq!(windows.len(), 2);
    assert_eq!(
        windows
            .iter()
            .map(|projection| projection.link.identity.window_index())
            .collect::<Vec<_>>(),
        [-2, 5],
    );
    for projection in &windows {
        assert_eq!(projection.link.identity.server_identity(), &server_identity);
        assert_eq!(projection.link.identity.session_id().as_ref(), "$3");
        assert_eq!(projection.link.identity.window_id().as_ref(), "@7");
        assert_eq!(projection.window.window_id.as_ref(), "@7");
    }
    assert!(
        hydrate_window_projections_from_stdout(&server_identity, &window_plan, b"")
            .ok()
            .expect("empty Window stdout is an empty listing")
            .is_empty()
    );

    let pane_plan = pane_projection_plan(&detected)
        .ok()
        .expect("Pane projection plan is valid");
    let mut pane_stdout = Vec::new();
    for overrides in [
        [
            ("pane_id", b"%9".as_slice()),
            ("session_id", b"$3".as_slice()),
            ("window_id", b"@7".as_slice()),
            ("window_index", b"-2".as_slice()),
        ],
        [
            ("pane_id", b"%9".as_slice()),
            ("session_id", b"$3".as_slice()),
            ("window_id", b"@7".as_slice()),
            ("window_index", b"5".as_slice()),
        ],
    ] {
        pane_stdout.extend_from_slice(&framed_row(&pane_plan, &overrides).0);
    }
    let panes = hydrate_pane_projections_from_stdout(&server_identity, &pane_plan, &pane_stdout)
        .ok()
        .expect("raw Pane rows parse and hydrate");
    assert_eq!(panes.len(), 2);
    assert_eq!(
        panes
            .iter()
            .map(|projection| projection.link_identity.window_index())
            .collect::<Vec<_>>(),
        [-2, 5],
    );
    for projection in &panes {
        assert_eq!(projection.link_identity.server_identity(), &server_identity);
        assert_eq!(projection.link_identity.session_id().as_ref(), "$3");
        assert_eq!(projection.link_identity.window_id().as_ref(), "@7");
        assert_eq!(projection.pane.pane_id.as_ref(), "%9");
    }
    assert!(
        hydrate_pane_projections_from_stdout(&server_identity, &pane_plan, b"")
            .ok()
            .expect("empty Pane stdout is an empty listing")
            .is_empty()
    );
}

fn malformed_stdout_sentinel() -> Vec<u8> {
    let mut stdout = Vec::new();
    stdout.extend_from_slice(SHORT_SENTINEL.as_bytes());
    stdout.push(b'_');
    stdout.extend_from_slice(LONG_SENTINEL.as_bytes());
    stdout.push(b'_');
    stdout.extend_from_slice(&CONTROL_SENTINEL);
    stdout.push(b'_');
    stdout.extend_from_slice(&INVALID_UTF8_SENTINEL);
    stdout
}

fn assert_missing_field_terminator(
    error: &FormatCodecError,
    row: usize,
    field_name: &'static str,
    offset: usize,
) {
    assert_eq!(error.kind(), FormatCodecErrorKind::MissingFieldTerminator);
    assert_eq!(error.phase(), FormatCodecPhase::Field);
    assert_eq!(error.row(), Some(row));
    assert_eq!(error.field(), Some(0));
    assert_eq!(error.field_name(), Some(field_name));
    assert_eq!(error.expected(), None);
    assert_eq!(error.offset(), Some(offset));
    assert_eq!(error.profile(), None);
    assert_safe_diagnostic(error);
}

#[test]
fn snapshot_projection_stdout_composition_guards_before_parsing_malformed_payloads() {
    let server_identity = projection_server_identity("/private/stdout-guard-endpoint");
    let detected = version(b"tmux 3.7\n");
    let malformed = malformed_stdout_sentinel();

    let pane_projection = pane_projection_plan(&detected)
        .ok()
        .expect("Pane projection plan is valid");
    let wrong_window_profile = error(hydrate_window_projections_from_stdout(
        &server_identity,
        &pane_projection,
        &malformed,
    ));
    assert_projection_guard_error(&wrong_window_profile, ListProfile::Panes);

    let intrinsic_windows = FormatPlan::for_profile(ListProfile::Windows, &detected);
    let wrong_window_purpose = error(hydrate_window_projections_from_stdout(
        &server_identity,
        &intrinsic_windows,
        &malformed,
    ));
    assert_projection_guard_error(&wrong_window_purpose, ListProfile::Windows);

    let window_projection = window_projection_plan(&detected)
        .ok()
        .expect("Window projection plan is valid");
    let wrong_pane_profile = error(hydrate_pane_projections_from_stdout(
        &server_identity,
        &window_projection,
        &malformed,
    ));
    assert_projection_guard_error(&wrong_pane_profile, ListProfile::Windows);

    let intrinsic_panes = FormatPlan::for_profile(ListProfile::Panes, &detected);
    let wrong_pane_purpose = error(hydrate_pane_projections_from_stdout(
        &server_identity,
        &intrinsic_panes,
        &malformed,
    ));
    assert_projection_guard_error(&wrong_pane_purpose, ListProfile::Panes);
}

#[test]
fn snapshot_projection_stdout_composition_reports_exact_framing_errors() {
    let server_identity = projection_server_identity("/private/stdout-framing-endpoint");
    let detected = version(b"tmux 3.7\n");
    let malformed = malformed_stdout_sentinel();
    let window_plan = window_projection_plan(&detected)
        .ok()
        .expect("Window projection plan is valid");
    let window_error = error(hydrate_window_projections_from_stdout(
        &server_identity,
        &window_plan,
        &malformed,
    ));
    assert_missing_field_terminator(&window_error, 0, "window_id", malformed.len());

    let pane_plan = pane_projection_plan(&detected)
        .ok()
        .expect("Pane projection plan is valid");
    let pane_error = error(hydrate_pane_projections_from_stdout(
        &server_identity,
        &pane_plan,
        &malformed,
    ));
    assert_missing_field_terminator(&pane_error, 0, "pane_id", malformed.len());
}

#[test]
fn snapshot_projection_stdout_composition_returns_only_error_for_later_bad_rows() {
    let server_identity = projection_server_identity("/private/stdout-later-row-endpoint");
    let detected = version(b"tmux 3.7\n");
    let malformed = malformed_stdout_sentinel();
    let window_plan = window_projection_plan(&detected)
        .ok()
        .expect("Window projection plan is valid");
    let mut window_stdout = framed_row(&window_plan, &[("window_index", b"1")]).0;
    let later_start = window_stdout.len();
    window_stdout.extend_from_slice(&malformed);
    let window_error = error(hydrate_window_projections_from_stdout(
        &server_identity,
        &window_plan,
        &window_stdout,
    ));
    assert_missing_field_terminator(&window_error, 1, "window_id", later_start + malformed.len());

    let pane_plan = pane_projection_plan(&detected)
        .ok()
        .expect("Pane projection plan is valid");
    let mut pane_stdout = framed_row(&pane_plan, &[("window_index", b"1")]).0;
    pane_stdout.extend_from_slice(
        &framed_row(&pane_plan, &[("window_index", SHORT_SENTINEL.as_bytes())]).0,
    );
    let pane_error = error(hydrate_pane_projections_from_stdout(
        &server_identity,
        &pane_plan,
        &pane_stdout,
    ));
    assert_eq!(pane_error.kind(), FormatCodecErrorKind::InvalidValue);
    assert_eq!(pane_error.phase(), FormatCodecPhase::Decode);
    assert_eq!(pane_error.row(), Some(1));
    assert_eq!(pane_error.field_name(), Some("window_index"));
    assert_eq!(pane_error.expected(), Some(DecoderKind::I32));
    assert_eq!(pane_error.profile(), None);
    assert_safe_diagnostic(&pane_error);
}

#[cfg(feature = "query")]
#[test]
fn snapshot_projection_pane_unavailability_is_preserved_while_edges_are_selected() {
    for raw in [
        b"tmux 3.2a\n".as_slice(),
        b"tmux master\n".as_slice(),
        b"tmux next-3.8\n".as_slice(),
    ] {
        let plan = pane_projection_plan(&version(raw))
            .ok()
            .expect("Pane projection plan is valid");
        let (row, _) = parsed_row(
            &plan,
            &[
                ("session_id", b"$9"),
                ("window_id", b"@7"),
                ("window_index", b"-4"),
            ],
        );
        let projection = hydrate_pane_projection(
            &projection_server_identity("/private/pane-availability-endpoint"),
            &plan,
            &row,
        )
        .ok()
        .expect("Pane projection hydrates across availability states");
        let intrinsic = pane_fixture(raw, &[])
            .ok()
            .expect("matching intrinsic PaneInfo hydrates");

        assert_eq!(projection.pane(), &intrinsic);
        assert_eq!(projection.link_identity().session_id().as_ref(), "$9");
        assert_eq!(projection.link_identity().window_id().as_ref(), "@7");
        assert_eq!(projection.link_identity().window_index(), -4);
        for edge in &plan.planned()[PANE_INFO_DESCRIPTORS.len()..] {
            assert!(matches!(edge.state, PlanFieldState::Selected { .. }));
        }
    }
}

#[test]
fn snapshot_projection_raw_link_aggregates_remain_typed_opaque_observations() {
    let plan = window_projection_plan(&version(b"tmux 3.7\n"))
        .ok()
        .expect("Window projection plan is valid");
    let rows = projection_rows(
        &plan,
        &[
            vec![
                ("session_id", b"$1"),
                ("window_index", b"1"),
                ("window_linked", b"0"),
                ("window_linked_sessions", b"2"),
                ("window_linked_sessions_list", b"dup,dup"),
                ("window_flags", b""),
                ("window_raw_flags", b""),
            ],
            vec![
                ("session_id", b"$1"),
                ("window_index", b"5"),
                ("window_linked", b"1"),
                ("window_linked_sessions", b"1"),
                ("window_linked_sessions_list", b"dup,dup"),
            ],
        ],
    );
    let projections = hydrate_window_projections(
        &projection_server_identity("/private/raw-aggregate-endpoint"),
        &plan,
        &rows,
    )
    .ok()
    .expect("raw aggregate fixtures hydrate");

    assert!(!projections[0].link.window_linked);
    assert_eq!(projections[0].window_linked_sessions, 2);
    assert_eq!(
        projections[0].window_linked_sessions_list.as_bytes(),
        b"dup,dup",
    );
    assert!(projections[0].link.window_flags.as_bytes().is_empty());
    assert!(projections[0].link.window_raw_flags.as_bytes().is_empty());
    assert!(projections[1].link.window_linked);
    assert_eq!(projections[1].window_linked_sessions, 1);
    assert_eq!(
        projections[1].window_linked_sessions_list.as_bytes(),
        b"dup,dup",
    );
}

#[test]
fn snapshot_projection_window_suffix_fields_map_to_their_exact_slots() {
    let bool_fields = [
        "window_active",
        "window_activity_flag",
        "window_bell_flag",
        "window_end_flag",
        "window_last_flag",
        "window_linked",
        "window_marked_flag",
        "window_silence_flag",
        "window_start_flag",
    ];
    for (expected_index, field_name) in bool_fields.iter().copied().enumerate() {
        let projection = window_projection_fixture(
            b"tmux 3.7\n",
            "/private/exact-bool-slot-endpoint",
            &[(field_name, b"1")],
        );
        let actual = [
            projection.link.window_active,
            projection.link.window_activity_flag,
            projection.link.window_bell_flag,
            projection.link.window_end_flag,
            projection.link.window_last_flag,
            projection.link.window_linked,
            projection.link.window_marked_flag,
            projection.link.window_silence_flag,
            projection.link.window_start_flag,
        ];
        for (actual_index, value) in actual.into_iter().enumerate() {
            assert_eq!(
                value,
                actual_index == expected_index,
                "{field_name} must hydrate only its exact bool member",
            );
        }
    }

    let projection = window_projection_fixture(
        b"tmux 3.7\n",
        "/private/exact-scalar-slot-endpoint",
        &[
            ("window_flags", b"flags-distinct"),
            ("window_raw_flags", b"raw-flags-distinct"),
            ("window_stack_index", b"101"),
            ("window_active_clients", b"202"),
            ("window_active_clients_list", b"clients-distinct"),
            ("window_active_sessions", b"303"),
            ("window_active_sessions_list", b"active-sessions-distinct"),
            ("window_linked_sessions", b"404"),
            ("window_linked_sessions_list", b"linked-sessions-distinct"),
        ],
    );
    assert_eq!(projection.link.window_flags.as_bytes(), b"flags-distinct");
    assert_eq!(
        projection.link.window_raw_flags.as_bytes(),
        b"raw-flags-distinct",
    );
    assert_eq!(projection.link.window_stack_index, 101);
    assert_eq!(projection.window_active_clients, 202);
    assert_eq!(
        projection.window_active_clients_list.as_bytes(),
        b"clients-distinct",
    );
    assert_eq!(projection.window_active_sessions, 303);
    assert_eq!(
        projection.window_active_sessions_list.as_bytes(),
        b"active-sessions-distinct",
    );
    assert_eq!(projection.window_linked_sessions, 404);
    assert_eq!(
        projection.window_linked_sessions_list.as_bytes(),
        b"linked-sessions-distinct",
    );
}

#[test]
fn snapshot_projection_holder_sessions_use_only_structural_link_identity() {
    let projections = vec![
        window_projection_fixture(
            b"tmux 3.7\n",
            "/private/holder-endpoint-a",
            &[
                ("session_id", b"$2"),
                ("window_id", b"@1"),
                ("window_index", b"5"),
                ("window_linked_sessions", b"0"),
                ("window_linked_sessions_list", b"contradictory-a"),
            ],
        ),
        window_projection_fixture(
            b"tmux 3.7\n",
            "/private/holder-endpoint-a",
            &[
                ("session_id", b"$1"),
                ("window_id", b"@1"),
                ("window_index", b"1"),
                ("window_linked_sessions", b"99"),
                ("window_linked_sessions_list", b"contradictory-b,c,d"),
            ],
        ),
        window_projection_fixture(
            b"tmux 3.7\n",
            "/private/holder-endpoint-a",
            &[
                ("session_id", b"$2"),
                ("window_id", b"@1"),
                ("window_index", b"8"),
                ("window_linked_sessions", b"1"),
                ("window_linked_sessions_list", b"not-a-relation"),
            ],
        ),
        window_projection_fixture(
            b"tmux 3.7\n",
            "/private/holder-endpoint-a",
            &[
                ("session_id", b"$3"),
                ("window_id", b"@2"),
                ("window_index", b"3"),
            ],
        ),
        window_projection_fixture(
            b"tmux 3.7\n",
            "/private/holder-endpoint-b",
            &[
                ("session_id", b"$4"),
                ("window_id", b"@1"),
                ("window_index", b"4"),
            ],
        ),
    ];
    let endpoint = projection_server_identity("/private/holder-endpoint-a");
    let window_id: WindowId = "@1".parse().expect("fixture Window ID is valid");
    let holders = holder_session_ids(&projections, &endpoint, &window_id);

    assert_eq!(
        holders.iter().map(AsRef::<str>::as_ref).collect::<Vec<_>>(),
        ["$2", "$1"],
    );
}

#[test]
fn snapshot_projection_nested_debug_redacts_endpoint_and_all_text_payloads() {
    let text = LONG_SENTINEL.as_bytes();
    let numeric_text = format!("{text:?}");
    let window = window_projection_fixture(
        b"tmux 3.7\n",
        "/private/nested-debug-endpoint-sentinel/socket",
        &[
            ("window_layout", text),
            ("window_name", text),
            ("window_visible_layout", text),
            ("window_flags", text),
            ("window_raw_flags", text),
            ("window_active_clients_list", text),
            ("window_active_sessions_list", text),
            ("window_linked_sessions_list", text),
        ],
    );
    let pane = pane_projection_fixture(
        b"tmux 3.7\n",
        "/private/nested-debug-endpoint-sentinel/socket",
        &[
            ("cursor_character", text),
            ("pane_bg", text),
            ("pane_current_command", text),
            ("pane_current_path", text),
            ("pane_dead_signal", text),
            ("pane_fg", text),
            ("pane_flags", text),
            ("pane_mode", text),
            ("pane_path", text),
            ("pane_search_string", text),
            ("pane_start_command", text),
            ("pane_start_path", text),
            ("pane_tabs", text),
            ("pane_title", text),
            ("pane_tty", text),
        ],
    );
    let debug = format!("{window:?}\n{pane:?}");

    assert!(!debug.contains(LONG_SENTINEL));
    assert!(!debug.contains(&numeric_text));
    assert!(!debug.contains("nested-debug-endpoint-sentinel"));
    assert!(!debug.contains("/private"));
}

#[cfg(feature = "query")]
#[test]
fn snapshot_projection_client_attachment_descriptors_remain_catalog_only() {
    assert!(!CLIENT_INFO_DESCRIPTORS.iter().any(|descriptor| {
        std::ptr::from_ref(*descriptor) == std::ptr::from_ref(&CLIENT_SESSION)
    }));
    assert!(!CLIENT_INFO_DESCRIPTORS.iter().any(|descriptor| {
        std::ptr::from_ref(*descriptor) == std::ptr::from_ref(&CLIENT_LAST_SESSION)
    }));
    assert_eq!(ClientInfo::FILTER_TARGET, "client");
}

#[cfg(feature = "test-support")]
fn projection_test_executable() -> std::ffi::OsString {
    std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| std::ffi::OsString::from("tmux"))
}

#[cfg(feature = "test-support")]
async fn projection_test_server() -> (TestServer, std::ffi::OsString, Option<std::ffi::OsString>) {
    let explicit = std::env::var_os("LIBTMUX_TEST_TMUX");
    let executable = projection_test_executable();
    let guard = TestServer::builder()
        .tmux_executable(executable.clone())
        .start()
        .await
        .expect("the explicitly selected tmux starts");
    assert!(
        guard.server().tmux_executable() == executable.as_os_str(),
        "TestServer retains the selected tmux executable",
    );
    (guard, executable, explicit)
}

#[cfg(feature = "test-support")]
async fn projection_run(server: &crate::Server, command: Command) {
    let result = server
        .cmd(command)
        .await
        .expect("tmux setup command executes");
    assert!(result.success(), "tmux setup command succeeds");
    assert!(result.stderr().is_empty(), "tmux setup stderr is empty");
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_session_listing_hydrates_ordered_named_snapshots() {
    let (guard, _executable, _explicit) = projection_test_server().await;
    let server = guard.server();

    for name in ["alpha", "beta"] {
        projection_run(
            server,
            Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg(name)
                .arg("sleep 120"),
        )
        .await;
    }

    let version = server
        .capabilities()
        .await
        .expect("tmux capabilities are detected")
        .tmux_version();
    let plan = FormatPlan::for_profile(ListProfile::Sessions, version);
    let result = server
        .cmd(Command::new("list-sessions").arg("-F").arg(plan.template()))
        .await
        .expect("the session listing executes");
    assert!(result.success(), "the session listing succeeds");

    let sessions = hydrate_session_infos_from_stdout(&plan, result.stdout())
        .ok()
        .expect("live session rows hydrate");

    let names = sessions
        .iter()
        .map(|session| session.session_name().as_bytes().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(names, [b"alpha".to_vec(), b"beta".to_vec()]);

    // The baseline identity is unconditional, so it needs no evidence.
    assert!(
        sessions
            .iter()
            .all(|session| AsRef::<str>::as_ref(session.session_id()).starts_with('$')),
        "every hydrated session carries its tmux identity",
    );

    // tmux emits nothing for a session it has never attached. Treating
    // that as a required field made every `new-session -d` listing fail
    // to hydrate, which is the ordinary scripted and CI case.
    assert!(
        sessions
            .iter()
            .all(|session| session.session_last_attached() == Availability::Absent),
        "a never-attached session reports an absent last-attach time",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "test-support")]
async fn projection_stdout(server: &crate::Server, command: Command) -> String {
    let result = server
        .cmd(command)
        .await
        .expect("tmux scalar query executes");
    assert!(result.success(), "tmux scalar query succeeds");
    assert!(result.stderr().is_empty(), "tmux scalar stderr is empty");
    result
        .stdout_utf8()
        .expect("tmux scalar output is strict UTF-8")
        .to_owned()
}

#[cfg(feature = "test-support")]
fn projection_uses_post_3_6_semantics(version: &TmuxVersion) -> bool {
    match version.release() {
        Some(release) => *release >= ReleaseVersion::new(3, 6, ReleaseSuffix::FINAL),
        None => true,
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated fixture proves the repeated-winlink scalar shape end to end"
)]
async fn real_tmux_compat_projection_repeated_winlinks_preserve_rows_and_raw_aggregates() {
    let (guard, executable, explicit) = projection_test_server().await;
    let server = guard.server();
    if let Some(expected) = explicit {
        assert!(
            executable == expected,
            "the selected executable matches LIBTMUX_TEST_TMUX",
        );
    }
    assert!(
        server.tmux_executable() == executable.as_os_str(),
        "the Server retains the selected tmux executable",
    );

    projection_run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg("dup")
            .arg("-n")
            .arg("repeated")
            .arg("sleep 120"),
    )
    .await;
    projection_run(
        server,
        Command::new("move-window")
            .arg("-s")
            .arg("dup:0")
            .arg("-t")
            .arg("dup:1"),
    )
    .await;
    projection_run(
        server,
        Command::new("new-window")
            .arg("-d")
            .arg("-t")
            .arg("dup:2")
            .arg("-n")
            .arg("unrelated")
            .arg("sleep 120"),
    )
    .await;
    projection_run(
        server,
        Command::new("link-window")
            .arg("-s")
            .arg("dup:1")
            .arg("-t")
            .arg("dup:5"),
    )
    .await;

    let repeated_session_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("dup")
            .arg("#{session_id}"),
    )
    .await
    .trim()
    .parse::<SessionId>()
    .expect("the repeated-link Session ID is valid");
    let repeated_window_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("dup:1")
            .arg("#{window_id}"),
    )
    .await
    .trim()
    .parse::<WindowId>()
    .expect("the repeated Window ID is valid");
    let repeated_pane_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("dup:1.0")
            .arg("#{pane_id}"),
    )
    .await
    .trim()
    .parse::<PaneId>()
    .expect("the repeated Pane ID is valid");
    let version = server
        .capabilities()
        .await
        .expect("tmux capabilities are detected")
        .tmux_version();
    let expected_count = if projection_uses_post_3_6_semantics(version) {
        1
    } else {
        2
    };
    let window_plan = window_projection_plan(version)
        .ok()
        .expect("the detected Window projection plan is valid");
    let window_result = server
        .cmd(
            Command::new("list-windows")
                .arg("-t")
                .arg("dup")
                .arg("-F")
                .arg(window_plan.template()),
        )
        .await
        .expect("the Window projection listing executes");
    assert!(
        window_result.success(),
        "the Window projection listing succeeds"
    );
    assert!(
        window_result.stderr().is_empty(),
        "the Window projection stderr is empty"
    );
    let windows = hydrate_window_projections_from_stdout(
        server.identity(),
        &window_plan,
        window_result.stdout(),
    )
    .ok()
    .expect("the Window projection stdout parses and hydrates");
    let repeated_windows: Vec<_> = windows
        .iter()
        .filter(|projection| projection.window.window_id.as_ref() == repeated_window_id.as_ref())
        .collect();
    assert_eq!(repeated_windows.len(), 2);
    assert_eq!(
        repeated_windows
            .iter()
            .map(|projection| projection.link.identity.window_index())
            .collect::<Vec<_>>(),
        [1, 5],
    );
    for projection in &repeated_windows {
        assert_eq!(
            projection.link.identity.server_identity(),
            server.identity()
        );
        assert_eq!(projection.link.identity.session_id(), &repeated_session_id);
        assert_eq!(projection.link.identity.window_id(), &repeated_window_id);
        assert_eq!(&projection.window.window_id, &repeated_window_id);
        assert_eq!(projection.window_linked_sessions, expected_count);
        assert_eq!(
            projection.window_linked_sessions_list.as_bytes(),
            b"dup,dup"
        );
    }

    let pane_plan = pane_projection_plan(version)
        .ok()
        .expect("the detected Pane projection plan is valid");
    let pane_result = server
        .cmd(
            Command::new("list-panes")
                .arg("-a")
                .arg("-F")
                .arg(pane_plan.template()),
        )
        .await
        .expect("the Pane projection listing executes");
    assert!(
        pane_result.success(),
        "the Pane projection listing succeeds"
    );
    assert!(
        pane_result.stderr().is_empty(),
        "the Pane projection stderr is empty"
    );
    let panes =
        hydrate_pane_projections_from_stdout(server.identity(), &pane_plan, pane_result.stdout())
            .ok()
            .expect("the Pane projection stdout parses and hydrates");
    let repeated_panes: Vec<_> = panes
        .iter()
        .filter(|projection| projection.pane.pane_id.as_ref() == repeated_pane_id.as_ref())
        .collect();
    assert_eq!(repeated_panes.len(), 2);
    assert_eq!(
        repeated_panes
            .iter()
            .map(|projection| projection.link_identity.window_index())
            .collect::<Vec<_>>(),
        [1, 5],
    );
    for projection in &repeated_panes {
        assert_eq!(&projection.pane.pane_id, &repeated_pane_id);
        assert_eq!(
            projection.link_identity.server_identity(),
            server.identity()
        );
        assert_eq!(projection.link_identity.session_id(), &repeated_session_id);
        assert_eq!(projection.link_identity.window_id(), &repeated_window_id);
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated fixture proves grouped aggregate semantics end to end"
)]
async fn real_tmux_compat_aggregate_grouped_window_linked_tracks_release_semantics() {
    let (guard, executable, explicit) = projection_test_server().await;
    let server = guard.server();
    if let Some(expected) = explicit {
        assert!(
            executable == expected,
            "the selected executable matches LIBTMUX_TEST_TMUX",
        );
    }
    assert!(
        server.tmux_executable() == executable.as_os_str(),
        "the Server retains the selected tmux executable",
    );

    projection_run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg("group-a")
            .arg("-n")
            .arg("shared")
            .arg("sleep 120"),
    )
    .await;
    projection_run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg("group-b")
            .arg("-t")
            .arg("group-a"),
    )
    .await;

    let first_session_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("group-a")
            .arg("#{session_id}"),
    )
    .await
    .trim()
    .parse::<SessionId>()
    .expect("the first group Session ID is valid");
    let second_session_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("group-b")
            .arg("#{session_id}"),
    )
    .await
    .trim()
    .parse::<SessionId>()
    .expect("the second group Session ID is valid");
    let shared_window_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("group-a:0")
            .arg("#{window_id}"),
    )
    .await
    .trim()
    .parse::<WindowId>()
    .expect("the shared Window ID is valid");
    let version = server
        .capabilities()
        .await
        .expect("tmux capabilities are detected")
        .tmux_version();
    let expected_linked = projection_uses_post_3_6_semantics(version);
    let window_plan = window_projection_plan(version)
        .ok()
        .expect("the detected Window projection plan is valid");
    let result = server
        .cmd(
            Command::new("list-windows")
                .arg("-a")
                .arg("-F")
                .arg(window_plan.template()),
        )
        .await
        .expect("the grouped Window projection listing executes");
    assert!(result.success(), "the grouped Window listing succeeds");
    assert!(
        result.stderr().is_empty(),
        "the grouped Window stderr is empty"
    );
    let windows =
        hydrate_window_projections_from_stdout(server.identity(), &window_plan, result.stdout())
            .ok()
            .expect("the grouped Window stdout parses and hydrates");
    let grouped_rows: Vec<_> = windows
        .iter()
        .filter(|projection| {
            projection.window.window_id.as_ref() == shared_window_id.as_ref()
                && (projection.link.identity.session_id() == &first_session_id
                    || projection.link.identity.session_id() == &second_session_id)
        })
        .collect();

    assert_eq!(grouped_rows.len(), 2);
    let mut actual_session_ids = grouped_rows
        .iter()
        .map(|projection| projection.link.identity.session_id().clone())
        .collect::<Vec<_>>();
    actual_session_ids.sort();
    let mut expected_session_ids = vec![first_session_id, second_session_id];
    expected_session_ids.sort();
    assert_eq!(actual_session_ids, expected_session_ids);
    for projection in grouped_rows {
        assert_eq!(
            projection.link.identity.server_identity(),
            server.identity()
        );
        assert_eq!(projection.link.identity.window_id(), &shared_window_id);
        assert_eq!(&projection.window.window_id, &shared_window_id);
        assert_eq!(projection.link.window_linked, expected_linked);
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}
