#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::{
    __private::{self, Predicate},
    BoolField, FilterExpressionError, FilterExpressionErrorKind, Filterable as QueryFilterable,
    IntegerField, TextField,
};
use renamed_libtmux::TmuxText;

#[derive(Filterable)]
#[filterable(target = "scalar_row")]
struct ScalarRow {
    string: String,
    maybe_string: Option<String>,
    raw: TmuxText,
    maybe_raw: Option<TmuxText>,
    flag: bool,
    maybe_flag: Option<bool>,
    i8_value: i8,
    maybe_i8: Option<i8>,
    i16_value: i16,
    maybe_i16: Option<i16>,
    i32_value: i32,
    maybe_i32: Option<i32>,
    i64_value: i64,
    maybe_i64: Option<i64>,
    i128_value: i128,
    maybe_i128: Option<i128>,
    u8_value: u8,
    maybe_u8: Option<u8>,
    u16_value: u16,
    maybe_u16: Option<u16>,
    u32_value: u32,
    maybe_u32: Option<u32>,
    u64_value: u64,
    maybe_u64: Option<u64>,
    u128_value: u128,
    maybe_u128: Option<u128>,
}

fn text(_: TextField<ScalarRow>) {}
fn boolean(_: BoolField<ScalarRow>) {}
fn integer<N>(_: IntegerField<ScalarRow, N>) {}

fn candidate(optionals_present: bool) -> ScalarRow {
    ScalarRow {
        string: String::from("string"),
        maybe_string: optionals_present.then(|| String::from("maybe-string")),
        raw: TmuxText::from("raw"),
        maybe_raw: optionals_present.then(|| TmuxText::from("maybe-raw")),
        flag: true,
        maybe_flag: optionals_present.then_some(false),
        i8_value: i8::MIN,
        maybe_i8: optionals_present.then_some(i8::MAX),
        i16_value: i16::MIN,
        maybe_i16: optionals_present.then_some(i16::MAX),
        i32_value: i32::MIN,
        maybe_i32: optionals_present.then_some(i32::MAX),
        i64_value: i64::MIN,
        maybe_i64: optionals_present.then_some(i64::MAX),
        i128_value: i128::MIN,
        maybe_i128: optionals_present.then_some(i128::MAX),
        u8_value: u8::MAX,
        maybe_u8: optionals_present.then_some(0),
        u16_value: u16::MAX,
        maybe_u16: optionals_present.then_some(0),
        u32_value: u32::MAX,
        maybe_u32: optionals_present.then_some(0),
        u64_value: u64::MAX,
        maybe_u64: optionals_present.then_some(0),
        u128_value: u128::MAX,
        maybe_u128: optionals_present.then_some(0),
    }
}

struct ValidationProbe {
    expected: Option<FilterExpressionErrorKind>,
}

struct ValidationProbeFields {
    i8_value: IntegerField<ValidationProbe, i128>,
    maybe_i8: IntegerField<ValidationProbe, i128>,
    i16_value: IntegerField<ValidationProbe, i128>,
    maybe_i16: IntegerField<ValidationProbe, i128>,
    i32_value: IntegerField<ValidationProbe, i128>,
    maybe_i32: IntegerField<ValidationProbe, i128>,
    i64_value: IntegerField<ValidationProbe, i128>,
    maybe_i64: IntegerField<ValidationProbe, i128>,
    i128_value: IntegerField<ValidationProbe, i128>,
    maybe_i128: IntegerField<ValidationProbe, i128>,
    u8_value: IntegerField<ValidationProbe, u128>,
    maybe_u8: IntegerField<ValidationProbe, u128>,
    u16_value: IntegerField<ValidationProbe, u128>,
    maybe_u16: IntegerField<ValidationProbe, u128>,
    u32_value: IntegerField<ValidationProbe, u128>,
    maybe_u32: IntegerField<ValidationProbe, u128>,
    u64_value: IntegerField<ValidationProbe, u128>,
    maybe_u64: IntegerField<ValidationProbe, u128>,
    u128_value: IntegerField<ValidationProbe, u128>,
    maybe_u128: IntegerField<ValidationProbe, u128>,
    unknown: TextField<ValidationProbe>,
}

impl QueryFilterable for ValidationProbe {
    type Fields = ValidationProbeFields;

    const FILTER_TARGET: &'static str = "scalar_row";

    fn filter_fields() -> Self::Fields {
        ValidationProbeFields {
            i8_value: __private::integer_field(Self::FILTER_TARGET, "i8_value"),
            maybe_i8: __private::integer_field(Self::FILTER_TARGET, "maybe_i8"),
            i16_value: __private::integer_field(Self::FILTER_TARGET, "i16_value"),
            maybe_i16: __private::integer_field(Self::FILTER_TARGET, "maybe_i16"),
            i32_value: __private::integer_field(Self::FILTER_TARGET, "i32_value"),
            maybe_i32: __private::integer_field(Self::FILTER_TARGET, "maybe_i32"),
            i64_value: __private::integer_field(Self::FILTER_TARGET, "i64_value"),
            maybe_i64: __private::integer_field(Self::FILTER_TARGET, "maybe_i64"),
            i128_value: __private::integer_field(Self::FILTER_TARGET, "i128_value"),
            maybe_i128: __private::integer_field(Self::FILTER_TARGET, "maybe_i128"),
            u8_value: __private::integer_field(Self::FILTER_TARGET, "u8_value"),
            maybe_u8: __private::integer_field(Self::FILTER_TARGET, "maybe_u8"),
            u16_value: __private::integer_field(Self::FILTER_TARGET, "u16_value"),
            maybe_u16: __private::integer_field(Self::FILTER_TARGET, "maybe_u16"),
            u32_value: __private::integer_field(Self::FILTER_TARGET, "u32_value"),
            maybe_u32: __private::integer_field(Self::FILTER_TARGET, "maybe_u32"),
            u64_value: __private::integer_field(Self::FILTER_TARGET, "u64_value"),
            maybe_u64: __private::integer_field(Self::FILTER_TARGET, "maybe_u64"),
            u128_value: __private::integer_field(Self::FILTER_TARGET, "u128_value"),
            maybe_u128: __private::integer_field(Self::FILTER_TARGET, "maybe_u128"),
            unknown: __private::text_field(Self::FILTER_TARGET, "unknown"),
        }
    }

    fn __filter_matches(&self, predicate: &Predicate) -> bool {
        match (
            <ScalarRow as QueryFilterable>::__filter_validate(predicate),
            self.expected,
        ) {
            (Ok(()), None) => true,
            (Err(error), Some(expected)) => error.kind() == expected,
            _ => false,
        }
    }

    fn __filter_validate(_: &Predicate) -> Result<(), FilterExpressionError> {
        Ok(())
    }
}

fn assert_signed_validation(
    field: IntegerField<ValidationProbe, i128>,
    minimum: i128,
    maximum: i128,
    outside: Option<i128>,
) {
    let valid = ValidationProbe { expected: None };
    let invalid_literal = ValidationProbe {
        expected: Some(FilterExpressionErrorKind::InvalidLiteral),
    };
    assert!(field.eq(minimum).matches(&valid));
    assert!(field.eq(maximum).matches(&valid));
    if let Some(outside) = outside {
        assert!(field.eq(outside).matches(&invalid_literal));
    }
}

fn assert_unsigned_validation(
    field: IntegerField<ValidationProbe, u128>,
    maximum: u128,
    outside: Option<u128>,
) {
    let valid = ValidationProbe { expected: None };
    let invalid_literal = ValidationProbe {
        expected: Some(FilterExpressionErrorKind::InvalidLiteral),
    };
    assert!(field.eq(0).matches(&valid));
    assert!(field.eq(maximum).matches(&valid));
    if let Some(outside) = outside {
        assert!(field.eq(outside).matches(&invalid_literal));
    }
}

fn main() {
    let fields = ScalarRow::filter_fields();
    text(fields.string);
    text(fields.maybe_string);
    text(fields.raw);
    text(fields.maybe_raw);
    boolean(fields.flag);
    boolean(fields.maybe_flag);
    integer::<i8>(fields.i8_value);
    integer::<i8>(fields.maybe_i8);
    integer::<i16>(fields.i16_value);
    integer::<i16>(fields.maybe_i16);
    integer::<i32>(fields.i32_value);
    integer::<i32>(fields.maybe_i32);
    integer::<i64>(fields.i64_value);
    integer::<i64>(fields.maybe_i64);
    integer::<i128>(fields.i128_value);
    integer::<i128>(fields.maybe_i128);
    integer::<u8>(fields.u8_value);
    integer::<u8>(fields.maybe_u8);
    integer::<u16>(fields.u16_value);
    integer::<u16>(fields.maybe_u16);
    integer::<u32>(fields.u32_value);
    integer::<u32>(fields.maybe_u32);
    integer::<u64>(fields.u64_value);
    integer::<u64>(fields.maybe_u64);
    integer::<u128>(fields.u128_value);
    integer::<u128>(fields.maybe_u128);

    let hit = candidate(true);
    let none = candidate(false);
    assert!(fields.string.eq("string").matches(&hit));
    assert!(fields.maybe_string.eq("maybe-string").matches(&hit));
    assert!(!fields.maybe_string.eq("maybe-string").matches(&none));
    assert!(fields.maybe_string.not_in([] as [&str; 0]).matches(&hit));
    assert!(!fields
        .maybe_string
        .not_in([] as [&str; 0])
        .matches(&none));
    assert!(fields
        .maybe_string
        .not_in([] as [&str; 0])
        .not()
        .matches(&none));
    assert!(fields.raw.eq("raw").matches(&hit));
    assert!(fields.maybe_raw.eq("maybe-raw").matches(&hit));
    assert!(!fields.maybe_raw.eq("maybe-raw").matches(&none));
    assert!(fields.maybe_raw.not_in([] as [&str; 0]).matches(&hit));
    assert!(!fields
        .maybe_raw
        .not_in([] as [&str; 0])
        .matches(&none));
    assert!(fields
        .maybe_raw
        .not_in([] as [&str; 0])
        .not()
        .matches(&none));
    let mut invalid_raw = candidate(true);
    invalid_raw.raw = TmuxText::from(vec![b'r', 0xff]);
    invalid_raw.maybe_raw = Some(TmuxText::from(vec![b'm', 0xff]));
    assert!(!fields.raw.eq("r").matches(&invalid_raw));
    assert!(!fields.maybe_raw.eq("m").matches(&invalid_raw));
    assert!(!fields
        .raw
        .not_in([] as [&str; 0])
        .matches(&invalid_raw));
    assert!(!fields
        .maybe_raw
        .not_in([] as [&str; 0])
        .matches(&invalid_raw));
    assert!(fields.flag.eq(true).matches(&hit));
    assert!(fields.maybe_flag.eq(false).matches(&hit));
    assert!(!fields.maybe_flag.eq(false).matches(&none));
    assert!(fields.maybe_flag.not_in([]).matches(&hit));
    assert!(!fields.maybe_flag.not_in([]).matches(&none));
    assert!(fields.maybe_flag.not_in([]).not().matches(&none));

    macro_rules! assert_integer_matches {
        ($field:expr, $value:expr, $optional:expr, $optional_value:expr) => {{
            assert!($field.eq($value).matches(&hit));
            assert!($optional.eq($optional_value).matches(&hit));
            assert!(!$optional.eq($optional_value).matches(&none));
            assert!($optional.not_in([]).matches(&hit));
            assert!(!$optional.not_in([]).matches(&none));
            assert!($optional.not_in([]).not().matches(&none));
        }};
    }
    assert_integer_matches!(fields.i8_value, i8::MIN, fields.maybe_i8, i8::MAX);
    assert_integer_matches!(fields.i16_value, i16::MIN, fields.maybe_i16, i16::MAX);
    assert_integer_matches!(fields.i32_value, i32::MIN, fields.maybe_i32, i32::MAX);
    assert_integer_matches!(fields.i64_value, i64::MIN, fields.maybe_i64, i64::MAX);
    assert_integer_matches!(
        fields.i128_value,
        i128::MIN,
        fields.maybe_i128,
        i128::MAX
    );
    assert_integer_matches!(fields.u8_value, u8::MAX, fields.maybe_u8, 0);
    assert_integer_matches!(fields.u16_value, u16::MAX, fields.maybe_u16, 0);
    assert_integer_matches!(fields.u32_value, u32::MAX, fields.maybe_u32, 0);
    assert_integer_matches!(fields.u64_value, u64::MAX, fields.maybe_u64, 0);
    assert_integer_matches!(
        fields.u128_value,
        u128::MAX,
        fields.maybe_u128,
        0
    );

    let probe = ValidationProbe::filter_fields();
    assert_signed_validation(
        probe.i8_value,
        i128::from(i8::MIN),
        i128::from(i8::MAX),
        Some(i128::from(i8::MAX) + 1),
    );
    assert_signed_validation(
        probe.maybe_i8,
        i128::from(i8::MIN),
        i128::from(i8::MAX),
        Some(i128::from(i8::MAX) + 1),
    );
    assert_signed_validation(
        probe.i16_value,
        i128::from(i16::MIN),
        i128::from(i16::MAX),
        Some(i128::from(i16::MAX) + 1),
    );
    assert_signed_validation(
        probe.maybe_i16,
        i128::from(i16::MIN),
        i128::from(i16::MAX),
        Some(i128::from(i16::MAX) + 1),
    );
    assert_signed_validation(
        probe.i32_value,
        i128::from(i32::MIN),
        i128::from(i32::MAX),
        Some(i128::from(i32::MAX) + 1),
    );
    assert_signed_validation(
        probe.maybe_i32,
        i128::from(i32::MIN),
        i128::from(i32::MAX),
        Some(i128::from(i32::MAX) + 1),
    );
    assert_signed_validation(
        probe.i64_value,
        i128::from(i64::MIN),
        i128::from(i64::MAX),
        Some(i128::from(i64::MAX) + 1),
    );
    assert_signed_validation(
        probe.maybe_i64,
        i128::from(i64::MIN),
        i128::from(i64::MAX),
        Some(i128::from(i64::MAX) + 1),
    );
    assert_signed_validation(probe.i128_value, i128::MIN, i128::MAX, None);
    assert_signed_validation(probe.maybe_i128, i128::MIN, i128::MAX, None);
    assert_unsigned_validation(
        probe.u8_value,
        u128::from(u8::MAX),
        Some(u128::from(u8::MAX) + 1),
    );
    assert_unsigned_validation(
        probe.maybe_u8,
        u128::from(u8::MAX),
        Some(u128::from(u8::MAX) + 1),
    );
    assert_unsigned_validation(
        probe.u16_value,
        u128::from(u16::MAX),
        Some(u128::from(u16::MAX) + 1),
    );
    assert_unsigned_validation(
        probe.maybe_u16,
        u128::from(u16::MAX),
        Some(u128::from(u16::MAX) + 1),
    );
    assert_unsigned_validation(
        probe.u32_value,
        u128::from(u32::MAX),
        Some(u128::from(u32::MAX) + 1),
    );
    assert_unsigned_validation(
        probe.maybe_u32,
        u128::from(u32::MAX),
        Some(u128::from(u32::MAX) + 1),
    );
    assert_unsigned_validation(
        probe.u64_value,
        u128::from(u64::MAX),
        Some(u128::from(u64::MAX) + 1),
    );
    assert_unsigned_validation(
        probe.maybe_u64,
        u128::from(u64::MAX),
        Some(u128::from(u64::MAX) + 1),
    );
    assert_unsigned_validation(probe.u128_value, u128::MAX, None);
    assert_unsigned_validation(probe.maybe_u128, u128::MAX, None);
    let unknown_field = ValidationProbe {
        expected: Some(FilterExpressionErrorKind::UnknownField),
    };
    assert!(probe.unknown.eq("unknown").matches(&unknown_field));

    let unknown_match =
        __private::text_field::<ScalarRow>("scalar_row", "unknown").eq("unknown");
    assert!(!unknown_match.matches(&hit));
}
