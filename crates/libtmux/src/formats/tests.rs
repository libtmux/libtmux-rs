#![allow(clippy::err_expect, clippy::expect_used, clippy::ok_expect)]

#[cfg(feature = "test-support")]
use super::decode_text;
use super::{
    CLIENT_INFO_DESCRIPTORS, CLIENT_INFO_SUPPLEMENTS, CLIENT_NAME, DecoderKind, EmptyPolicy,
    FormatCodecError, FormatCodecErrorKind, FormatCodecPhase, FormatDescriptor, FormatPlan,
    GROUPED_CATALOG, InfoPlacement, ListProfile, PANE_ID, PANE_INFO_DESCRIPTORS,
    PANE_INFO_SUPPLEMENTS, ParsedRow, ParsedSlot, PlanFieldState, PlanPurpose, PlanVersion,
    ProfileSet, QUOTE_SHELL_SPECIALS, RequiredContext, SESSION_ID, SESSION_INFO_DESCRIPTORS,
    SESSION_INFO_SUPPLEMENTS, SemanticOwner, TransportDialect, WINDOW_ID, WINDOW_INFO_DESCRIPTORS,
    WINDOW_INFO_SUPPLEMENTS, for_profile_selection_test,
};
#[cfg(feature = "test-support")]
use crate::Command;
#[cfg(feature = "test-support")]
use crate::test::TestServer;
use crate::{ReleaseSuffix, ReleaseVersion, TmuxVersion};
use static_assertions::assert_impl_all;

static FIRST: FormatDescriptor = FormatDescriptor::for_codec_test("first", DecoderKind::Text);
static SECOND: FormatDescriptor = FormatDescriptor::for_codec_test("second", DecoderKind::Ascii);
static POST_BASELINE: FormatDescriptor = FormatDescriptor {
    name: "post_baseline",
    owner: SemanticOwner::Session,
    required_context: RequiredContext::Session,
    profiles: ProfileSet::all(),
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
    0x7c, 0x26, 0x3b, 0x3c, 0x3e, 0x28, 0x29, 0x24, 0x60, 0x5c, 0x22, 0x27, 0x2a, 0x3f, 0x5b, 0x23,
    0x20, 0x3d, 0x25,
];
const SHORT_SENTINEL: &str = "zot-private";
const LONG_SENTINEL: &str = "quartz-private-payload-with-a-distinct-and-deliberately-long-shape";
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
    0x3a, 0x0a, 0x5c, 0x25, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x5c, 0x32, 0x30, 0x30, 0x5c, 0x33, 0x37,
    0x37, 0x25, 0x0a,
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
    assert_impl_all!(ProfileSet: Clone, Copy, std::fmt::Debug, Eq, PartialEq);
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
    let error_expected: fn(&FormatCodecError) -> Option<DecoderKind> = FormatCodecError::expected;
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
    ) -> Result<Vec<ParsedRow>, FormatCodecError> = FormatPlan::parse_rows;

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
    const DOCUMENT: &str = include_str!("../../docs/parity.md");
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
    assert_eq!(rows.len(), 179);
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

fn profiles_token(profiles: ProfileSet) -> &'static str {
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
    } else if minimum == ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL) {
        "3.4"
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
    catalog()
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

fn catalog() -> Vec<&'static FormatDescriptor> {
    let mut catalog = GROUPED_CATALOG.to_vec();
    catalog.sort_unstable_by_key(|descriptor| descriptor.name());
    catalog
}

#[test]
fn format_catalog_checked_parity_is_an_exact_sorted_bijection() {
    let rows = checked_catalog_rows();
    let catalog = catalog();
    assert_eq!(catalog.len(), 179);
    assert_eq!(GROUPED_CATALOG.len(), 179);

    let mut names = std::collections::BTreeSet::new();
    let mut pointers = std::collections::BTreeSet::new();
    for (index, (row, descriptor)) in rows.iter().zip(&catalog).enumerate() {
        assert!(names.insert(descriptor.name()));
        assert!(pointers.insert(descriptor_address(descriptor)));
        if let Some(previous) = index.checked_sub(1) {
            assert!(catalog[previous].name() < descriptor.name());
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
    assert_eq!(grouped_pointers.len(), 179);
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
        std::collections::BTreeMap::from([("3.2a", 159), ("3.3", 8), ("3.4", 1), ("3.7", 11)])
    );
    assert_eq!(
        count_tokens(rows.iter().map(|row| row.profiles)),
        std::collections::BTreeMap::from([("all", 135), ("clients", 27), ("none", 17)])
    );
    assert_eq!(
        count_tokens(rows.iter().map(|row| row.empty)),
        std::collections::BTreeMap::from([("absent", 30), ("available", 28), ("required", 121),])
    );
    assert_eq!(
        count_tokens(rows.iter().map(|row| row.placement)),
        std::collections::BTreeMap::from([
            ("catalog-only", 68),
            ("client-info", 22),
            ("pane-info", 69),
            ("session-info", 9),
            ("window-info", 11),
        ])
    );
    assert_eq!(
        count_tokens(rows.iter().map(|row| row.decoder)),
        std::collections::BTreeMap::from([
            ("bool", 50),
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
            ("pane", 70),
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
    let fields_3_4 = rows
        .iter()
        .filter(|row| row.minimum == "3.4")
        .map(|row| row.field)
        .collect::<Vec<_>>();
    assert_eq!(fields_3_4, ["pane_unseen_changes"]);
    assert!(rows.iter().all(|row| !matches!(row.minimum, "3.5" | "3.6")));
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
        (b"tmux 3.6\n".as_slice(), [9, 11, 58, 22]),
        (b"tmux 3.7\n".as_slice(), [9, 11, 69, 22]),
        (b"tmux 3.8\n".as_slice(), [9, 11, 69, 22]),
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
    let version_3_4 = parse_version(b"tmux 3.4\n");
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

    for descriptor in catalog()
        .into_iter()
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
        } else if descriptor.minimum_release() == ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL) {
            (&version_3_2a, &version_3_4)
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

    for descriptor in catalog().into_iter().filter(|descriptor| {
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
        assert_eq!(unproven, 17);
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

    let plan = FormatPlan::for_descriptors(ListProfile::Sessions, &version, &[windows, activity])
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
    let executable =
        std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| std::ffi::OsString::from("tmux"));
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
        0x3a, 0x0a, 0x5c, 0x25, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x5c, 0x32, 0x30, 0x30, 0x5c, 0x33,
        0x37, 0x37, 0x25, 0x0a,
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

/// Record which format names each listing asks tmux for.
///
/// The supplement arrays are macro-generated, so the answer exists in the
/// source and cannot be read out of it: the only way to learn what a
/// listing requests has been to intercept the `-F` string of a running
/// command. This writes the answer down and fails when it drifts.
///
/// Regenerate with `just list-profiles`.
#[test]
fn each_listing_records_the_fields_it_requests() {
    use super::{FormatPlan, ListProfile};
    use crate::snapshot::{pane_projection_plan, window_projection_plan};
    use crate::version::TmuxVersion;
    use std::fmt::Write as _;

    let version = TmuxVersion::parse_output(b"tmux 3.7\n").expect("a supported release");
    let mut recorded = String::from(
        "# Which format names each listing asks tmux for, at the newest\n\
         # release the lanes build. The supplement arrays this comes from\n\
         # are macro-generated; without this file the only way to learn a\n\
         # listing's fields is to intercept its `-F` string.\n\
         #\n\
         # Recorded from the plan each listing builds, which is not the\n\
         # same for all four: sessions and clients plan from the profile,\n\
         # windows and panes from a requested set that chains a projection\n\
         # suffix onto it. What this cannot see is a listing that changes\n\
         # which plan it builds -- the check would then hold this file\n\
         # consistent with itself rather than with the `-F` string. The\n\
         # cross-check is to intercept the argv and count.\n\
         #\n\
         # Regenerate with `just list-profiles`.\n\n",
    );
    // Built the way each listing builds it, not the way they look alike.
    // `sessions` and `clients` plan from the profile; `windows` and
    // `panes` plan from a requested set that chains a projection suffix
    // onto the supplements, and recording the profile plan for those two
    // documented a template nothing sends.
    let plans = [
        (
            "sessions",
            FormatPlan::for_profile(ListProfile::Sessions, &version),
        ),
        (
            "windows",
            window_projection_plan(&version).expect("a supported release plans windows"),
        ),
        (
            "panes",
            pane_projection_plan(&version).expect("a supported release plans panes"),
        ),
        (
            "clients",
            FormatPlan::for_profile(ListProfile::Clients, &version),
        ),
    ];
    for (label, plan) in plans {
        let mut names: Vec<&str> = plan
            .descriptors_for_test()
            .iter()
            .map(|descriptor| descriptor.name())
            .collect();
        names.sort_unstable();
        let _ = writeln!(recorded, "{label} ({} fields)", names.len());
        for name in names {
            let _ = writeln!(recorded, "  {name}");
        }
        recorded.push('\n');
    }

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/list-profiles.txt");
    if std::env::var_os("LIBTMUX_RERECORD").is_some() {
        std::fs::write(path, &recorded).expect("the ledger is writable");
        return;
    }
    let stored = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        stored, recorded,
        "the fields a listing requests changed; run `just list-profiles`",
    );
}
