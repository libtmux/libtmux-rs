//! Private portable-filter AST, validation, matching, and redacted debug.

use std::fmt;
#[cfg(feature = "serde")]
use std::sync::OnceLock;

use regex::Regex;

use super::grammar::{RelationQuantifier, SetOperator, TextOperator};
use super::{__private, FilterExpressionError, FilterExpressionErrorKind, Filterable};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct FieldId {
    pub(super) target: &'static str,
    pub(super) field: &'static str,
}

#[cfg(feature = "serde")]
pub(super) fn set_once_eq<T: Eq>(
    cell: &OnceLock<T>,
    value: T,
    error_kind: FilterExpressionErrorKind,
) -> Result<(), FilterExpressionError> {
    match cell.set(value) {
        Ok(()) => Ok(()),
        Err(value) if cell.get() == Some(&value) => Ok(()),
        Err(_) => Err(FilterExpressionError::new(error_kind)),
    }
}

pub(super) enum PredicateIdentity {
    Static(FieldId),
    #[cfg(feature = "serde")]
    Wire {
        target: OnceLock<&'static str>,
        field: String,
    },
}

impl PredicateIdentity {
    pub(super) fn clone_internal(&self) -> Self {
        match self {
            Self::Static(id) => Self::Static(*id),
            #[cfg(feature = "serde")]
            Self::Wire { target, field } => {
                let cloned_target = OnceLock::new();
                if let Some(target) = target.get() {
                    let _ = cloned_target.set(*target);
                }
                Self::Wire {
                    target: cloned_target,
                    field: field.clone(),
                }
            }
        }
    }

    pub(super) fn field(&self) -> &str {
        match self {
            Self::Static(id) => id.field,
            #[cfg(feature = "serde")]
            Self::Wire { field, .. } => field,
        }
    }

    pub(super) fn target(&self) -> &str {
        match self {
            Self::Static(id) => id.target,
            #[cfg(feature = "serde")]
            Self::Wire { target, .. } => target.get().copied().unwrap_or_default(),
        }
    }

    pub(super) fn bind_target(&self, target: &'static str) -> Result<(), FilterExpressionError> {
        match self {
            Self::Static(id) if id.target == target => Ok(()),
            Self::Static(_) => Err(FilterExpressionError::new(
                FilterExpressionErrorKind::InvalidTarget,
            )),
            #[cfg(feature = "serde")]
            Self::Wire {
                target: resolved, ..
            } => set_once_eq(resolved, target, FilterExpressionErrorKind::InvalidTarget),
        }
    }
}

impl PartialEq for PredicateIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.target() == other.target() && self.field() == other.field()
    }
}

impl Eq for PredicateIdentity {}

#[derive(Clone)]
pub(super) struct TextPredicate {
    pub(super) operator: TextOperator,
    pub(super) values: Vec<String>,
    pub(super) compiled_regex: Option<Regex>,
}

impl PartialEq for TextPredicate {
    fn eq(&self, other: &Self) -> bool {
        self.operator == other.operator && self.values == other.values
    }
}

impl Eq for TextPredicate {}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct SetPredicate<T> {
    pub(super) operator: SetOperator,
    pub(super) values: Vec<T>,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct RelationPredicate {
    pub(super) quantifier: RelationQuantifier,
    pub(super) expression: Box<ExprData>,
}

#[cfg(feature = "serde")]
#[derive(Clone, Eq, PartialEq)]
pub(super) enum WireStringResolved {
    Text(TextPredicate),
    Signed(SetPredicate<i128>),
    Unsigned(SetPredicate<u128>),
    Enum(SetPredicate<String>),
}

#[cfg(feature = "serde")]
pub(super) struct WireStringPredicate {
    pub(super) operator: TextOperator,
    pub(super) values: Vec<String>,
    pub(super) resolved: OnceLock<WireStringResolved>,
}

#[cfg(feature = "serde")]
impl Clone for WireStringPredicate {
    fn clone(&self) -> Self {
        let resolved = OnceLock::new();
        if let Some(value) = self.resolved.get() {
            let _ = resolved.set(value.clone());
        }
        Self {
            operator: self.operator,
            values: self.values.clone(),
            resolved,
        }
    }
}

#[cfg(feature = "serde")]
pub(super) struct WireBoolPredicate {
    pub(super) operator: SetOperator,
    pub(super) values: Vec<bool>,
    pub(super) resolved: OnceLock<()>,
}

#[cfg(feature = "serde")]
impl Clone for WireBoolPredicate {
    fn clone(&self) -> Self {
        let resolved = OnceLock::new();
        if self.resolved.get().is_some() {
            let _ = resolved.set(());
        }
        Self {
            operator: self.operator,
            values: self.values.clone(),
            resolved,
        }
    }
}

#[cfg(feature = "serde")]
#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum WireEmptyResolved {
    Text,
    Bool,
    Signed,
    Unsigned,
    Enum,
}

#[cfg(feature = "serde")]
pub(super) struct WireEmptyPredicate {
    pub(super) operator: SetOperator,
    pub(super) resolved: OnceLock<WireEmptyResolved>,
}

#[cfg(feature = "serde")]
impl Clone for WireEmptyPredicate {
    fn clone(&self) -> Self {
        let resolved = OnceLock::new();
        if let Some(value) = self.resolved.get() {
            let _ = resolved.set(*value);
        }
        Self {
            operator: self.operator,
            resolved,
        }
    }
}

/// Evaluate one operator against its operands.
///
/// Ordering compares against the single bound `has_valid_shape` requires;
/// a malformed predicate that reached here matches nothing rather than
/// panicking on an absent bound.
pub(super) fn set_matches<T: PartialOrd>(
    operator: SetOperator,
    values: &[T],
    candidate: &T,
) -> bool {
    match operator {
        SetOperator::Eq | SetOperator::In => values.contains(candidate),
        SetOperator::NotIn => !values.contains(candidate),
        SetOperator::Lt => values.first().is_some_and(|bound| candidate < bound),
        SetOperator::Lte => values.first().is_some_and(|bound| candidate <= bound),
        SetOperator::Gt => values.first().is_some_and(|bound| candidate > bound),
        SetOperator::Gte => values.first().is_some_and(|bound| candidate >= bound),
    }
}

impl<T: PartialOrd> SetPredicate<T> {
    pub(super) fn matches(&self, candidate: &T) -> bool {
        set_matches(self.operator, &self.values, candidate)
    }

    /// Report whether the operator and its operands agree.
    ///
    /// `eq` takes one value and an ordering operator takes one bound; only
    /// `in` and `not_in` take a set.
    pub(super) fn has_valid_shape(&self) -> bool {
        if self.operator == SetOperator::Eq || self.operator.is_ordering() {
            self.values.len() == 1
        } else {
            true
        }
    }
}

#[derive(Clone)]
pub(super) enum PredicateData {
    Text(TextPredicate),
    Bool(SetPredicate<bool>),
    Signed(SetPredicate<i128>),
    Unsigned(SetPredicate<u128>),
    Enum(SetPredicate<String>),
    Relation(RelationPredicate),
    #[cfg(feature = "serde")]
    WireString(WireStringPredicate),
    #[cfg(feature = "serde")]
    WireBool(WireBoolPredicate),
    #[cfg(feature = "serde")]
    WireEmpty(WireEmptyPredicate),
}

impl PredicateData {
    pub(super) fn operator_label(&self) -> &str {
        match self {
            Self::Text(predicate) => predicate.operator.label(),
            Self::Bool(predicate) => predicate.operator.label(),
            Self::Signed(predicate) => predicate.operator.label(),
            Self::Unsigned(predicate) => predicate.operator.label(),
            Self::Enum(predicate) => predicate.operator.label(),
            Self::Relation(_) => "relation",
            #[cfg(feature = "serde")]
            Self::WireString(predicate) => predicate.operator.label(),
            #[cfg(feature = "serde")]
            Self::WireBool(predicate) => predicate.operator.label(),
            #[cfg(feature = "serde")]
            Self::WireEmpty(predicate) => predicate.operator.label(),
        }
    }

    #[cfg(feature = "serde")]
    pub(super) fn scalar(&self) -> Option<ResolvedScalar<'_>> {
        match self {
            Self::Text(predicate) => Some(ResolvedScalar::Text(
                predicate.operator,
                &predicate.values,
                predicate.compiled_regex.as_ref(),
            )),
            Self::Bool(predicate) => {
                Some(ResolvedScalar::Bool(predicate.operator, &predicate.values))
            }
            Self::Signed(predicate) => Some(ResolvedScalar::Signed(
                predicate.operator,
                &predicate.values,
            )),
            Self::Unsigned(predicate) => Some(ResolvedScalar::Unsigned(
                predicate.operator,
                &predicate.values,
            )),
            Self::Enum(predicate) => {
                Some(ResolvedScalar::Enum(predicate.operator, &predicate.values))
            }
            Self::WireString(predicate) => match predicate.resolved.get()? {
                WireStringResolved::Text(value) => Some(ResolvedScalar::Text(
                    value.operator,
                    &value.values,
                    value.compiled_regex.as_ref(),
                )),
                WireStringResolved::Signed(value) => {
                    Some(ResolvedScalar::Signed(value.operator, &value.values))
                }
                WireStringResolved::Unsigned(value) => {
                    Some(ResolvedScalar::Unsigned(value.operator, &value.values))
                }
                WireStringResolved::Enum(value) => {
                    Some(ResolvedScalar::Enum(value.operator, &value.values))
                }
            },
            Self::WireBool(predicate) if predicate.resolved.get().is_some() => {
                Some(ResolvedScalar::Bool(predicate.operator, &predicate.values))
            }
            Self::WireEmpty(predicate) => match predicate.resolved.get()? {
                WireEmptyResolved::Text => Some(ResolvedScalar::Text(
                    TextOperator::from_set(predicate.operator)?,
                    &[],
                    None,
                )),
                WireEmptyResolved::Bool => Some(ResolvedScalar::Bool(predicate.operator, &[])),
                WireEmptyResolved::Signed => Some(ResolvedScalar::Signed(predicate.operator, &[])),
                WireEmptyResolved::Unsigned => {
                    Some(ResolvedScalar::Unsigned(predicate.operator, &[]))
                }
                WireEmptyResolved::Enum => Some(ResolvedScalar::Enum(predicate.operator, &[])),
            },
            Self::Relation(_) | Self::WireBool(_) => None,
        }
    }

    #[cfg(feature = "serde")]
    pub(super) fn is_resolved(&self) -> bool {
        match self {
            Self::WireString(predicate) => predicate.resolved.get().is_some(),
            Self::WireBool(predicate) => predicate.resolved.get().is_some(),
            Self::WireEmpty(predicate) => predicate.resolved.get().is_some(),
            Self::Text(_)
            | Self::Bool(_)
            | Self::Signed(_)
            | Self::Unsigned(_)
            | Self::Enum(_)
            | Self::Relation(_) => true,
        }
    }
}

#[cfg(feature = "serde")]
#[derive(Clone, Copy)]
pub(super) enum ResolvedScalar<'a> {
    Text(TextOperator, &'a [String], Option<&'a Regex>),
    Bool(SetOperator, &'a [bool]),
    Signed(SetOperator, &'a [i128]),
    Unsigned(SetOperator, &'a [u128]),
    Enum(SetOperator, &'a [String]),
}

#[cfg(feature = "serde")]
impl PartialEq for ResolvedScalar<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(left_op, left, _), Self::Text(right_op, right, _)) => {
                left_op == right_op && left == right
            }
            (Self::Bool(left_op, left), Self::Bool(right_op, right)) => {
                left_op == right_op && left == right
            }
            (Self::Signed(left_op, left), Self::Signed(right_op, right)) => {
                left_op == right_op && left == right
            }
            (Self::Unsigned(left_op, left), Self::Unsigned(right_op, right)) => {
                left_op == right_op && left == right
            }
            (Self::Enum(left_op, left), Self::Enum(right_op, right)) => {
                left_op == right_op && left == right
            }
            _ => false,
        }
    }
}

impl PartialEq for PredicateData {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Relation(left), Self::Relation(right)) => left == right,
            #[cfg(feature = "serde")]
            _ => self.scalar() == other.scalar(),
            #[cfg(not(feature = "serde"))]
            (Self::Text(left), Self::Text(right)) => left == right,
            #[cfg(not(feature = "serde"))]
            (Self::Bool(left), Self::Bool(right)) => left == right,
            #[cfg(not(feature = "serde"))]
            (Self::Signed(left), Self::Signed(right)) => left == right,
            #[cfg(not(feature = "serde"))]
            (Self::Unsigned(left), Self::Unsigned(right)) => left == right,
            #[cfg(not(feature = "serde"))]
            (Self::Enum(left), Self::Enum(right)) => left == right,
            #[cfg(not(feature = "serde"))]
            _ => false,
        }
    }
}

impl Eq for PredicateData {}

pub(super) fn evaluate<T: Filterable>(expression: &ExprData, candidate: &T) -> bool {
    match expression {
        ExprData::Predicate(predicate) => candidate.__filter_matches(predicate),
        ExprData::And(expressions) => expressions
            .iter()
            .all(|expression| evaluate(expression, candidate)),
        ExprData::Or(expressions) => expressions
            .iter()
            .any(|expression| evaluate(expression, candidate)),
        ExprData::Not(expression) => !evaluate(expression, candidate),
        #[cfg(feature = "serde")]
        ExprData::Invalid(_) => false,
    }
}

pub(super) fn validate_expression<T: Filterable>(
    expression: &ExprData,
) -> Result<(), FilterExpressionError> {
    match expression {
        ExprData::Predicate(predicate) => {
            predicate.bind_target_internal(T::FILTER_TARGET)?;
            #[cfg(feature = "serde")]
            if !valid_wire_name(predicate.field()) {
                return Err(FilterExpressionError::new(
                    FilterExpressionErrorKind::UnknownField,
                ));
            }
            T::__filter_validate(predicate)
        }
        ExprData::And(expressions) | ExprData::Or(expressions) => {
            let mut first_error = None;
            for expression in expressions {
                if let Err(error) = validate_expression::<T>(expression) {
                    first_error.get_or_insert(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        }
        ExprData::Not(expression) => validate_expression::<T>(expression),
        #[cfg(feature = "serde")]
        ExprData::Invalid(kind) => Err(FilterExpressionError::new(*kind)),
    }
}

#[cfg(feature = "serde")]
pub(super) fn valid_wire_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(feature = "serde")]
pub(super) fn validate_wire_targets(
    expression: &ExprData,
    expected_target: Option<&str>,
) -> Result<(), FilterExpressionError> {
    match expression {
        ExprData::Predicate(predicate) => {
            if !valid_wire_name(predicate.target())
                || expected_target.is_some_and(|target| predicate.target() != target)
            {
                return Err(FilterExpressionError::new(
                    FilterExpressionErrorKind::InvalidTarget,
                ));
            }
            if let Some(relation) = predicate.relation_internal() {
                validate_wire_targets(&relation.expression, None)?;
            }
            Ok(())
        }
        ExprData::And(expressions) | ExprData::Or(expressions) => {
            for expression in expressions {
                validate_wire_targets(expression, expected_target)?;
            }
            Ok(())
        }
        ExprData::Not(expression) => validate_wire_targets(expression, expected_target),
        ExprData::Invalid(_) => Ok(()),
    }
}

#[cfg(feature = "serde")]
pub(super) fn validate_wire_fields(expression: &ExprData) -> Result<(), FilterExpressionError> {
    match expression {
        ExprData::Predicate(predicate) => {
            if !valid_wire_name(predicate.field()) {
                return Err(FilterExpressionError::new(
                    FilterExpressionErrorKind::UnknownField,
                ));
            }
            if let Some(relation) = predicate.relation_internal() {
                validate_wire_fields(&relation.expression)?;
            }
            Ok(())
        }
        ExprData::And(expressions) | ExprData::Or(expressions) => {
            for expression in expressions {
                validate_wire_fields(expression)?;
            }
            Ok(())
        }
        ExprData::Not(expression) => validate_wire_fields(expression),
        ExprData::Invalid(_) => Ok(()),
    }
}

#[cfg(feature = "serde")]
pub(super) fn expression_is_resolved(expression: &ExprData) -> bool {
    match expression {
        ExprData::Predicate(predicate) => {
            predicate.is_resolved_internal()
                && predicate
                    .relation_internal()
                    .is_none_or(|relation| expression_is_resolved(&relation.expression))
        }
        ExprData::And(expressions) | ExprData::Or(expressions) => {
            expressions.iter().all(expression_is_resolved)
        }
        ExprData::Not(expression) => expression_is_resolved(expression),
        ExprData::Invalid(_) => false,
    }
}

pub(super) enum ExprData {
    Predicate(__private::Predicate),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
    #[cfg(feature = "serde")]
    Invalid(FilterExpressionErrorKind),
}

impl Clone for ExprData {
    fn clone(&self) -> Self {
        match self {
            Self::Predicate(predicate) => Self::Predicate(predicate.clone_internal()),
            Self::And(expressions) => Self::And(expressions.clone()),
            Self::Or(expressions) => Self::Or(expressions.clone()),
            Self::Not(expression) => Self::Not(expression.clone()),
            #[cfg(feature = "serde")]
            Self::Invalid(kind) => Self::Invalid(*kind),
        }
    }
}

impl PartialEq for ExprData {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Predicate(left), Self::Predicate(right)) => left.eq_internal(right),
            (Self::And(left), Self::And(right)) | (Self::Or(left), Self::Or(right)) => {
                left == right
            }
            (Self::Not(left), Self::Not(right)) => left == right,
            #[cfg(feature = "serde")]
            (Self::Invalid(left), Self::Invalid(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for ExprData {}

pub(super) struct RedactedExprDebug<'a>(pub(super) &'a ExprData);

impl fmt::Debug for RedactedExprDebug<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            ExprData::Predicate(predicate) => {
                if let Some(relation) = predicate.relation_internal() {
                    formatter
                        .debug_struct("Relation")
                        .field("target", &predicate.target())
                        .field("field", &predicate.field())
                        .field("quantifier", &relation.quantifier.label())
                        .field("expression", &RedactedExprDebug(&relation.expression))
                        .finish()
                } else {
                    formatter
                        .debug_struct("Predicate")
                        .field("target", &predicate.target())
                        .field("field", &predicate.field())
                        .field("operator", &predicate.operator_label())
                        .finish()
                }
            }
            ExprData::And(expressions) => formatter
                .debug_tuple("And")
                .field(&RedactedExprList(expressions))
                .finish(),
            ExprData::Or(expressions) => formatter
                .debug_tuple("Or")
                .field(&RedactedExprList(expressions))
                .finish(),
            ExprData::Not(expression) => formatter
                .debug_tuple("Not")
                .field(&RedactedExprDebug(expression))
                .finish(),
            #[cfg(feature = "serde")]
            ExprData::Invalid(kind) => formatter.debug_tuple("Invalid").field(kind).finish(),
        }
    }
}

struct RedactedExprList<'a>(&'a [ExprData]);

impl fmt::Debug for RedactedExprList<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entries(self.0.iter().map(RedactedExprDebug))
            .finish()
    }
}
