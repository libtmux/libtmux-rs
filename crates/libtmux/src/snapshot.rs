//! Private snapshot decoder boundaries.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

#[cfg(test)]
use crate::formats::{DecoderKind, decode_ascii};
use crate::formats::{
    EmptyPolicy, FormatCodecError, FormatCodecErrorKind, FormatDescriptor, FormatPlan,
    InfoPlacement, ListProfile, PANE_INFO_SUPPLEMENTS, ParsedRow, ParsedSlot, PlanFieldState,
    PlanPurpose, TmuxText, WINDOW_INFO_SUPPLEMENTS, decode_text, format_catalog,
};
#[cfg(feature = "query")]
use crate::query::{
    BoolField, EnumField, FilterEnum, FilterExpressionError, FilterSchema, Filterable,
    IntegerField, TextField,
};
use crate::target::WindowLinkIdentity;
use crate::{PaneId, ServerIdentity, SessionId, TmuxVersion, WindowId};

/// Availability evidence retained for a modeled snapshot field.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Availability<T> {
    /// The detected numbered release predates the field.
    Unsupported,
    /// A development build provides no numbered availability proof.
    Unproven,
    /// Tmux emitted an empty value for a conditionally available field.
    Absent,
    /// Tmux emitted a decoded value, including preserved empty text.
    Available(T),
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl<T> Availability<T> {
    /// Borrow the contained value without cloning the evidence.
    pub(crate) const fn as_ref(&self) -> Availability<&T> {
        match self {
            Self::Unsupported => Availability::Unsupported,
            Self::Unproven => Availability::Unproven,
            Self::Absent => Availability::Absent,
            Self::Available(value) => Availability::Available(value),
        }
    }

    /// Discard the reason a value is missing and keep only the value.
    ///
    /// Callers that treat every unavailable state alike use this. Callers that
    /// must distinguish an unsupported release from a genuinely absent value
    /// match on the evidence instead.
    pub(crate) fn available(self) -> Option<T> {
        match self {
            Self::Available(value) => Some(value),
            Self::Unsupported | Self::Unproven | Self::Absent => None,
        }
    }

    /// Report whether tmux emitted a decoded value.
    pub(crate) const fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl<T: Copy> Availability<&T> {
    /// Copy a borrowed scalar out of its evidence.
    pub(crate) const fn copied(self) -> Availability<T> {
        match self {
            Self::Unsupported => Availability::Unsupported,
            Self::Unproven => Availability::Unproven,
            Self::Absent => Availability::Absent,
            Self::Available(value) => Availability::Available(*value),
        }
    }
}

/// Closed progress-state vocabulary tmux emits for a pane's progress bar.
///
/// This is public because [`crate::PaneFields`] exposes a typed handle for the
/// field, so callers name these variants when filtering.
///
/// # Examples
///
/// ```
/// # #[cfg(feature = "query")] {
/// use libtmux::query::Filterable as _;
/// use libtmux::PaneProgressState;
///
/// // Reached as a filter field rather than a getter: tmux only reports it from
/// // 3.7, so asking for it is a query a caller opts into.
/// let fields = libtmux::Pane::filter_fields();
/// let stuck = fields
///     .pane_pb_state
///     .is_in([PaneProgressState::Error, PaneProgressState::Paused]);
/// # let _ = stuck;
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PaneProgressState {
    /// Progress display is hidden.
    Hidden,
    /// Progress display is normal.
    Normal,
    /// Progress display reports an error.
    Error,
    /// Progress is indeterminate.
    Indeterminate,
    /// Progress is paused.
    Paused,
}

#[cfg(feature = "query")]
impl FilterEnum for PaneProgressState {
    const FILTER_VARIANTS: &'static [&'static str] =
        &["hidden", "normal", "error", "indeterminate", "paused"];

    fn filter_name(&self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Normal => "normal",
            Self::Error => "error",
            Self::Indeterminate => "indeterminate",
            Self::Paused => "paused",
        }
    }
}

/// Selected slot or retained unavailability for one planned field.
enum PlannedSlot<'row> {
    Unsupported,
    Unproven,
    Selected(ParsedSlot<'row>),
}

/// Single-pass association between a plan and one parsed row.
struct RowHydrator<'plan, 'row> {
    plan: &'plan FormatPlan,
    row: &'row ParsedRow,
    planned_index: usize,
    slot_index: usize,
}

impl<'plan, 'row> RowHydrator<'plan, 'row> {
    fn new(plan: &'plan FormatPlan, row: &'row ParsedRow) -> Self {
        Self {
            plan,
            row,
            planned_index: 0,
            slot_index: 0,
        }
    }

    fn next(
        &mut self,
        expected_descriptor: &'static FormatDescriptor,
    ) -> Result<PlannedSlot<'row>, FormatCodecError> {
        let Some(planned) = self.plan.planned().get(self.planned_index) else {
            return Err(FormatCodecError::row_mismatch(
                self.row.row(),
                Some(self.slot_index),
                Some(expected_descriptor.name()),
                None,
            ));
        };
        self.planned_index += 1;

        if !std::ptr::eq(planned.descriptor, expected_descriptor) {
            let offset = self.row.slot(self.slot_index).map(|slot| slot.raw_start());
            return Err(FormatCodecError::row_mismatch(
                self.row.row(),
                Some(self.slot_index),
                Some(expected_descriptor.name()),
                offset,
            ));
        }

        match planned.state {
            PlanFieldState::Unsupported => Ok(PlannedSlot::Unsupported),
            PlanFieldState::Unproven => Ok(PlannedSlot::Unproven),
            PlanFieldState::Selected { slot } => {
                let Some(actual) = self.row.slot(self.slot_index) else {
                    return Err(FormatCodecError::row_mismatch(
                        self.row.row(),
                        Some(self.slot_index),
                        Some(planned.descriptor.name()),
                        None,
                    ));
                };
                if slot != self.slot_index || !std::ptr::eq(actual.descriptor(), planned.descriptor)
                {
                    return Err(FormatCodecError::row_mismatch(
                        self.row.row(),
                        Some(actual.field()),
                        Some(planned.descriptor.name()),
                        Some(actual.raw_start()),
                    ));
                }
                self.slot_index += 1;
                Ok(PlannedSlot::Selected(actual))
            }
        }
    }

    fn finish(self) -> Result<(), FormatCodecError> {
        if let Some(planned) = self.plan.planned().get(self.planned_index) {
            return Err(FormatCodecError::row_mismatch(
                self.row.row(),
                Some(self.slot_index),
                Some(planned.descriptor.name()),
                None,
            ));
        }
        if let Some(actual) = self.row.slot(self.slot_index) {
            return Err(FormatCodecError::row_mismatch(
                self.row.row(),
                Some(actual.field()),
                Some(actual.descriptor().name()),
                Some(actual.raw_start()),
            ));
        }
        Ok(())
    }

    fn decode<T, F>(
        &mut self,
        descriptor: &'static FormatDescriptor,
        decoder: F,
    ) -> Result<Availability<T>, FormatCodecError>
    where
        F: FnOnce(ParsedSlot<'row>) -> Result<T, FormatCodecError>,
    {
        match self.next(descriptor)? {
            PlannedSlot::Unsupported => Ok(Availability::Unsupported),
            PlannedSlot::Unproven => Ok(Availability::Unproven),
            PlannedSlot::Selected(slot) if slot.as_bytes().is_empty() => {
                match descriptor.empty_policy() {
                    EmptyPolicy::Absent => Ok(Availability::Absent),
                    EmptyPolicy::Required => Err(FormatCodecError::typed(
                        FormatCodecErrorKind::RequiredFieldEmpty,
                        &slot,
                    )),
                    EmptyPolicy::Available => decoder(slot).map(Availability::Available),
                }
            }
            PlannedSlot::Selected(slot) => decoder(slot).map(Availability::Available),
        }
    }

    fn decode_infallible<T, F>(
        &mut self,
        descriptor: &'static FormatDescriptor,
        decoder: F,
    ) -> Result<Availability<T>, FormatCodecError>
    where
        F: FnOnce(ParsedSlot<'row>) -> T,
    {
        match self.next(descriptor)? {
            PlannedSlot::Unsupported => Ok(Availability::Unsupported),
            PlannedSlot::Unproven => Ok(Availability::Unproven),
            PlannedSlot::Selected(slot) if slot.as_bytes().is_empty() => {
                match descriptor.empty_policy() {
                    EmptyPolicy::Absent => Ok(Availability::Absent),
                    EmptyPolicy::Required => Err(FormatCodecError::typed(
                        FormatCodecErrorKind::RequiredFieldEmpty,
                        &slot,
                    )),
                    EmptyPolicy::Available => Ok(Availability::Available(decoder(slot))),
                }
            }
            PlannedSlot::Selected(slot) => Ok(Availability::Available(decoder(slot))),
        }
    }
}

fn invalid_value(slot: &ParsedSlot<'_>) -> FormatCodecError {
    FormatCodecError::typed(FormatCodecErrorKind::InvalidValue, slot)
}

fn unsigned_text(bytes: &[u8]) -> Option<&str> {
    if !bytes.is_ascii() {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text == "0" {
        return Some(text);
    }
    if matches!(bytes.first(), Some(b'1'..=b'9')) && bytes.iter().all(u8::is_ascii_digit) {
        Some(text)
    } else {
        None
    }
}

fn signed_text(bytes: &[u8]) -> Option<&str> {
    if !bytes.is_ascii() {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if text == "0" {
        return Some(text);
    }
    let digits = match text.strip_prefix('-') {
        Some(digits) => digits,
        None => text,
    };
    if matches!(digits.as_bytes().first(), Some(b'1'..=b'9'))
        && digits.as_bytes().iter().all(u8::is_ascii_digit)
    {
        Some(text)
    } else {
        None
    }
}

fn decode_owned_text(slot: ParsedSlot<'_>) -> TmuxText {
    decode_text(slot)
}

fn decode_bool(slot: ParsedSlot<'_>) -> Result<bool, FormatCodecError> {
    match slot.as_bytes() {
        b"0" => Ok(false),
        b"1" => Ok(true),
        _ => Err(invalid_value(&slot)),
    }
}

fn decode_u8(slot: ParsedSlot<'_>) -> Result<u8, FormatCodecError> {
    unsigned_text(slot.as_bytes())
        .and_then(|text| text.parse::<u8>().ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_u32(slot: ParsedSlot<'_>) -> Result<u32, FormatCodecError> {
    unsigned_text(slot.as_bytes())
        .and_then(|text| text.parse::<u32>().ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_u64(slot: ParsedSlot<'_>) -> Result<u64, FormatCodecError> {
    unsigned_text(slot.as_bytes())
        .and_then(|text| text.parse::<u64>().ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_i32(slot: ParsedSlot<'_>) -> Result<i32, FormatCodecError> {
    signed_text(slot.as_bytes())
        .and_then(|text| text.parse::<i32>().ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_timestamp(slot: ParsedSlot<'_>) -> Result<i64, FormatCodecError> {
    signed_text(slot.as_bytes())
        .and_then(|text| text.parse::<i64>().ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_session_id(slot: ParsedSlot<'_>) -> Result<SessionId, FormatCodecError> {
    if !slot.as_bytes().is_ascii() {
        return Err(invalid_value(&slot));
    }
    std::str::from_utf8(slot.as_bytes())
        .ok()
        .and_then(|text| SessionId::from_str(text).ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_window_id(slot: ParsedSlot<'_>) -> Result<WindowId, FormatCodecError> {
    if !slot.as_bytes().is_ascii() {
        return Err(invalid_value(&slot));
    }
    std::str::from_utf8(slot.as_bytes())
        .ok()
        .and_then(|text| WindowId::from_str(text).ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_pane_id(slot: ParsedSlot<'_>) -> Result<PaneId, FormatCodecError> {
    if !slot.as_bytes().is_ascii() {
        return Err(invalid_value(&slot));
    }
    std::str::from_utf8(slot.as_bytes())
        .ok()
        .and_then(|text| PaneId::from_str(text).ok())
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_pane_progress(slot: ParsedSlot<'_>) -> Result<u8, FormatCodecError> {
    decode_u8(slot)
        .ok()
        .filter(|progress| *progress <= 100)
        .ok_or_else(|| invalid_value(&slot))
}

fn decode_pane_progress_state(slot: ParsedSlot<'_>) -> Result<PaneProgressState, FormatCodecError> {
    match slot.as_bytes() {
        b"hidden" => Ok(PaneProgressState::Hidden),
        b"normal" => Ok(PaneProgressState::Normal),
        b"error" => Ok(PaneProgressState::Error),
        b"indeterminate" => Ok(PaneProgressState::Indeterminate),
        b"paused" => Ok(PaneProgressState::Paused),
        _ => Err(invalid_value(&slot)),
    }
}

#[cfg(test)]
fn decode_ascii_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_ascii(slot).map(|_| ())
}

#[cfg(test)]
fn decode_text_marker(slot: ParsedSlot<'_>) {
    let _ = decode_text(slot);
}

#[cfg(test)]
fn decode_bool_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_bool(slot).map(|_| ())
}

#[cfg(test)]
fn decode_u8_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_u8(slot).map(|_| ())
}

#[cfg(test)]
fn decode_u32_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_u32(slot).map(|_| ())
}

#[cfg(test)]
fn decode_u64_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_u64(slot).map(|_| ())
}

#[cfg(test)]
fn decode_i32_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_i32(slot).map(|_| ())
}

#[cfg(test)]
fn decode_timestamp_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_timestamp(slot).map(|_| ())
}

#[cfg(test)]
fn decode_session_id_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_session_id(slot).map(|_| ())
}

#[cfg(test)]
fn decode_window_id_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_window_id(slot).map(|_| ())
}

#[cfg(test)]
fn decode_pane_id_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_pane_id(slot).map(|_| ())
}

#[cfg(test)]
fn decode_pane_progress_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_pane_progress(slot).map(|_| ())
}

#[cfg(test)]
fn decode_pane_progress_state_marker(slot: ParsedSlot<'_>) -> Result<(), FormatCodecError> {
    decode_pane_progress_state(slot).map(|_| ())
}

#[cfg(test)]
fn decode_marker(
    hydrator: &mut RowHydrator<'_, '_>,
    descriptor: &'static FormatDescriptor,
) -> Result<(), FormatCodecError> {
    let result = match descriptor.decoder() {
        DecoderKind::Ascii => hydrator.decode(descriptor, decode_ascii_marker),
        DecoderKind::Text => hydrator.decode_infallible(descriptor, decode_text_marker),
        DecoderKind::Bool => hydrator.decode(descriptor, decode_bool_marker),
        DecoderKind::U8 => hydrator.decode(descriptor, decode_u8_marker),
        DecoderKind::U32 => hydrator.decode(descriptor, decode_u32_marker),
        DecoderKind::U64 => hydrator.decode(descriptor, decode_u64_marker),
        DecoderKind::I32 => hydrator.decode(descriptor, decode_i32_marker),
        DecoderKind::Timestamp => hydrator.decode(descriptor, decode_timestamp_marker),
        DecoderKind::SessionId => hydrator.decode(descriptor, decode_session_id_marker),
        DecoderKind::WindowId => hydrator.decode(descriptor, decode_window_id_marker),
        DecoderKind::PaneId => hydrator.decode(descriptor, decode_pane_id_marker),
        DecoderKind::PaneProgress => hydrator.decode(descriptor, decode_pane_progress_marker),
        DecoderKind::PaneProgressState => {
            hydrator.decode(descriptor, decode_pane_progress_state_marker)
        }
    }?;
    let _ = result;
    Ok(())
}

fn required_identity<T>(
    availability: Availability<T>,
    row: usize,
    descriptor: &'static FormatDescriptor,
) -> Result<T, FormatCodecError> {
    match availability {
        Availability::Available(value) => Ok(value),
        Availability::Unsupported | Availability::Unproven | Availability::Absent => Err(
            FormatCodecError::row_mismatch(row, Some(0), Some(descriptor.name()), None),
        ),
    }
}

macro_rules! snapshot_type {
    (Text) => {
        TmuxText
    };
    (Bool) => {
        bool
    };
    (U8) => {
        u8
    };
    (U32) => {
        u32
    };
    (U64) => {
        u64
    };
    (I32) => {
        i32
    };
    (Timestamp) => {
        i64
    };
    (SessionId) => {
        SessionId
    };
    (WindowId) => {
        WindowId
    };
    (PaneId) => {
        PaneId
    };
    (PaneProgress) => {
        u8
    };
    (PaneProgressState) => {
        PaneProgressState
    };
}

macro_rules! decode_catalog_field {
    ($hydrator:expr, $descriptor:expr, Text) => {
        $hydrator.decode_infallible($descriptor, decode_owned_text)
    };
    ($hydrator:expr, $descriptor:expr, Bool) => {
        $hydrator.decode($descriptor, decode_bool)
    };
    ($hydrator:expr, $descriptor:expr, U8) => {
        $hydrator.decode($descriptor, decode_u8)
    };
    ($hydrator:expr, $descriptor:expr, U32) => {
        $hydrator.decode($descriptor, decode_u32)
    };
    ($hydrator:expr, $descriptor:expr, U64) => {
        $hydrator.decode($descriptor, decode_u64)
    };
    ($hydrator:expr, $descriptor:expr, I32) => {
        $hydrator.decode($descriptor, decode_i32)
    };
    ($hydrator:expr, $descriptor:expr, Timestamp) => {
        $hydrator.decode($descriptor, decode_timestamp)
    };
    ($hydrator:expr, $descriptor:expr, SessionId) => {
        $hydrator.decode($descriptor, decode_session_id)
    };
    ($hydrator:expr, $descriptor:expr, WindowId) => {
        $hydrator.decode($descriptor, decode_window_id)
    };
    ($hydrator:expr, $descriptor:expr, PaneId) => {
        $hydrator.decode($descriptor, decode_pane_id)
    };
    ($hydrator:expr, $descriptor:expr, PaneProgress) => {
        $hydrator.decode($descriptor, decode_pane_progress)
    };
    ($hydrator:expr, $descriptor:expr, PaneProgressState) => {
        $hydrator.decode($descriptor, decode_pane_progress_state)
    };
}

#[cfg(feature = "query")]
macro_rules! filter_field_type {
    ($info:ident, Text) => { TextField<$info> };
    ($info:ident, SessionId) => { TextField<$info> };
    ($info:ident, WindowId) => { TextField<$info> };
    ($info:ident, PaneId) => { TextField<$info> };
    ($info:ident, Bool) => { BoolField<$info> };
    ($info:ident, U8) => { IntegerField<$info, u8> };
    ($info:ident, U32) => { IntegerField<$info, u32> };
    ($info:ident, U64) => { IntegerField<$info, u64> };
    ($info:ident, I32) => { IntegerField<$info, i32> };
    ($info:ident, Timestamp) => { IntegerField<$info, i64> };
    ($info:ident, PaneProgress) => { IntegerField<$info, u8> };
    ($info:ident, PaneProgressState) => { EnumField<$info, PaneProgressState> };
}

#[cfg(feature = "query")]
macro_rules! filter_field_value {
    ($info:ident, $target:expr, $name:expr, Text) => {
        crate::query::__private::text_field::<$info>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, SessionId) => {
        crate::query::__private::text_field::<$info>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, WindowId) => {
        crate::query::__private::text_field::<$info>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, PaneId) => {
        crate::query::__private::text_field::<$info>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, Bool) => {
        crate::query::__private::bool_field::<$info>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, U8) => {
        crate::query::__private::integer_field::<$info, u8>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, U32) => {
        crate::query::__private::integer_field::<$info, u32>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, U64) => {
        crate::query::__private::integer_field::<$info, u64>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, I32) => {
        crate::query::__private::integer_field::<$info, i32>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, Timestamp) => {
        crate::query::__private::integer_field::<$info, i64>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, PaneProgress) => {
        crate::query::__private::integer_field::<$info, u8>($target, $name)
    };
    ($info:ident, $target:expr, $name:expr, PaneProgressState) => {
        crate::query::__private::enum_field::<$info, PaneProgressState>($target, $name)
    };
}

#[cfg(feature = "query")]
macro_rules! match_scalar {
    ($predicate:expr, $value:expr, Text) => {
        $predicate.matches_text($value.as_bytes())
    };
    ($predicate:expr, $value:expr, SessionId) => {
        $predicate.matches_text($value.as_ref().as_bytes())
    };
    ($predicate:expr, $value:expr, WindowId) => {
        $predicate.matches_text($value.as_ref().as_bytes())
    };
    ($predicate:expr, $value:expr, PaneId) => {
        $predicate.matches_text($value.as_ref().as_bytes())
    };
    ($predicate:expr, $value:expr, Bool) => {
        $predicate.matches_bool(*$value)
    };
    ($predicate:expr, $value:expr, U8) => {
        $predicate.matches_unsigned(u128::from(*$value))
    };
    ($predicate:expr, $value:expr, U32) => {
        $predicate.matches_unsigned(u128::from(*$value))
    };
    ($predicate:expr, $value:expr, U64) => {
        $predicate.matches_unsigned(u128::from(*$value))
    };
    ($predicate:expr, $value:expr, I32) => {
        $predicate.matches_signed(i128::from(*$value))
    };
    ($predicate:expr, $value:expr, Timestamp) => {
        $predicate.matches_signed(i128::from(*$value))
    };
    ($predicate:expr, $value:expr, PaneProgress) => {
        $predicate.matches_unsigned(u128::from(*$value))
    };
    ($predicate:expr, $value:expr, PaneProgressState) => {
        $predicate.matches_enum($value.filter_name())
    };
}

#[cfg(feature = "query")]
macro_rules! match_available {
    ($predicate:expr, $value:expr, $decoder:ident) => {
        match $value {
            Availability::Available(value) => match_scalar!($predicate, value, $decoder),
            Availability::Unsupported | Availability::Unproven | Availability::Absent => false,
        }
    };
}

#[cfg(feature = "query")]
macro_rules! validate_scalar {
    ($predicate:expr, Text) => {
        $predicate.validate_text()
    };
    ($predicate:expr, SessionId) => {
        $predicate.validate_text()
    };
    ($predicate:expr, WindowId) => {
        $predicate.validate_text()
    };
    ($predicate:expr, PaneId) => {
        $predicate.validate_text()
    };
    ($predicate:expr, Bool) => {
        $predicate.validate_bool()
    };
    ($predicate:expr, U8) => {
        $predicate.validate_integer(crate::query::__private::IntegerKind::U8)
    };
    ($predicate:expr, U32) => {
        $predicate.validate_integer(crate::query::__private::IntegerKind::U32)
    };
    ($predicate:expr, U64) => {
        $predicate.validate_integer(crate::query::__private::IntegerKind::U64)
    };
    ($predicate:expr, I32) => {
        $predicate.validate_integer(crate::query::__private::IntegerKind::I32)
    };
    ($predicate:expr, Timestamp) => {
        $predicate.validate_integer(crate::query::__private::IntegerKind::I64)
    };
    ($predicate:expr, PaneProgress) => {
        $predicate.validate_integer(crate::query::__private::IntegerKind::U8)
    };
    ($predicate:expr, PaneProgressState) => {
        $predicate.validate_enum(PaneProgressState::FILTER_VARIANTS)
    };
}

#[cfg(feature = "query")]
macro_rules! filter_schema_value {
    (Text) => {
        crate::query::__private::FilterValueSchema::Text
    };
    (SessionId) => {
        crate::query::__private::FilterValueSchema::Text
    };
    (WindowId) => {
        crate::query::__private::FilterValueSchema::Text
    };
    (PaneId) => {
        crate::query::__private::FilterValueSchema::Text
    };
    (Bool) => {
        crate::query::__private::FilterValueSchema::Bool
    };
    (U8) => {
        crate::query::__private::FilterValueSchema::Unsigned
    };
    (U32) => {
        crate::query::__private::FilterValueSchema::Unsigned
    };
    (U64) => {
        crate::query::__private::FilterValueSchema::Unsigned
    };
    (I32) => {
        crate::query::__private::FilterValueSchema::Signed
    };
    (Timestamp) => {
        crate::query::__private::FilterValueSchema::Signed
    };
    (PaneProgress) => {
        crate::query::__private::FilterValueSchema::Unsigned
    };
    (PaneProgressState) => {
        crate::query::__private::FilterValueSchema::Enum(PaneProgressState::FILTER_VARIANTS)
    };
}

/// The stored type for a field, given its version floor and empty policy.
///
/// A field at or below the supported floor with a `Required` or `Available`
/// policy cannot be absent: the codec already fails hydration on an empty
/// `Required` field, an empty `Available` field is a value, and only `Absent`
/// means missing. `classify_field` marks anything at the floor `Selected` for
/// numbered and development builds alike, so it is never `Unsupported` or
/// `Unproven` either. Such a field is stored flat.
macro_rules! stored_type {
    ($decoder:ident, V3_2A, Required) => { snapshot_type!($decoder) };
    ($decoder:ident, V3_2A, Available) => { snapshot_type!($decoder) };
    ($decoder:ident, $floor:ident, $empty:ident) => { Availability<snapshot_type!($decoder)> };
}

/// The borrowed return type matching [`stored_type`].
macro_rules! borrowed_type {
    ($decoder:ident, V3_2A, Required) => { &snapshot_type!($decoder) };
    ($decoder:ident, V3_2A, Available) => { &snapshot_type!($decoder) };
    ($decoder:ident, $floor:ident, $empty:ident) => { Availability<&snapshot_type!($decoder)> };
}

/// Borrow a stored field, matching [`stored_type`].
macro_rules! borrow_stored {
    ($value:expr, V3_2A, Required) => {
        &$value
    };
    ($value:expr, V3_2A, Available) => {
        &$value
    };
    ($value:expr, $floor:ident, $empty:ident) => {
        $value.as_ref()
    };
}

/// Decode a field into its stored shape, matching [`stored_type`].
///
/// The flat arms unwrap the availability. That cannot fail: the codec already
/// rejects an empty `Required` field before hydration reaches here, an empty
/// `Available` field is a value rather than absence, and a field at the
/// supported floor is never version-gated. The error path exists so the
/// unwrap is not a panic, not because it is reachable.
macro_rules! decode_stored {
    ($hydrator:expr, $descriptor:expr, $decoder:ident, V3_2A, Required) => {{
        let value = decode_catalog_field!($hydrator, $descriptor, $decoder)?;
        required_projection_value(value, $hydrator, $descriptor)
    }};
    ($hydrator:expr, $descriptor:expr, $decoder:ident, V3_2A, Available) => {{
        let value = decode_catalog_field!($hydrator, $descriptor, $decoder)?;
        required_projection_value(value, $hydrator, $descriptor)
    }};
    ($hydrator:expr, $descriptor:expr, $decoder:ident, $floor:ident, $empty:ident) => {
        decode_catalog_field!($hydrator, $descriptor, $decoder)
    };
}

/// Match a predicate against a stored field, matching [`stored_type`].
#[cfg(feature = "query")]
macro_rules! match_stored {
    ($predicate:expr, $value:expr, $decoder:ident, V3_2A, Required) => {
        match_scalar!($predicate, $value, $decoder)
    };
    ($predicate:expr, $value:expr, $decoder:ident, V3_2A, Available) => {
        match_scalar!($predicate, $value, $decoder)
    };
    ($predicate:expr, $value:expr, $decoder:ident, $floor:ident, $empty:ident) => {
        match_available!($predicate, $value, $decoder)
    };
}

macro_rules! define_snapshot_info {
    (
        $info:ident, $fields:ident, $handle:ident, $hydrate:ident, $hydrate_inner:ident, $target:literal,
        $placement:ident,
        ($baseline_static:ident, $baseline_field:ident, $baseline_name:literal, $baseline_owner:ident, $baseline_context:ident, $baseline_profiles:ident, $baseline_floor:ident, $baseline_decoder:ident, $baseline_empty:ident),
        [$(($static:ident, $field:ident, $name:literal, $owner:ident, $context:ident, $profiles:ident, $floor:ident, $decoder:ident, $empty:ident)),* $(,)?]
    ) => {
        #[allow(
            dead_code,
            reason = "modelled and tested; only a projection of it is hydrated today"
        )]
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub(crate) struct $info {
            $baseline_field: snapshot_type!($baseline_decoder),
            $($field: stored_type!($decoder, $floor, $empty)),*
        }

        #[allow(
            dead_code,
            reason = "modelled and tested; only a projection of it is hydrated today"
        )]
        impl $info {
            #[doc = concat!("Return the decoded `", $baseline_name, "` value.")]
            ///
            /// This field is the snapshot's identity, so it is always present
            /// on every supported release.
            pub(crate) const fn $baseline_field(&self) -> &snapshot_type!($baseline_decoder) {
                &self.$baseline_field
            }

            $(
                #[doc = concat!("Return the decoded `", $name, "` value.")]
                ///
                /// A field that tmux always reports on every supported release
                /// is returned flat. One that can be missing carries its
                /// evidence, so a release predating the field is separable
                /// from one that reported nothing.
                pub(crate) const fn $field(&self) -> borrowed_type!($decoder, $floor, $empty) {
                    borrow_stored!(self.$field, $floor, $empty)
                }
            )*
        }

        #[doc = concat!("Typed filter field handles for a `", $target, "`.")]
        ///
        /// The type parameter names what an expression built from these
        /// handles matches, so a listing returns the same type its
        /// expressions filter.
        ///
        /// Every field is named after the tmux format it reads, so an
        /// expression reads like the `-F` string it replaces. A field's type
        /// decides which operations exist, which is what makes a mismatched
        /// comparison a compile error rather than a predicate that is always
        /// false.
        ///
        /// # Examples
        ///
        #[doc = concat!(
            "```\n",
            "# #[cfg(feature = \"query\")] {\n",
            "use libtmux::query::Filterable as _;\n",
            "\n",
            "let fields = libtmux::", stringify!($handle), "::filter_fields();\n",
            "\n",
            "// Every handle set carries its target's identity, here `",
            $baseline_name, "`.\n",
            "let expression = fields.", stringify!($baseline_field), ".eq(\"\");\n",
            "\n",
            "// The handles are zero-sized names, so `Debug` reports which set\n",
            "// is being held rather than listing a page of nothing.\n",
            "assert_eq!(format!(\"{fields:?}\"), \"", stringify!($fields), " { .. }\");\n",
            "# let _ = expression;\n",
            "# }\n",
            "```",
        )]
        #[cfg(feature = "query")]
        pub struct $fields<Target> {
            #[doc = concat!("The `", $baseline_name, "` field.")]
            pub $baseline_field: filter_field_type!(Target, $baseline_decoder),
            $(#[doc = concat!("The `", $name, "` field.")]
            pub $field: filter_field_type!(Target, $decoder)),*
        }

        // Named rather than exhaustive. Every field is a zero-sized handle
        // whose whole content is its own name, so listing all of them prints a
        // page of nothing; what a caller printing this wants to know is which
        // handle set they are holding.
        #[cfg(feature = "query")]
        impl<Target> core::fmt::Debug for $fields<Target> {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                formatter.debug_struct(stringify!($fields)).finish_non_exhaustive()
            }
        }

        #[cfg(feature = "query")]
        impl<Target> $fields<Target> {
            /// Build handles bound to one filter target name.
            pub(crate) fn for_target(target: &'static str) -> Self {
                Self {
                    $baseline_field: filter_field_value!(
                        Target,
                        target,
                        $baseline_name,
                        $baseline_decoder
                    ),
                    $($field: filter_field_value!(
                        Target,
                        target,
                        $name,
                        $decoder
                    )),*
                }
            }
        }

        #[cfg(feature = "query")]
        impl Filterable for $info {
            type Fields = $fields<$info>;

            const FILTER_TARGET: &'static str = $target;

            fn filter_fields() -> Self::Fields {
                Self::Fields::for_target(Self::FILTER_TARGET)
            }

            fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
                match predicate.field() {
                    $baseline_name => {
                        match_scalar!(predicate, &self.$baseline_field, $baseline_decoder)
                    }
                    $($name => match_stored!(predicate, &self.$field, $decoder, $floor, $empty),)*
                    _ => false,
                }
            }

            fn __filter_validate(
                predicate: &crate::query::__private::Predicate,
            ) -> Result<(), FilterExpressionError> {
                match predicate.field() {
                    $baseline_name => validate_scalar!(predicate, $baseline_decoder),
                    $($name => validate_scalar!(predicate, $decoder),)*
                    _ => Err(crate::query::__private::unknown_field_error()),
                }
            }
        }

        #[cfg(feature = "query")]
        impl FilterSchema for $info {
            fn __filter_schema() -> crate::query::__private::FilterSchemaDescriptor {
                crate::query::__private::FilterSchemaDescriptor::new(
                    $target,
                    vec![
                        crate::query::__private::FilterFieldSchema::new(
                            $baseline_name,
                            filter_schema_value!($baseline_decoder),
                        ),
                        $(crate::query::__private::FilterFieldSchema::new(
                            $name,
                            filter_schema_value!($decoder),
                        )),*
                    ],
                )
            }
        }

        fn $hydrate_inner(
            hydrator: &mut RowHydrator<'_, '_>,
        ) -> Result<$info, FormatCodecError> {
            let $baseline_field = required_identity(
                decode_catalog_field!(
                    hydrator,
                    &crate::formats::$baseline_static,
                    $baseline_decoder
                )?,
                hydrator.row.row(),
                &crate::formats::$baseline_static,
            )?;
            $(let $field = decode_stored!(
                hydrator,
                &crate::formats::$static,
                $decoder,
                $floor,
                $empty
            )?;)*

            Ok($info {
                $baseline_field,
                $($field),*
            })
        }

        #[allow(
            dead_code,
            reason = "modelled and tested; only a projection of it is hydrated today"
        )]
        fn $hydrate(
            plan: &FormatPlan,
            row: &ParsedRow,
        ) -> Result<$info, FormatCodecError> {
            if plan.purpose() != PlanPurpose::Intrinsic(InfoPlacement::$placement) {
                return Err(FormatCodecError::purpose_mismatch(plan.profile(), row.row()));
            }

            let mut hydrator = RowHydrator::new(plan, row);
            let info = $hydrate_inner(&mut hydrator)?;
            hydrator.finish()?;
            Ok(info)
        }
    };
}

macro_rules! define_snapshots {
    (
        SessionInfo {
            target: $session_target:literal;
            baseline: $session_baseline:tt;
            supplements: [$($session_supplements:tt),* $(,)?];
        }
        WindowInfo {
            target: $window_target:literal;
            baseline: $window_baseline:tt;
            supplements: [$($window_supplements:tt),* $(,)?];
        }
        PaneInfo {
            target: $pane_target:literal;
            baseline: $pane_baseline:tt;
            supplements: [$($pane_supplements:tt),* $(,)?];
        }
        ClientInfo {
            target: $client_target:literal;
            baseline: $client_baseline:tt;
            supplements: [$($client_supplements:tt),* $(,)?];
        }
        CatalogOnly { $($catalog_only:tt)* }
    ) => {
        define_snapshot_info!(
            SessionInfo,
            SessionFields,
            Session,
            hydrate_session_info,
            decode_session_info_inner,
            $session_target,
            SessionInfo,
            $session_baseline,
            [$($session_supplements),*]
        );
        define_snapshot_info!(
            WindowInfo,
            WindowFields,
            Window,
            hydrate_window_info,
            decode_window_info_inner,
            $window_target,
            WindowInfo,
            $window_baseline,
            [$($window_supplements),*]
        );
        define_snapshot_info!(
            PaneInfo,
            PaneFields,
            Pane,
            hydrate_pane_info,
            decode_pane_info_inner,
            $pane_target,
            PaneInfo,
            $pane_baseline,
            [$($pane_supplements),*]
        );
        define_snapshot_info!(
            ClientInfo,
            ClientFields,
            Client,
            hydrate_client_info,
            decode_client_info_inner,
            $client_target,
            ClientInfo,
            $client_baseline,
            [$($client_supplements),*]
        );
    };
}
format_catalog!(define_snapshots);

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
static WINDOW_PROJECTION_SUFFIX: &[&FormatDescriptor] = &[
    &crate::formats::SESSION_ID,
    &crate::formats::WINDOW_INDEX,
    &crate::formats::WINDOW_ACTIVE,
    &crate::formats::WINDOW_ACTIVITY_FLAG,
    &crate::formats::WINDOW_BELL_FLAG,
    &crate::formats::WINDOW_END_FLAG,
    &crate::formats::WINDOW_FLAGS,
    &crate::formats::WINDOW_LAST_FLAG,
    &crate::formats::WINDOW_LINKED,
    &crate::formats::WINDOW_MARKED_FLAG,
    &crate::formats::WINDOW_RAW_FLAGS,
    &crate::formats::WINDOW_SILENCE_FLAG,
    &crate::formats::WINDOW_STACK_INDEX,
    &crate::formats::WINDOW_START_FLAG,
    &crate::formats::WINDOW_ACTIVE_CLIENTS,
    &crate::formats::WINDOW_ACTIVE_CLIENTS_LIST,
    &crate::formats::WINDOW_ACTIVE_SESSIONS,
    &crate::formats::WINDOW_ACTIVE_SESSIONS_LIST,
    &crate::formats::WINDOW_LINKED_SESSIONS,
    &crate::formats::WINDOW_LINKED_SESSIONS_LIST,
];

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
static PANE_PROJECTION_SUFFIX: &[&FormatDescriptor] = &[
    &crate::formats::SESSION_ID,
    &crate::formats::WINDOW_ID,
    &crate::formats::WINDOW_INDEX,
];

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "fields match the tmux wire schema"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WindowLink {
    identity: WindowLinkIdentity,
    window_active: bool,
    window_activity_flag: bool,
    window_bell_flag: bool,
    window_end_flag: bool,
    window_flags: TmuxText,
    window_last_flag: bool,
    window_linked: bool,
    window_marked_flag: bool,
    window_raw_flags: TmuxText,
    window_silence_flag: bool,
    window_stack_index: u32,
    window_start_flag: bool,
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
impl WindowLink {
    pub(crate) const fn identity(&self) -> &WindowLinkIdentity {
        &self.identity
    }

    /// Report whether this edge is the session's active window.
    ///
    /// Activity belongs to the edge, not the window: one linked window can be
    /// active in one session and inactive in another.
    pub(crate) const fn is_active(&self) -> bool {
        self.window_active
    }

    /// Report whether the window is linked into more than one session.
    pub(crate) const fn is_linked(&self) -> bool {
        self.window_linked
    }

    /// Report whether the window has unseen activity in this session.
    pub(crate) const fn has_activity(&self) -> bool {
        self.window_activity_flag
    }

    /// Report whether the window rang a bell in this session.
    pub(crate) const fn has_bell(&self) -> bool {
        self.window_bell_flag
    }

    /// Return the session's most-recently-used ordering for this edge.
    pub(crate) const fn stack_index(&self) -> u32 {
        self.window_stack_index
    }
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WindowProjection {
    window: WindowInfo,
    link: WindowLink,
    window_active_clients: u32,
    window_active_clients_list: TmuxText,
    window_active_sessions: u32,
    window_active_sessions_list: TmuxText,
    window_linked_sessions: u32,
    window_linked_sessions_list: TmuxText,
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
impl WindowProjection {
    pub(crate) const fn window(&self) -> &WindowInfo {
        &self.window
    }

    pub(crate) const fn link(&self) -> &WindowLink {
        &self.link
    }
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PaneProjection {
    pane: PaneInfo,
    link_identity: WindowLinkIdentity,
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
impl PaneProjection {
    pub(crate) const fn pane(&self) -> &PaneInfo {
        &self.pane
    }

    pub(crate) const fn link_identity(&self) -> &WindowLinkIdentity {
        &self.link_identity
    }
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
pub(crate) fn window_projection_plan(
    version: &TmuxVersion,
) -> Result<FormatPlan, FormatCodecError> {
    let requested: Vec<_> = WINDOW_INFO_SUPPLEMENTS
        .iter()
        .copied()
        .chain(WINDOW_PROJECTION_SUFFIX.iter().copied())
        .collect();
    FormatPlan::for_descriptors(ListProfile::Windows, version, &requested)
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
pub(crate) fn pane_projection_plan(version: &TmuxVersion) -> Result<FormatPlan, FormatCodecError> {
    let requested: Vec<_> = PANE_INFO_SUPPLEMENTS
        .iter()
        .copied()
        .chain(PANE_PROJECTION_SUFFIX.iter().copied())
        .collect();
    FormatPlan::for_descriptors(ListProfile::Panes, version, &requested)
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn required_projection_value<T>(
    availability: Availability<T>,
    hydrator: &RowHydrator<'_, '_>,
    descriptor: &'static FormatDescriptor,
) -> Result<T, FormatCodecError> {
    match availability {
        Availability::Available(value) => Ok(value),
        Availability::Unsupported | Availability::Unproven | Availability::Absent => {
            Err(FormatCodecError::row_mismatch(
                hydrator.row.row(),
                Some(hydrator.slot_index),
                Some(descriptor.name()),
                None,
            ))
        }
    }
}

macro_rules! decode_required_projection {
    ($hydrator:expr, $descriptor:expr, $decoder:ident) => {{
        let availability = decode_catalog_field!($hydrator, $descriptor, $decoder)?;
        required_projection_value(availability, $hydrator, $descriptor)?
    }};
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn require_projection_plan(
    plan: &FormatPlan,
    profile: ListProfile,
    row: usize,
) -> Result<(), FormatCodecError> {
    if plan.profile() != profile || plan.purpose() != PlanPurpose::Projection {
        return Err(FormatCodecError::purpose_mismatch(plan.profile(), row));
    }
    Ok(())
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn hydrate_window_projection(
    server_identity: &ServerIdentity,
    plan: &FormatPlan,
    row: &ParsedRow,
) -> Result<WindowProjection, FormatCodecError> {
    require_projection_plan(plan, ListProfile::Windows, row.row())?;

    let mut hydrator = RowHydrator::new(plan, row);
    let window = decode_window_info_inner(&mut hydrator)?;
    let session_id =
        decode_required_projection!(&mut hydrator, &crate::formats::SESSION_ID, SessionId);
    let window_index =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_INDEX, I32);
    let window_active =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_ACTIVE, Bool);
    let window_activity_flag =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_ACTIVITY_FLAG, Bool);
    let window_bell_flag =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_BELL_FLAG, Bool);
    let window_end_flag =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_END_FLAG, Bool);
    let window_flags =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_FLAGS, Text);
    let window_last_flag =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_LAST_FLAG, Bool);
    let window_linked =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_LINKED, Bool);
    let window_marked_flag =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_MARKED_FLAG, Bool);
    let window_raw_flags =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_RAW_FLAGS, Text);
    let window_silence_flag =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_SILENCE_FLAG, Bool);
    let window_stack_index =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_STACK_INDEX, U32);
    let window_start_flag =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_START_FLAG, Bool);
    let window_active_clients =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_ACTIVE_CLIENTS, U32);
    let window_active_clients_list = decode_required_projection!(
        &mut hydrator,
        &crate::formats::WINDOW_ACTIVE_CLIENTS_LIST,
        Text
    );
    let window_active_sessions =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_ACTIVE_SESSIONS, U32);
    let window_active_sessions_list = decode_required_projection!(
        &mut hydrator,
        &crate::formats::WINDOW_ACTIVE_SESSIONS_LIST,
        Text
    );
    let window_linked_sessions =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_LINKED_SESSIONS, U32);
    let window_linked_sessions_list = decode_required_projection!(
        &mut hydrator,
        &crate::formats::WINDOW_LINKED_SESSIONS_LIST,
        Text
    );
    hydrator.finish()?;

    let identity = WindowLinkIdentity::new(
        server_identity.clone(),
        session_id,
        window_index,
        window.window_id.clone(),
    );
    Ok(WindowProjection {
        window,
        link: WindowLink {
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
        },
        window_active_clients,
        window_active_clients_list,
        window_active_sessions,
        window_active_sessions_list,
        window_linked_sessions,
        window_linked_sessions_list,
    })
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn hydrate_window_projections(
    server_identity: &ServerIdentity,
    plan: &FormatPlan,
    rows: &[ParsedRow],
) -> Result<Vec<WindowProjection>, FormatCodecError> {
    let row = rows.first().map_or(0, ParsedRow::row);
    require_projection_plan(plan, ListProfile::Windows, row)?;
    rows.iter()
        .map(|row| hydrate_window_projection(server_identity, plan, row))
        .collect()
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
pub(crate) fn hydrate_window_projections_from_stdout(
    server_identity: &ServerIdentity,
    plan: &FormatPlan,
    stdout: &[u8],
) -> Result<Vec<WindowProjection>, FormatCodecError> {
    require_projection_plan(plan, ListProfile::Windows, 0)?;
    let rows = plan.parse_rows(stdout)?;
    hydrate_window_projections(server_identity, plan, &rows)
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn hydrate_pane_projection(
    server_identity: &ServerIdentity,
    plan: &FormatPlan,
    row: &ParsedRow,
) -> Result<PaneProjection, FormatCodecError> {
    require_projection_plan(plan, ListProfile::Panes, row.row())?;

    let mut hydrator = RowHydrator::new(plan, row);
    let pane = decode_pane_info_inner(&mut hydrator)?;
    let session_id =
        decode_required_projection!(&mut hydrator, &crate::formats::SESSION_ID, SessionId);
    let window_id =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_ID, WindowId);
    let window_index =
        decode_required_projection!(&mut hydrator, &crate::formats::WINDOW_INDEX, I32);
    hydrator.finish()?;

    Ok(PaneProjection {
        pane,
        link_identity: WindowLinkIdentity::new(
            server_identity.clone(),
            session_id,
            window_index,
            window_id,
        ),
    })
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn hydrate_pane_projections(
    server_identity: &ServerIdentity,
    plan: &FormatPlan,
    rows: &[ParsedRow],
) -> Result<Vec<PaneProjection>, FormatCodecError> {
    let row = rows.first().map_or(0, ParsedRow::row);
    require_projection_plan(plan, ListProfile::Panes, row)?;
    rows.iter()
        .map(|row| hydrate_pane_projection(server_identity, plan, row))
        .collect()
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
pub(crate) fn hydrate_pane_projections_from_stdout(
    server_identity: &ServerIdentity,
    plan: &FormatPlan,
    stdout: &[u8],
) -> Result<Vec<PaneProjection>, FormatCodecError> {
    require_projection_plan(plan, ListProfile::Panes, 0)?;
    let rows = plan.parse_rows(stdout)?;
    hydrate_pane_projections(server_identity, plan, &rows)
}

/// Decode a complete `list-sessions` stdout into ordered snapshots.
///
/// Sessions and clients are intrinsic rather than projected, so they carry no
/// winlink edge and need no server identity. Order is tmux's own.
pub(crate) fn hydrate_session_infos_from_stdout(
    plan: &FormatPlan,
    stdout: &[u8],
) -> Result<Vec<SessionInfo>, FormatCodecError> {
    plan.parse_rows(stdout)?
        .iter()
        .map(|row| hydrate_session_info(plan, row))
        .collect()
}

/// Decode a complete `list-clients` stdout into ordered snapshots.
#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
pub(crate) fn hydrate_client_infos_from_stdout(
    plan: &FormatPlan,
    stdout: &[u8],
) -> Result<Vec<ClientInfo>, FormatCodecError> {
    plan.parse_rows(stdout)?
        .iter()
        .map(|row| hydrate_client_info(plan, row))
        .collect()
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PointSelectionError {
    MixedSession,
}

impl fmt::Display for PointSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MixedSession => formatter.write_str("mixed Session candidates"),
        }
    }
}

impl std::error::Error for PointSelectionError {}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn select_local_window_projection(
    projections: &[WindowProjection],
) -> Result<Option<&WindowProjection>, PointSelectionError> {
    let Some(first) = projections.first() else {
        return Ok(None);
    };
    let session_id = first.link.identity.session_id();
    if projections
        .iter()
        .any(|projection| projection.link.identity.session_id() != session_id)
    {
        return Err(PointSelectionError::MixedSession);
    }
    if let Some(active) = projections
        .iter()
        .find(|projection| projection.link.window_active)
    {
        return Ok(Some(active));
    }

    Ok(projections.iter().reduce(|lowest, candidate| {
        if candidate.link.identity.window_index() < lowest.link.identity.window_index() {
            candidate
        } else {
            lowest
        }
    }))
}

#[allow(
    dead_code,
    reason = "modelled and tested; no public accessor returns it yet"
)]
fn holder_session_ids(
    projections: &[WindowProjection],
    server_identity: &ServerIdentity,
    window_id: &WindowId,
) -> Vec<SessionId> {
    let mut seen = HashSet::new();
    projections
        .iter()
        .filter_map(|projection| {
            let identity = projection.link.identity();
            if identity.server_identity() != server_identity || identity.window_id() != window_id {
                return None;
            }
            let session_id = identity.session_id().clone();
            seen.insert(session_id.clone()).then_some(session_id)
        })
        .collect()
}

/// Validate arbitrary test plans with the production single-pass walker.
#[cfg(test)]
fn validate_planned_row_for_test(
    plan: &FormatPlan,
    row: &ParsedRow,
) -> Result<(), FormatCodecError> {
    let mut hydrator = RowHydrator::new(plan, row);
    let mut index = 0;
    while let Some(descriptor) = plan.planned().get(index).map(|field| field.descriptor) {
        decode_marker(&mut hydrator, descriptor)?;
        index += 1;
    }
    hydrator.finish()
}

#[cfg(test)]
mod tests;
