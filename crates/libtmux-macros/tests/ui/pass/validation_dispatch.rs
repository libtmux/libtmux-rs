#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::{
    __private::{self, Predicate},
    BoolField, EnumField, FilterEnum, FilterExpressionError, FilterExpressionErrorKind,
    Filterable as QueryFilterable, IntegerField, ManyRelation, OneRelation, TextField,
};
use renamed_libtmux::TmuxText;

enum State {
    Ready,
}

impl FilterEnum for State {
    const FILTER_VARIANTS: &'static [&'static str] = &["ready"];

    fn filter_name(&self) -> &'static str {
        "ready"
    }
}

enum ProbeState {
    Other,
}

impl FilterEnum for ProbeState {
    const FILTER_VARIANTS: &'static [&'static str] = &["other"];

    fn filter_name(&self) -> &'static str {
        "other"
    }
}

#[derive(Filterable)]
#[filterable(target = "validation_child")]
struct Child {
    done: bool,
}

#[derive(Filterable)]
#[filterable(target = "validation_schema")]
struct ValidationSchema {
    text: String,
    maybe_text: Option<String>,
    raw: TmuxText,
    maybe_raw: Option<TmuxText>,
    flag: bool,
    maybe_flag: Option<bool>,
    maybe_i8: Option<i8>,
    maybe_u8: Option<u8>,
    #[filterable(enum)]
    state: State,
    #[filterable(enum)]
    maybe_state: Option<State>,
    #[filterable(many)]
    children: Vec<Child>,
    #[filterable(one)]
    favorite: Option<Child>,
}

struct ValidationProbe {
    expected: Option<FilterExpressionErrorKind>,
}

struct ValidationProbeFields {
    text: TextField<ValidationProbe>,
    text_as_bool: BoolField<ValidationProbe>,
    maybe_text: TextField<ValidationProbe>,
    maybe_text_as_bool: BoolField<ValidationProbe>,
    raw: TextField<ValidationProbe>,
    raw_as_bool: BoolField<ValidationProbe>,
    maybe_raw: TextField<ValidationProbe>,
    maybe_raw_as_bool: BoolField<ValidationProbe>,
    flag: BoolField<ValidationProbe>,
    flag_as_text: TextField<ValidationProbe>,
    maybe_flag: BoolField<ValidationProbe>,
    maybe_flag_as_text: TextField<ValidationProbe>,
    maybe_i8: IntegerField<ValidationProbe, i128>,
    maybe_i8_as_text: TextField<ValidationProbe>,
    maybe_u8: IntegerField<ValidationProbe, u128>,
    maybe_u8_as_text: TextField<ValidationProbe>,
    state: EnumField<ValidationProbe, State>,
    state_unknown: EnumField<ValidationProbe, ProbeState>,
    state_as_bool: BoolField<ValidationProbe>,
    maybe_state: EnumField<ValidationProbe, State>,
    maybe_state_unknown: EnumField<ValidationProbe, ProbeState>,
    maybe_state_as_bool: BoolField<ValidationProbe>,
    children: ManyRelation<ValidationProbe, Child>,
    children_as_one: OneRelation<ValidationProbe, Child>,
    children_as_bool: BoolField<ValidationProbe>,
    favorite: OneRelation<ValidationProbe, Child>,
    favorite_as_many: ManyRelation<ValidationProbe, Child>,
    favorite_as_bool: BoolField<ValidationProbe>,
}

impl QueryFilterable for ValidationProbe {
    type Fields = ValidationProbeFields;

    const FILTER_TARGET: &'static str = "validation_schema";

    fn filter_fields() -> Self::Fields {
        ValidationProbeFields {
            text: __private::text_field(Self::FILTER_TARGET, "text"),
            text_as_bool: __private::bool_field(Self::FILTER_TARGET, "text"),
            maybe_text: __private::text_field(Self::FILTER_TARGET, "maybe_text"),
            maybe_text_as_bool: __private::bool_field(Self::FILTER_TARGET, "maybe_text"),
            raw: __private::text_field(Self::FILTER_TARGET, "raw"),
            raw_as_bool: __private::bool_field(Self::FILTER_TARGET, "raw"),
            maybe_raw: __private::text_field(Self::FILTER_TARGET, "maybe_raw"),
            maybe_raw_as_bool: __private::bool_field(Self::FILTER_TARGET, "maybe_raw"),
            flag: __private::bool_field(Self::FILTER_TARGET, "flag"),
            flag_as_text: __private::text_field(Self::FILTER_TARGET, "flag"),
            maybe_flag: __private::bool_field(Self::FILTER_TARGET, "maybe_flag"),
            maybe_flag_as_text: __private::text_field(Self::FILTER_TARGET, "maybe_flag"),
            maybe_i8: __private::integer_field(Self::FILTER_TARGET, "maybe_i8"),
            maybe_i8_as_text: __private::text_field(Self::FILTER_TARGET, "maybe_i8"),
            maybe_u8: __private::integer_field(Self::FILTER_TARGET, "maybe_u8"),
            maybe_u8_as_text: __private::text_field(Self::FILTER_TARGET, "maybe_u8"),
            state: __private::enum_field(Self::FILTER_TARGET, "state"),
            state_unknown: __private::enum_field(Self::FILTER_TARGET, "state"),
            state_as_bool: __private::bool_field(Self::FILTER_TARGET, "state"),
            maybe_state: __private::enum_field(Self::FILTER_TARGET, "maybe_state"),
            maybe_state_unknown: __private::enum_field(Self::FILTER_TARGET, "maybe_state"),
            maybe_state_as_bool: __private::bool_field(Self::FILTER_TARGET, "maybe_state"),
            children: __private::many_relation(Self::FILTER_TARGET, "children"),
            children_as_one: __private::one_relation(Self::FILTER_TARGET, "children"),
            children_as_bool: __private::bool_field(Self::FILTER_TARGET, "children"),
            favorite: __private::one_relation(Self::FILTER_TARGET, "favorite"),
            favorite_as_many: __private::many_relation(Self::FILTER_TARGET, "favorite"),
            favorite_as_bool: __private::bool_field(Self::FILTER_TARGET, "favorite"),
        }
    }

    fn __filter_matches(&self, predicate: &Predicate) -> bool {
        match (
            <ValidationSchema as QueryFilterable>::__filter_validate(predicate),
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

fn main() {
    let valid = ValidationProbe { expected: None };
    let unknown_operator = ValidationProbe {
        expected: Some(FilterExpressionErrorKind::UnknownOperator),
    };
    let unknown_quantifier = ValidationProbe {
        expected: Some(FilterExpressionErrorKind::UnknownQuantifier),
    };
    let invalid_literal = ValidationProbe {
        expected: Some(FilterExpressionErrorKind::InvalidLiteral),
    };
    let fields = ValidationProbe::filter_fields();
    let child = Child::filter_fields().done.eq(true);

    assert!(fields.text.eq("value").matches(&valid));
    assert!(fields.text_as_bool.eq(true).matches(&unknown_operator));
    assert!(fields.maybe_text.eq("value").matches(&valid));
    assert!(fields
        .maybe_text_as_bool
        .eq(true)
        .matches(&unknown_operator));
    assert!(fields.raw.eq("value").matches(&valid));
    assert!(fields.raw_as_bool.eq(true).matches(&unknown_operator));
    assert!(fields.maybe_raw.eq("value").matches(&valid));
    assert!(fields
        .maybe_raw_as_bool
        .eq(true)
        .matches(&unknown_operator));
    assert!(fields.flag.eq(true).matches(&valid));
    assert!(fields
        .flag_as_text
        .eq("true")
        .matches(&unknown_operator));
    assert!(fields.maybe_flag.eq(true).matches(&valid));
    assert!(fields
        .maybe_flag_as_text
        .eq("true")
        .matches(&unknown_operator));
    assert!(fields
        .maybe_i8
        .eq(i128::from(i8::MAX))
        .matches(&valid));
    assert!(fields
        .maybe_i8
        .eq(i128::from(i8::MAX) + 1)
        .matches(&invalid_literal));
    assert!(fields
        .maybe_i8_as_text
        .eq("127")
        .matches(&unknown_operator));
    assert!(fields
        .maybe_u8
        .eq(u128::from(u8::MAX))
        .matches(&valid));
    assert!(fields
        .maybe_u8
        .eq(u128::from(u8::MAX) + 1)
        .matches(&invalid_literal));
    assert!(fields
        .maybe_u8_as_text
        .eq("255")
        .matches(&unknown_operator));
    assert!(fields.state.eq(State::Ready).matches(&valid));
    assert!(fields
        .state_unknown
        .eq(ProbeState::Other)
        .matches(&invalid_literal));
    assert!(fields.state_as_bool.eq(true).matches(&unknown_operator));
    assert!(fields.maybe_state.eq(State::Ready).matches(&valid));
    assert!(fields
        .maybe_state_unknown
        .eq(ProbeState::Other)
        .matches(&invalid_literal));
    assert!(fields
        .maybe_state_as_bool
        .eq(true)
        .matches(&unknown_operator));

    assert!(fields.children.any(child.clone()).matches(&valid));
    assert!(fields
        .children_as_one
        .is(child.clone())
        .matches(&unknown_quantifier));
    assert!(fields
        .children_as_bool
        .eq(true)
        .matches(&unknown_operator));
    assert!(fields.favorite.is(child.clone()).matches(&valid));
    assert!(fields
        .favorite_as_many
        .any(child)
        .matches(&unknown_quantifier));
    assert!(fields
        .favorite_as_bool
        .eq(true)
        .matches(&unknown_operator));
}
