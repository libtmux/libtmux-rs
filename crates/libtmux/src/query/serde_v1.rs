use std::fmt;
use std::marker::PhantomData;

use serde::de::{
    self, Deserialize, Deserializer, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor,
};
use serde::ser::{Error as _, Serialize, SerializeSeq, SerializeStruct, Serializer};

use super::{
    ExprData, FilterExpr, FilterExpressionError, FilterExpressionErrorKind, Filterable,
    PredicateData, RelationPredicate, RelationQuantifier, ResolvedScalar, SetOperator,
    TextOperator, WireBoolPredicate, WireEmptyPredicate, WireStringPredicate,
    expression_is_resolved, validate_expression,
};

const VERSION: u8 = 1;

fn expression_error(kind: FilterExpressionErrorKind) -> FilterExpressionError {
    FilterExpressionError::new(kind)
}

fn invalid_structure<E: de::Error>() -> E {
    E::custom(expression_error(
        FilterExpressionErrorKind::InvalidStructure,
    ))
}

fn consume_seq<'de, A: SeqAccess<'de>>(mut sequence: A) -> Result<(), A::Error> {
    while sequence.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn consume_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

#[derive(Clone, Copy)]
enum WireVersion {
    One,
    Unsupported,
    Invalid,
}

struct VersionVisitor;

impl<'de> Visitor<'de> for VersionVisitor {
    type Value = WireVersion;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a portable filter version")
    }

    fn visit_i8<E: de::Error>(self, value: i8) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_i16<E: de::Error>(self, value: i16) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_i32<E: de::Error>(self, value: i32) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_u8<E: de::Error>(self, value: u8) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_u16<E: de::Error>(self, value: u16) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_u32<E: de::Error>(self, value: u32) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> Result<Self::Value, E> {
        Ok(if value == 1 {
            WireVersion::One
        } else {
            WireVersion::Unsupported
        })
    }

    fn visit_f32<E: de::Error>(self, value: f32) -> Result<Self::Value, E> {
        self.visit_f64(f64::from(value))
    }

    #[allow(clippy::float_cmp)]
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(if value == 1.0 {
            WireVersion::One
        } else if value.is_finite() && value.fract() == 0.0 {
            WireVersion::Unsupported
        } else {
            WireVersion::Invalid
        })
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(WireVersion::Invalid)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(WireVersion::Invalid)
    }

    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Ok(WireVersion::Invalid)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(WireVersion::Invalid)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(WireVersion::Invalid)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, sequence: A) -> Result<Self::Value, A::Error> {
        consume_seq(sequence)?;
        Ok(WireVersion::Invalid)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        consume_map(map)?;
        Ok(WireVersion::Invalid)
    }
}

impl<'de> Deserialize<'de> for WireVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(VersionVisitor)
    }
}

enum WireValue {
    String(String),
    Bool(bool),
    Strings(Vec<String>),
    Bools(Vec<bool>),
    Empty,
    Other,
}

enum WireElement {
    String(String),
    Bool(bool),
    Other,
}

struct WireElementVisitor;

impl<'de> Visitor<'de> for WireElementVisitor {
    type Value = WireElement;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a portable scalar literal")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(WireElement::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(WireElement::String(value))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(WireElement::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(WireElement::Other)
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<Self::Value, E> {
        Ok(WireElement::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(WireElement::Other)
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<Self::Value, E> {
        Ok(WireElement::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(WireElement::Other)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(WireElement::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(WireElement::Other)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, sequence: A) -> Result<Self::Value, A::Error> {
        consume_seq(sequence)?;
        Ok(WireElement::Other)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        consume_map(map)?;
        Ok(WireElement::Other)
    }
}

impl<'de> Deserialize<'de> for WireElement {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(WireElementVisitor)
    }
}

struct WireValueVisitor;

impl<'de> Visitor<'de> for WireValueVisitor {
    type Value = WireValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a portable wire value")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(WireValue::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(WireValue::String(value))
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(WireValue::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(WireValue::Other)
    }

    fn visit_i128<E: de::Error>(self, _: i128) -> Result<Self::Value, E> {
        Ok(WireValue::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(WireValue::Other)
    }

    fn visit_u128<E: de::Error>(self, _: u128) -> Result<Self::Value, E> {
        Ok(WireValue::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(WireValue::Other)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(WireValue::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(WireValue::Other)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut strings = Vec::new();
        let mut booleans = Vec::new();
        let mut family = None;
        let mut invalid = false;
        while let Some(element) = sequence.next_element::<WireElement>()? {
            match element {
                WireElement::String(value) => {
                    if family == Some(false) {
                        invalid = true;
                    }
                    family = Some(true);
                    strings.push(value);
                }
                WireElement::Bool(value) => {
                    if family == Some(true) {
                        invalid = true;
                    }
                    family = Some(false);
                    booleans.push(value);
                }
                WireElement::Other => invalid = true,
            }
        }
        Ok(if invalid {
            WireValue::Other
        } else {
            match family {
                None => WireValue::Empty,
                Some(true) => WireValue::Strings(strings),
                Some(false) => WireValue::Bools(booleans),
            }
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        consume_map(map)?;
        Ok(WireValue::Other)
    }
}

impl<'de> Deserialize<'de> for WireValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(WireValueVisitor)
    }
}

struct RawNode {
    invalid_member: bool,
    op: Option<WireValue>,
    args: Option<RawArgs>,
    expression: Option<RawExpression>,
    field: Option<WireValue>,
    value: Option<WireValue>,
    quantifier: Option<WireValue>,
}

enum RawArgs {
    Nodes(Vec<RawNode>),
    Other,
}

enum RawExpression {
    Node(Box<RawNode>),
    Other,
}

struct RawNodeVisitor;

impl RawNode {
    fn from_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<Self, A::Error> {
        let mut node = Self {
            invalid_member: false,
            op: None,
            args: None,
            expression: None,
            field: None,
            value: None,
            quantifier: None,
        };
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "op" if node.op.is_none() => node.op = Some(map.next_value()?),
                "args" if node.args.is_none() => node.args = Some(map.next_value()?),
                "expr" if node.expression.is_none() => {
                    node.expression = Some(map.next_value()?);
                }
                "field" if node.field.is_none() => node.field = Some(map.next_value()?),
                "value" if node.value.is_none() => node.value = Some(map.next_value()?),
                "quantifier" if node.quantifier.is_none() => {
                    node.quantifier = Some(map.next_value()?);
                }
                _ => {
                    node.invalid_member = true;
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(node)
    }

    fn operator(&self) -> Option<&str> {
        match self.op.as_ref()? {
            WireValue::String(operator) => Some(operator),
            _ => None,
        }
    }

    fn has_only_scalar_members(&self) -> bool {
        self.args.is_none() && self.expression.is_none() && self.quantifier.is_none()
    }

    fn has_only_args_members(&self) -> bool {
        self.expression.is_none()
            && self.field.is_none()
            && self.value.is_none()
            && self.quantifier.is_none()
    }

    fn has_only_expression_members(&self) -> bool {
        self.args.is_none()
            && self.field.is_none()
            && self.value.is_none()
            && self.quantifier.is_none()
    }

    fn has_only_relation_members(&self) -> bool {
        self.args.is_none() && self.value.is_none()
    }

    fn validate_string(value: Option<&WireValue>) -> bool {
        matches!(value, Some(WireValue::String(_)))
    }

    fn validate_nested(expression: Option<&RawExpression>) -> bool {
        match expression {
            Some(RawExpression::Node(expression)) => expression.validate_structure().is_ok(),
            Some(RawExpression::Other) | None => false,
        }
    }

    fn validate_args(args: Option<&RawArgs>) -> bool {
        match args {
            Some(RawArgs::Nodes(expressions)) if expressions.len() >= 2 => expressions
                .iter()
                .all(|expression| expression.validate_structure().is_ok()),
            Some(RawArgs::Nodes(_) | RawArgs::Other) | None => false,
        }
    }

    fn validate_scalar_value(&self, operator: &str) -> bool {
        matches!(
            (operator, self.value.as_ref()),
            ("eq", Some(WireValue::String(_) | WireValue::Bool(_)))
                | (
                    "in" | "not_in",
                    Some(WireValue::Strings(_) | WireValue::Bools(_) | WireValue::Empty),
                )
                | (
                    "eq_ignore_case"
                        | "contains"
                        | "contains_ignore_case"
                        | "starts_with"
                        | "starts_with_ignore_case"
                        | "ends_with"
                        | "ends_with_ignore_case"
                        | "regex"
                        | "regex_ignore_case",
                    Some(WireValue::String(_)),
                )
        )
    }

    fn validate_unknown_shape(&self) -> bool {
        if self.has_only_scalar_members()
            && Self::validate_string(self.field.as_ref())
            && matches!(
                self.value,
                Some(
                    WireValue::String(_)
                        | WireValue::Bool(_)
                        | WireValue::Strings(_)
                        | WireValue::Bools(_)
                        | WireValue::Empty
                )
            )
        {
            true
        } else if self.has_only_args_members() {
            Self::validate_args(self.args.as_ref())
        } else if self.has_only_expression_members()
            || (self.has_only_relation_members()
                && Self::validate_string(self.field.as_ref())
                && Self::validate_string(self.quantifier.as_ref()))
        {
            Self::validate_nested(self.expression.as_ref())
        } else {
            false
        }
    }

    fn validate_structure(&self) -> Result<(), FilterExpressionError> {
        if self.invalid_member {
            return Err(expression_error(
                FilterExpressionErrorKind::InvalidStructure,
            ));
        }
        let Some(operator) = self.operator() else {
            return Err(expression_error(
                FilterExpressionErrorKind::InvalidStructure,
            ));
        };
        let valid = match operator {
            "and" | "or" => self.has_only_args_members() && Self::validate_args(self.args.as_ref()),
            "not" => {
                self.has_only_expression_members()
                    && Self::validate_nested(self.expression.as_ref())
            }
            "relation" => {
                self.has_only_relation_members()
                    && Self::validate_string(self.field.as_ref())
                    && Self::validate_string(self.quantifier.as_ref())
                    && Self::validate_nested(self.expression.as_ref())
            }
            "eq"
            | "eq_ignore_case"
            | "contains"
            | "contains_ignore_case"
            | "starts_with"
            | "starts_with_ignore_case"
            | "ends_with"
            | "ends_with_ignore_case"
            | "in"
            | "not_in"
            | "regex"
            | "regex_ignore_case" => {
                self.has_only_scalar_members()
                    && Self::validate_string(self.field.as_ref())
                    && self.validate_scalar_value(operator)
            }
            _ => self.validate_unknown_shape(),
        };
        if valid {
            Ok(())
        } else {
            Err(expression_error(
                FilterExpressionErrorKind::InvalidStructure,
            ))
        }
    }

    fn take_string(value: Option<WireValue>) -> Result<String, FilterExpressionError> {
        match value {
            Some(WireValue::String(value)) => Ok(value),
            _ => Err(expression_error(
                FilterExpressionErrorKind::InvalidStructure,
            )),
        }
    }

    fn take_nested(value: Option<RawExpression>) -> Result<RawNode, FilterExpressionError> {
        match value {
            Some(RawExpression::Node(value)) => Ok(*value),
            Some(RawExpression::Other) | None => Err(expression_error(
                FilterExpressionErrorKind::InvalidStructure,
            )),
        }
    }

    fn into_junction(self, operator: &str) -> Result<ExprData, FilterExpressionError> {
        let RawArgs::Nodes(nodes) = self
            .args
            .ok_or_else(|| expression_error(FilterExpressionErrorKind::InvalidStructure))?
        else {
            return Err(expression_error(
                FilterExpressionErrorKind::InvalidStructure,
            ));
        };
        let mut expressions = Vec::new();
        for node in nodes {
            let expression = match node.into_expression() {
                Ok(expression) => expression,
                Err(error) => ExprData::Invalid(error.kind()),
            };
            match (operator, expression) {
                ("and", ExprData::And(nested)) | ("or", ExprData::Or(nested)) => {
                    expressions.extend(nested);
                }
                (_, expression) => expressions.push(expression),
            }
        }
        Ok(if operator == "and" {
            ExprData::And(expressions)
        } else {
            ExprData::Or(expressions)
        })
    }

    fn into_relation(self) -> Result<ExprData, FilterExpressionError> {
        let quantifier = match Self::take_string(self.quantifier)?.as_str() {
            "any" => RelationQuantifier::Any,
            "all" => RelationQuantifier::All,
            "none" => RelationQuantifier::None,
            "is" => RelationQuantifier::Is,
            _ => {
                return Err(expression_error(
                    FilterExpressionErrorKind::UnknownQuantifier,
                ));
            }
        };
        let field = Self::take_string(self.field)?;
        if !super::valid_wire_name(&field) {
            return Err(expression_error(FilterExpressionErrorKind::UnknownField));
        }
        let expression = match Self::take_nested(self.expression)?.into_expression() {
            Ok(expression) => expression,
            Err(error) => ExprData::Invalid(error.kind()),
        };
        Ok(ExprData::Predicate(super::__private::Predicate::from_wire(
            field,
            PredicateData::Relation(RelationPredicate {
                quantifier,
                expression: Box::new(expression),
            }),
        )))
    }

    fn into_scalar(self, operator: &str) -> Result<ExprData, FilterExpressionError> {
        let Some(wire_operator) = parse_scalar_operator(operator) else {
            return Err(expression_error(FilterExpressionErrorKind::UnknownOperator));
        };
        let field = Self::take_string(self.field)?;
        if !super::valid_wire_name(&field) {
            return Err(expression_error(FilterExpressionErrorKind::UnknownField));
        }
        let data = match self.value {
            Some(WireValue::String(value)) => PredicateData::WireString(WireStringPredicate {
                operator: wire_operator,
                values: vec![value],
                resolved: super::OnceLock::new(),
            }),
            Some(WireValue::Strings(values)) => PredicateData::WireString(WireStringPredicate {
                operator: wire_operator,
                values,
                resolved: super::OnceLock::new(),
            }),
            Some(WireValue::Bool(value)) => PredicateData::WireBool(WireBoolPredicate {
                operator: wire_operator
                    .set_operator()
                    .ok_or_else(|| expression_error(FilterExpressionErrorKind::InvalidStructure))?,
                values: vec![value],
                resolved: super::OnceLock::new(),
            }),
            Some(WireValue::Bools(values)) => PredicateData::WireBool(WireBoolPredicate {
                operator: wire_operator
                    .set_operator()
                    .ok_or_else(|| expression_error(FilterExpressionErrorKind::InvalidStructure))?,
                values,
                resolved: super::OnceLock::new(),
            }),
            Some(WireValue::Empty) => PredicateData::WireEmpty(WireEmptyPredicate {
                operator: wire_operator
                    .set_operator()
                    .ok_or_else(|| expression_error(FilterExpressionErrorKind::InvalidStructure))?,
                resolved: super::OnceLock::new(),
            }),
            Some(WireValue::Other) | None => {
                return Err(expression_error(
                    FilterExpressionErrorKind::InvalidStructure,
                ));
            }
        };
        Ok(ExprData::Predicate(super::__private::Predicate::from_wire(
            field, data,
        )))
    }

    fn into_expression(self) -> Result<ExprData, FilterExpressionError> {
        let operator = match self.op.as_ref() {
            Some(WireValue::String(operator)) => operator.clone(),
            _ => {
                return Err(expression_error(
                    FilterExpressionErrorKind::InvalidStructure,
                ));
            }
        };
        match operator.as_str() {
            "and" | "or" => self.into_junction(&operator),
            "not" => Ok(ExprData::Not(Box::new(
                Self::take_nested(self.expression)?.into_expression()?,
            ))),
            "relation" => self.into_relation(),
            _ => self.into_scalar(&operator),
        }
    }
}

impl<'de> Visitor<'de> for RawNodeVisitor {
    type Value = RawNode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a portable filter expression object")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        RawNode::from_map(map)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, sequence: A) -> Result<Self::Value, A::Error> {
        consume_seq(sequence)?;
        Err(invalid_structure())
    }
}

impl<'de> Deserialize<'de> for RawNode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RawNodeVisitor)
    }
}

struct RawArgsVisitor;

impl<'de> Visitor<'de> for RawArgsVisitor {
    type Value = RawArgs;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an expression array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut nodes = Vec::new();
        let mut invalid = false;
        while let Some(expression) = sequence.next_element::<RawExpression>()? {
            match expression {
                RawExpression::Node(node) => nodes.push(*node),
                RawExpression::Other => invalid = true,
            }
        }
        Ok(if invalid {
            RawArgs::Other
        } else {
            RawArgs::Nodes(nodes)
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        consume_map(map)?;
        Ok(RawArgs::Other)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }

    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(RawArgs::Other)
    }
}

impl<'de> Deserialize<'de> for RawArgs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RawArgsVisitor)
    }
}

struct RawExpressionVisitor;

impl<'de> Visitor<'de> for RawExpressionVisitor {
    type Value = RawExpression;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an expression object")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        RawNode::from_map(map).map(|node| RawExpression::Node(Box::new(node)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, sequence: A) -> Result<Self::Value, A::Error> {
        consume_seq(sequence)?;
        Ok(RawExpression::Other)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }

    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(RawExpression::Other)
    }
}

impl<'de> Deserialize<'de> for RawExpression {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RawExpressionVisitor)
    }
}

struct RawEnvelope {
    invalid_member: bool,
    version: Option<WireVersion>,
    target: Option<WireValue>,
    expression: Option<RawExpression>,
}

struct EnvelopeVisitor<T>(PhantomData<fn() -> T>);

impl<'de, T: Filterable> Visitor<'de> for EnvelopeVisitor<T> {
    type Value = FilterExpr<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a version 1 portable filter envelope")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut raw = RawEnvelope {
            invalid_member: false,
            version: None,
            target: None,
            expression: None,
        };
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" if raw.version.is_none() => raw.version = Some(map.next_value()?),
                "target" if raw.target.is_none() => raw.target = Some(map.next_value()?),
                "expr" if raw.expression.is_none() => {
                    raw.expression = Some(map.next_value()?);
                }
                _ => {
                    raw.invalid_member = true;
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        if raw.invalid_member
            || raw.version.is_none()
            || !matches!(raw.target, Some(WireValue::String(_)))
            || !matches!(raw.expression, Some(RawExpression::Node(_)))
        {
            return Err(invalid_structure());
        }
        let Some(RawExpression::Node(expression)) = raw.expression.as_ref() else {
            return Err(invalid_structure());
        };
        expression.validate_structure().map_err(A::Error::custom)?;

        match raw.version {
            Some(WireVersion::One) => {}
            Some(WireVersion::Unsupported) => {
                return Err(A::Error::custom(expression_error(
                    FilterExpressionErrorKind::UnsupportedVersion,
                )));
            }
            Some(WireVersion::Invalid) | None => return Err(invalid_structure()),
        }

        let Some(WireValue::String(target)) = raw.target else {
            return Err(invalid_structure());
        };
        if !super::valid_wire_name(&target) || target != T::FILTER_TARGET {
            return Err(A::Error::custom(expression_error(
                FilterExpressionErrorKind::InvalidTarget,
            )));
        }
        let expression = match raw.expression {
            Some(RawExpression::Node(expression)) => *expression,
            Some(RawExpression::Other) | None => return Err(invalid_structure()),
        };
        let data = expression.into_expression().map_err(A::Error::custom)?;
        validate_expression::<T>(&data).map_err(A::Error::custom)?;
        if !expression_is_resolved(&data) {
            return Err(invalid_structure());
        }
        Ok(FilterExpr {
            data,
            marker: PhantomData,
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, sequence: A) -> Result<Self::Value, A::Error> {
        consume_seq(sequence)?;
        Err(invalid_structure())
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }

    fn visit_string<E: de::Error>(self, _: String) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }

    fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Err(invalid_structure())
    }
}

impl<'de, T: Filterable> Deserialize<'de> for FilterExpr<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(EnvelopeVisitor(PhantomData))
    }
}

fn parse_scalar_operator(value: &str) -> Option<TextOperator> {
    Some(match value {
        "eq" => TextOperator::Eq,
        "eq_ignore_case" => TextOperator::EqIgnoreCase,
        "contains" => TextOperator::Contains,
        "contains_ignore_case" => TextOperator::ContainsIgnoreCase,
        "starts_with" => TextOperator::StartsWith,
        "starts_with_ignore_case" => TextOperator::StartsWithIgnoreCase,
        "ends_with" => TextOperator::EndsWith,
        "ends_with_ignore_case" => TextOperator::EndsWithIgnoreCase,
        "in" => TextOperator::In,
        "not_in" => TextOperator::NotIn,
        "regex" => TextOperator::Regex,
        "regex_ignore_case" => TextOperator::RegexIgnoreCase,
        "lt" => TextOperator::Lt,
        "lte" => TextOperator::Lte,
        "gt" => TextOperator::Gt,
        "gte" => TextOperator::Gte,
        _ => return None,
    })
}

struct ExpressionRef<'a>(&'a ExprData);

impl Serialize for ExpressionRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            ExprData::And(expressions) | ExprData::Or(expressions) => {
                let mut state = serializer.serialize_struct("FilterJunction", 2)?;
                state.serialize_field(
                    "op",
                    if matches!(self.0, ExprData::And(_)) {
                        "and"
                    } else {
                        "or"
                    },
                )?;
                state.serialize_field("args", &ExpressionList(expressions))?;
                state.end()
            }
            ExprData::Not(expression) => {
                let mut state = serializer.serialize_struct("FilterNot", 2)?;
                state.serialize_field("op", "not")?;
                state.serialize_field("expr", &ExpressionRef(expression))?;
                state.end()
            }
            ExprData::Predicate(predicate) => {
                if let Some(relation) = predicate.relation_internal() {
                    let mut state = serializer.serialize_struct("FilterRelation", 4)?;
                    state.serialize_field("op", "relation")?;
                    state.serialize_field("field", predicate.field())?;
                    state.serialize_field("quantifier", relation.quantifier.label())?;
                    state.serialize_field("expr", &ExpressionRef(&relation.expression))?;
                    state.end()
                } else {
                    let scalar = predicate.scalar_internal().ok_or_else(|| {
                        S::Error::custom(expression_error(
                            FilterExpressionErrorKind::InvalidStructure,
                        ))
                    })?;
                    let mut state = serializer.serialize_struct("FilterScalar", 3)?;
                    state.serialize_field("op", predicate.operator_label())?;
                    state.serialize_field("field", predicate.field())?;
                    state.serialize_field("value", &ScalarValue(scalar))?;
                    state.end()
                }
            }
            ExprData::Invalid(kind) => Err(S::Error::custom(expression_error(*kind))),
        }
    }
}

struct ExpressionList<'a>(&'a [ExprData]);

impl Serialize for ExpressionList<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for expression in self.0 {
            sequence.serialize_element(&ExpressionRef(expression))?;
        }
        sequence.end()
    }
}

struct ScalarValue<'a>(ResolvedScalar<'a>);

impl Serialize for ScalarValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            ResolvedScalar::Text(operator, values, _) => {
                if matches!(operator, TextOperator::In | TextOperator::NotIn) {
                    values.serialize(serializer)
                } else {
                    values
                        .first()
                        .ok_or_else(|| {
                            S::Error::custom(expression_error(
                                FilterExpressionErrorKind::InvalidStructure,
                            ))
                        })?
                        .serialize(serializer)
                }
            }
            ResolvedScalar::Bool(operator, values) => {
                if matches!(operator, SetOperator::In | SetOperator::NotIn) {
                    values.serialize(serializer)
                } else {
                    values
                        .first()
                        .ok_or_else(|| {
                            S::Error::custom(expression_error(
                                FilterExpressionErrorKind::InvalidStructure,
                            ))
                        })?
                        .serialize(serializer)
                }
            }
            ResolvedScalar::Signed(operator, values) => {
                IntegerValue::Signed(operator, values).serialize(serializer)
            }
            ResolvedScalar::Unsigned(operator, values) => {
                IntegerValue::Unsigned(operator, values).serialize(serializer)
            }
            ResolvedScalar::Enum(operator, values) => {
                if matches!(operator, SetOperator::In | SetOperator::NotIn) {
                    values.serialize(serializer)
                } else {
                    values
                        .first()
                        .ok_or_else(|| {
                            S::Error::custom(expression_error(
                                FilterExpressionErrorKind::InvalidStructure,
                            ))
                        })?
                        .serialize(serializer)
                }
            }
        }
    }
}

enum IntegerValue<'a> {
    Signed(SetOperator, &'a [i128]),
    Unsigned(SetOperator, &'a [u128]),
}

impl Serialize for IntegerValue<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // A single-valued operator rides as a scalar; only `in` and
            // `not_in` carry a list.
            Self::Signed(operator, values) if !operator.takes_a_set() => values
                .first()
                .ok_or_else(|| {
                    S::Error::custom(expression_error(
                        FilterExpressionErrorKind::InvalidStructure,
                    ))
                })?
                .to_string()
                .serialize(serializer),
            Self::Unsigned(operator, values) if !operator.takes_a_set() => values
                .first()
                .ok_or_else(|| {
                    S::Error::custom(expression_error(
                        FilterExpressionErrorKind::InvalidStructure,
                    ))
                })?
                .to_string()
                .serialize(serializer),
            Self::Signed(_, values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in *values {
                    sequence.serialize_element(&value.to_string())?;
                }
                sequence.end()
            }
            Self::Unsigned(_, values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in *values {
                    sequence.serialize_element(&value.to_string())?;
                }
                sequence.end()
            }
        }
    }
}

impl<T: Filterable> Serialize for FilterExpr<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if !super::valid_wire_name(T::FILTER_TARGET) {
            return Err(S::Error::custom(expression_error(
                FilterExpressionErrorKind::InvalidTarget,
            )));
        }
        super::validate_wire_targets(&self.data, Some(T::FILTER_TARGET))
            .map_err(S::Error::custom)?;
        super::validate_wire_fields(&self.data).map_err(S::Error::custom)?;
        validate_expression::<T>(&self.data).map_err(S::Error::custom)?;
        if !expression_is_resolved(&self.data) {
            return Err(S::Error::custom(expression_error(
                FilterExpressionErrorKind::InvalidStructure,
            )));
        }
        let mut state = serializer.serialize_struct("FilterEnvelope", 3)?;
        state.serialize_field("version", &VERSION)?;
        state.serialize_field("target", T::FILTER_TARGET)?;
        state.serialize_field("expr", &ExpressionRef(&self.data))?;
        state.end()
    }
}
