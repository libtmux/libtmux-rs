use std::fmt;
use std::ops::Range;

use super::plan::{FormatPlan, TransportDialect};
use super::text::TmuxText;
use super::{DecoderKind, FormatDescriptor, ListProfile};

/// Bytes that `#{q:}` prefixes with a backslash.
///
/// This is tmux's `format_quote_shell` set. None of these bytes is an octal
/// digit or one of the letters `vis` emits, so the two escaping layers below
/// compose into one unambiguous grammar.
pub(super) const QUOTE_SHELL_SPECIALS: &[u8] = b"|&;<>()$`\\\"'*?[# =%";

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl FormatPlan {
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
    pub(super) const fn empty_plan() -> Self {
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
    pub(super) const fn plan(
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
