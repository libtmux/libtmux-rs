use super::{
    BoolField, EnumField, FieldId, FilterEnum, FilterExpressionError, FilterExpressionErrorKind,
    Filterable, IntegerField, ManyRelation, OneRelation, PhantomData, PredicateData,
    PredicateIdentity, RelationPredicate, RelationQuantifier, SetOperator, TextField, TextOperator,
    default_case_fold_str, evaluate, validate_expression,
};

#[doc(hidden)]
pub use super::schema::{
    FilterFieldSchema, FilterSchemaDescriptor, FilterValueSchema, filter_schema,
};

#[cfg(feature = "serde")]
use super::{ResolvedScalar, WireEmptyResolved, WireStringResolved};

/// The fixed-width integer types understood by generated schema checks.
///
/// # Examples
///
/// ```
/// use libtmux::query::__private::IntegerKind;
///
/// let signed = IntegerKind::I8;
/// let unsigned = IntegerKind::U8;
/// let _ = (signed, unsigned);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IntegerKind {
    /// `i8`.
    I8,
    /// `i16`.
    I16,
    /// `i32`.
    I32,
    /// `i64`.
    I64,
    /// `i128`.
    I128,
    /// `u8`.
    U8,
    /// `u16`.
    U16,
    /// `u32`.
    U32,
    /// `u64`.
    U64,
    /// `u128`.
    U128,
}

impl IntegerKind {
    fn signed_bounds(self) -> Option<(i128, i128)> {
        match self {
            Self::I8 => Some((i128::from(i8::MIN), i128::from(i8::MAX))),
            Self::I16 => Some((i128::from(i16::MIN), i128::from(i16::MAX))),
            Self::I32 => Some((i128::from(i32::MIN), i128::from(i32::MAX))),
            Self::I64 => Some((i128::from(i64::MIN), i128::from(i64::MAX))),
            Self::I128 => Some((i128::MIN, i128::MAX)),
            Self::U8 | Self::U16 | Self::U32 | Self::U64 | Self::U128 => None,
        }
    }

    fn unsigned_max(self) -> Option<u128> {
        match self {
            Self::U8 => Some(u128::from(u8::MAX)),
            Self::U16 => Some(u128::from(u16::MAX)),
            Self::U32 => Some(u128::from(u32::MAX)),
            Self::U64 => Some(u128::from(u64::MAX)),
            Self::U128 => Some(u128::MAX),
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::I128 => None,
        }
    }
}

/// One opaque predicate passed to generated candidate dispatch.
///
/// Downstream code can inspect only the stable field name and call the
/// typed matching or validation helpers.
///
/// The [`crate::query::Filterable`] example executes every scalar helper
/// through a hand-written generated-code expansion.
pub struct Predicate {
    id: PredicateIdentity,
    data: PredicateData,
}

// The field name, not the operand. A predicate carries values a caller
// filtered on, which are tmux data rather than this crate's to disclose.
impl core::fmt::Debug for Predicate {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Predicate")
            .field("field", &self.field())
            .finish_non_exhaustive()
    }
}

impl Predicate {
    pub(super) fn new(id: FieldId, data: PredicateData) -> Self {
        Self {
            id: PredicateIdentity::Static(id),
            data,
        }
    }

    #[cfg(feature = "serde")]
    pub(super) fn from_wire(field: String, data: PredicateData) -> Self {
        Self {
            id: PredicateIdentity::Wire {
                target: super::OnceLock::new(),
                field,
            },
            data,
        }
    }

    pub(super) fn clone_internal(&self) -> Self {
        Self {
            id: self.id.clone_internal(),
            data: self.data.clone(),
        }
    }

    pub(super) fn eq_internal(&self, other: &Self) -> bool {
        self.id == other.id && self.data == other.data
    }

    pub(super) fn target(&self) -> &str {
        self.id.target()
    }

    pub(super) fn operator_label(&self) -> &str {
        self.data.operator_label()
    }

    pub(super) fn bind_target_internal(
        &self,
        target: &'static str,
    ) -> Result<(), FilterExpressionError> {
        self.id.bind_target(target)
    }

    pub(super) fn relation_internal(&self) -> Option<&RelationPredicate> {
        match &self.data {
            PredicateData::Relation(relation) => Some(relation),
            _ => None,
        }
    }

    #[cfg(feature = "serde")]
    pub(super) fn scalar_internal(&self) -> Option<ResolvedScalar<'_>> {
        self.data.scalar()
    }

    #[cfg(feature = "serde")]
    pub(super) fn is_resolved_internal(&self) -> bool {
        self.data.is_resolved()
    }

    /// Return the stable field name used by generated dispatch.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    #[must_use]
    pub fn field(&self) -> &str {
        self.id.field()
    }

    /// Match a strict UTF-8 candidate with this text predicate.
    ///
    /// Invalid UTF-8 returns `false` before every operator, including
    /// exclusion from an empty set.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    #[must_use]
    pub fn matches_text(&self, candidate: &[u8]) -> bool {
        let Ok(candidate) = std::str::from_utf8(candidate) else {
            return false;
        };
        #[cfg(feature = "serde")]
        let Some(ResolvedScalar::Text(operator, values, compiled_regex)) = self.data.scalar()
        else {
            return false;
        };
        #[cfg(not(feature = "serde"))]
        let PredicateData::Text(predicate) = &self.data else {
            return false;
        };
        #[cfg(not(feature = "serde"))]
        let (operator, values, compiled_regex) = (
            predicate.operator,
            predicate.values.as_slice(),
            predicate.compiled_regex.as_ref(),
        );
        let value = values.first().map(String::as_str).unwrap_or_default();
        match operator {
            TextOperator::Eq => candidate == value,
            TextOperator::EqIgnoreCase => {
                default_case_fold_str(candidate) == default_case_fold_str(value)
            }
            TextOperator::Contains => candidate.contains(value),
            TextOperator::ContainsIgnoreCase => {
                default_case_fold_str(candidate).contains(&default_case_fold_str(value))
            }
            TextOperator::StartsWith => candidate.starts_with(value),
            TextOperator::StartsWithIgnoreCase => {
                default_case_fold_str(candidate).starts_with(&default_case_fold_str(value))
            }
            TextOperator::EndsWith => candidate.ends_with(value),
            TextOperator::EndsWithIgnoreCase => {
                default_case_fold_str(candidate).ends_with(&default_case_fold_str(value))
            }
            TextOperator::In => values.iter().any(|value| value == candidate),
            TextOperator::NotIn => values.iter().all(|value| value != candidate),
            TextOperator::Regex | TextOperator::RegexIgnoreCase => {
                compiled_regex.is_some_and(|regex| regex.is_match(candidate))
            }
            // Text has no ordering. Validation rejects these before a
            // text field ever sees one, so nothing matches here.
            TextOperator::Lt | TextOperator::Lte | TextOperator::Gt | TextOperator::Gte => false,
        }
    }

    /// Match a boolean candidate with this boolean predicate.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    #[must_use]
    pub fn matches_bool(&self, candidate: bool) -> bool {
        #[cfg(feature = "serde")]
        if let Some(ResolvedScalar::Bool(operator, values)) = self.data.scalar() {
            return set_matches(operator, values, &candidate);
        }
        match &self.data {
            PredicateData::Bool(predicate) => predicate.matches(&candidate),
            _ => false,
        }
    }

    /// Match a signed candidate widened without loss to `i128`.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    #[must_use]
    pub fn matches_signed(&self, candidate: i128) -> bool {
        #[cfg(feature = "serde")]
        if let Some(ResolvedScalar::Signed(operator, values)) = self.data.scalar() {
            return set_matches(operator, values, &candidate);
        }
        match &self.data {
            PredicateData::Signed(predicate) => predicate.matches(&candidate),
            _ => false,
        }
    }

    /// Match an unsigned candidate widened without loss to `u128`.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    #[must_use]
    pub fn matches_unsigned(&self, candidate: u128) -> bool {
        #[cfg(feature = "serde")]
        if let Some(ResolvedScalar::Unsigned(operator, values)) = self.data.scalar() {
            return set_matches(operator, values, &candidate);
        }
        match &self.data {
            PredicateData::Unsigned(predicate) => predicate.matches(&candidate),
            _ => false,
        }
    }

    /// Match a custom enum candidate's stable filter string.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    #[must_use]
    pub fn matches_enum(&self, candidate: &str) -> bool {
        #[cfg(feature = "serde")]
        if let Some(ResolvedScalar::Enum(operator, values)) = self.data.scalar() {
            return values.iter().any(|value| value == candidate)
                != (operator == SetOperator::NotIn);
        }
        match &self.data {
            PredicateData::Enum(predicate) => {
                predicate.values.iter().any(|value| value == candidate)
                    != (predicate.operator == SetOperator::NotIn)
            }
            _ => false,
        }
    }

    /// Match this relation predicate against already-loaded related values.
    ///
    /// `any`, `all`, and `none` use the standard iterator truth table and
    /// short-circuit from left to right. Other predicate families and the
    /// to-one `is` quantifier return `false`.
    ///
    /// The [`crate::query::Filterable`] relation example executes this
    /// method in a hand-written generated-code expansion.
    #[must_use]
    pub fn matches_many<U: Filterable>(&self, values: &[U]) -> bool {
        let PredicateData::Relation(relation) = &self.data else {
            return false;
        };
        match relation.quantifier {
            RelationQuantifier::Any => values
                .iter()
                .any(|candidate| evaluate(&relation.expression, candidate)),
            RelationQuantifier::All => values
                .iter()
                .all(|candidate| evaluate(&relation.expression, candidate)),
            RelationQuantifier::None => values
                .iter()
                .all(|candidate| !evaluate(&relation.expression, candidate)),
            RelationQuantifier::Is => false,
        }
    }

    /// Match this relation predicate against one already-loaded value.
    ///
    /// The `is` quantifier evaluates an existing value. An absent value,
    /// another quantifier, or another predicate family returns `false`.
    ///
    /// The [`crate::query::Filterable`] relation example executes this
    /// method in a hand-written generated-code expansion.
    #[must_use]
    pub fn matches_one<U: Filterable>(&self, value: Option<&U>) -> bool {
        let PredicateData::Relation(relation) = &self.data else {
            return false;
        };
        match (relation.quantifier, value) {
            (RelationQuantifier::Is, Some(candidate)) => evaluate(&relation.expression, candidate),
            _ => false,
        }
    }

    /// Validate that this predicate is a well-formed text predicate.
    ///
    /// # Errors
    ///
    /// Returns [`FilterExpressionErrorKind::UnknownOperator`] for another
    /// scalar family and [`FilterExpressionErrorKind::InvalidStructure`]
    /// for an invalid text value or compiled-regex shape.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    pub fn validate_text(&self) -> Result<(), FilterExpressionError> {
        match &self.data {
            PredicateData::Text(predicate) => validate_text_shape(predicate),
            #[cfg(feature = "serde")]
            PredicateData::WireString(predicate) => {
                let compiled_regex = if predicate.operator.is_regex() {
                    let Some(pattern) = predicate.values.first() else {
                        return validation_error(FilterExpressionErrorKind::InvalidStructure);
                    };
                    Some(
                        super::RegexBuilder::new(pattern)
                            .case_insensitive(predicate.operator == TextOperator::RegexIgnoreCase)
                            .build()
                            .map_err(|_| {
                                FilterExpressionError::new(FilterExpressionErrorKind::InvalidRegex)
                            })?,
                    )
                } else {
                    None
                };
                let resolved = super::TextPredicate {
                    operator: predicate.operator,
                    values: predicate.values.clone(),
                    compiled_regex,
                };
                validate_text_shape(&resolved)?;
                resolve_once(&predicate.resolved, WireStringResolved::Text(resolved))
            }
            #[cfg(feature = "serde")]
            PredicateData::WireBool(_) => {
                validation_error(FilterExpressionErrorKind::InvalidLiteral)
            }
            #[cfg(feature = "serde")]
            PredicateData::WireEmpty(predicate) => {
                validate_non_ordering(predicate.operator)?;
                resolve_once(&predicate.resolved, WireEmptyResolved::Text)
            }
            _ => validation_error(FilterExpressionErrorKind::UnknownOperator),
        }
    }

    /// Validate that this predicate is a well-formed boolean predicate.
    ///
    /// # Errors
    ///
    /// Returns [`FilterExpressionErrorKind::UnknownOperator`] for another
    /// scalar family and [`FilterExpressionErrorKind::InvalidStructure`]
    /// for an invalid equality shape.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    pub fn validate_bool(&self) -> Result<(), FilterExpressionError> {
        match &self.data {
            PredicateData::Bool(predicate) => validate_unordered_set_shape(predicate),
            #[cfg(feature = "serde")]
            PredicateData::WireBool(predicate) => {
                validate_non_ordering(predicate.operator)?;
                if !predicate.operator.takes_a_set() && predicate.values.len() != 1 {
                    return validation_error(FilterExpressionErrorKind::InvalidStructure);
                }
                resolve_once(&predicate.resolved, ())
            }
            #[cfg(feature = "serde")]
            PredicateData::WireString(predicate) => {
                if predicate.operator.set_operator().is_some() {
                    validation_error(FilterExpressionErrorKind::InvalidLiteral)
                } else {
                    validation_error(FilterExpressionErrorKind::UnknownOperator)
                }
            }
            #[cfg(feature = "serde")]
            PredicateData::WireEmpty(predicate) => {
                validate_non_ordering(predicate.operator)?;
                resolve_once(&predicate.resolved, WireEmptyResolved::Bool)
            }
            _ => validation_error(FilterExpressionErrorKind::UnknownOperator),
        }
    }

    /// Validate this predicate for one fixed-width integer kind.
    ///
    /// # Errors
    ///
    /// Returns [`FilterExpressionErrorKind::UnknownOperator`] for a
    /// non-integer family, [`FilterExpressionErrorKind::InvalidStructure`]
    /// for an invalid equality shape, or
    /// [`FilterExpressionErrorKind::InvalidLiteral`] for signedness or
    /// range mismatches.
    ///
    /// The [`crate::query::Filterable`] example executes this method for
    /// both signed and unsigned fields in a hand-written expansion.
    // Generated code passes this small ABI discriminator by value; adding
    // public traits to the hidden enum would widen that frozen surface.
    #[allow(clippy::needless_pass_by_value)]
    pub fn validate_integer(&self, kind: IntegerKind) -> Result<(), FilterExpressionError> {
        match &self.data {
            PredicateData::Signed(predicate) => {
                if !predicate.has_valid_shape() {
                    return validation_error(FilterExpressionErrorKind::InvalidStructure);
                }
                let Some((minimum, maximum)) = kind.signed_bounds() else {
                    return validation_error(FilterExpressionErrorKind::InvalidLiteral);
                };
                if predicate
                    .values
                    .iter()
                    .all(|value| (minimum..=maximum).contains(value))
                {
                    Ok(())
                } else {
                    validation_error(FilterExpressionErrorKind::InvalidLiteral)
                }
            }
            PredicateData::Unsigned(predicate) => {
                if !predicate.has_valid_shape() {
                    return validation_error(FilterExpressionErrorKind::InvalidStructure);
                }
                let Some(maximum) = kind.unsigned_max() else {
                    return validation_error(FilterExpressionErrorKind::InvalidLiteral);
                };
                if predicate.values.iter().all(|value| *value <= maximum) {
                    Ok(())
                } else {
                    validation_error(FilterExpressionErrorKind::InvalidLiteral)
                }
            }
            #[cfg(feature = "serde")]
            PredicateData::WireString(predicate) => {
                let Some(operator) = predicate.operator.set_operator() else {
                    return validation_error(FilterExpressionErrorKind::UnknownOperator);
                };
                // Only `in` and `not_in` take a set; everything else,
                // including a bound, takes exactly one value.
                if !operator.takes_a_set() && predicate.values.len() != 1 {
                    return validation_error(FilterExpressionErrorKind::InvalidStructure);
                }
                if let Some((minimum, maximum)) = kind.signed_bounds() {
                    let values = predicate
                        .values
                        .iter()
                        .map(|value| parse_signed(value, minimum, maximum))
                        .collect::<Result<Vec<_>, _>>()?;
                    resolve_once(
                        &predicate.resolved,
                        WireStringResolved::Signed(super::SetPredicate { operator, values }),
                    )
                } else if let Some(maximum) = kind.unsigned_max() {
                    let values = predicate
                        .values
                        .iter()
                        .map(|value| parse_unsigned(value, maximum))
                        .collect::<Result<Vec<_>, _>>()?;
                    resolve_once(
                        &predicate.resolved,
                        WireStringResolved::Unsigned(super::SetPredicate { operator, values }),
                    )
                } else {
                    validation_error(FilterExpressionErrorKind::InvalidLiteral)
                }
            }
            #[cfg(feature = "serde")]
            PredicateData::WireBool(_) => {
                validation_error(FilterExpressionErrorKind::InvalidLiteral)
            }
            #[cfg(feature = "serde")]
            PredicateData::WireEmpty(predicate) => {
                validate_non_ordering(predicate.operator)?;
                let resolved = if kind.signed_bounds().is_some() {
                    WireEmptyResolved::Signed
                } else if kind.unsigned_max().is_some() {
                    WireEmptyResolved::Unsigned
                } else {
                    return validation_error(FilterExpressionErrorKind::InvalidLiteral);
                };
                resolve_once(&predicate.resolved, resolved)
            }
            _ => validation_error(FilterExpressionErrorKind::UnknownOperator),
        }
    }

    /// Validate this predicate against a custom enum's stable variants.
    ///
    /// # Errors
    ///
    /// Returns [`FilterExpressionErrorKind::UnknownOperator`] for another
    /// scalar family, [`FilterExpressionErrorKind::InvalidStructure`] for
    /// an invalid equality shape, or
    /// [`FilterExpressionErrorKind::InvalidLiteral`] for an unknown
    /// variant.
    ///
    /// The [`crate::query::Filterable`] example executes this method in a
    /// hand-written generated-code expansion.
    pub fn validate_enum(&self, variants: &[&str]) -> Result<(), FilterExpressionError> {
        match &self.data {
            PredicateData::Enum(predicate) => validate_enum_values(predicate, variants),
            #[cfg(feature = "serde")]
            PredicateData::WireString(predicate) => {
                let Some(operator) = predicate.operator.set_operator() else {
                    return validation_error(FilterExpressionErrorKind::UnknownOperator);
                };
                let resolved = super::SetPredicate {
                    operator,
                    values: predicate.values.clone(),
                };
                validate_enum_values(&resolved, variants)?;
                resolve_once(&predicate.resolved, WireStringResolved::Enum(resolved))
            }
            #[cfg(feature = "serde")]
            PredicateData::WireBool(_) => {
                validation_error(FilterExpressionErrorKind::InvalidLiteral)
            }
            #[cfg(feature = "serde")]
            PredicateData::WireEmpty(predicate) => {
                validate_non_ordering(predicate.operator)?;
                resolve_once(&predicate.resolved, WireEmptyResolved::Enum)
            }
            _ => validation_error(FilterExpressionErrorKind::UnknownOperator),
        }
    }

    /// Validate this predicate as a to-many relation over `U`.
    ///
    /// Validation walks every nested logical branch and delegates each
    /// leaf to `U`'s generated schema implementation.
    ///
    /// # Errors
    ///
    /// Returns [`FilterExpressionErrorKind::UnknownOperator`] for a scalar
    /// predicate, [`FilterExpressionErrorKind::UnknownQuantifier`] for
    /// `is`, or the exact source-less nested schema error.
    ///
    /// The [`crate::query::Filterable`] relation example executes this
    /// method in a hand-written generated-code expansion.
    pub fn validate_many<U: Filterable>(&self) -> Result<(), FilterExpressionError> {
        let PredicateData::Relation(relation) = &self.data else {
            return validation_error(FilterExpressionErrorKind::UnknownOperator);
        };
        match relation.quantifier {
            RelationQuantifier::Any | RelationQuantifier::All | RelationQuantifier::None => {
                validate_expression::<U>(&relation.expression)
            }
            RelationQuantifier::Is => {
                validation_error(FilterExpressionErrorKind::UnknownQuantifier)
            }
        }
    }

    /// Validate this predicate as a to-one relation over `U`.
    ///
    /// Validation walks every nested logical branch and delegates each
    /// leaf to `U`'s generated schema implementation.
    ///
    /// # Errors
    ///
    /// Returns [`FilterExpressionErrorKind::UnknownOperator`] for a scalar
    /// predicate, [`FilterExpressionErrorKind::UnknownQuantifier`] for a
    /// to-many quantifier, or the exact source-less nested schema error.
    ///
    /// The [`crate::query::Filterable`] relation example executes this
    /// method in a hand-written generated-code expansion.
    pub fn validate_one<U: Filterable>(&self) -> Result<(), FilterExpressionError> {
        let PredicateData::Relation(relation) = &self.data else {
            return validation_error(FilterExpressionErrorKind::UnknownOperator);
        };
        match relation.quantifier {
            RelationQuantifier::Is => validate_expression::<U>(&relation.expression),
            RelationQuantifier::Any | RelationQuantifier::All | RelationQuantifier::None => {
                validation_error(FilterExpressionErrorKind::UnknownQuantifier)
            }
        }
    }
}

fn validate_text_shape(predicate: &super::TextPredicate) -> Result<(), FilterExpressionError> {
    // Text has no ordering, so an ordering operator on a text field is
    // rejected rather than silently matching nothing.
    if predicate.operator.is_ordering() {
        return validation_error(FilterExpressionErrorKind::UnknownOperator);
    }

    let one_value = predicate.values.len() == 1;
    let regex_state = predicate.operator.is_regex() == predicate.compiled_regex.is_some();
    let value_shape =
        matches!(predicate.operator, TextOperator::In | TextOperator::NotIn) || one_value;

    if regex_state && value_shape {
        Ok(())
    } else {
        validation_error(FilterExpressionErrorKind::InvalidStructure)
    }
}

fn validate_set_shape<T: PartialOrd>(
    predicate: &super::SetPredicate<T>,
) -> Result<(), FilterExpressionError> {
    if predicate.has_valid_shape() {
        Ok(())
    } else {
        validation_error(FilterExpressionErrorKind::InvalidStructure)
    }
}

fn validate_non_ordering(operator: SetOperator) -> Result<(), FilterExpressionError> {
    if operator.is_ordering() {
        validation_error(FilterExpressionErrorKind::UnknownOperator)
    } else {
        Ok(())
    }
}

fn validate_unordered_set_shape<T: PartialOrd>(
    predicate: &super::SetPredicate<T>,
) -> Result<(), FilterExpressionError> {
    validate_non_ordering(predicate.operator)?;
    validate_set_shape(predicate)
}

fn validate_enum_values(
    predicate: &super::SetPredicate<String>,
    variants: &[&str],
) -> Result<(), FilterExpressionError> {
    validate_unordered_set_shape(predicate)?;
    if predicate
        .values
        .iter()
        .all(|value| variants.contains(&value.as_str()))
    {
        Ok(())
    } else {
        validation_error(FilterExpressionErrorKind::InvalidLiteral)
    }
}

#[cfg(feature = "serde")]
fn resolve_once<T: Eq>(cell: &super::OnceLock<T>, value: T) -> Result<(), FilterExpressionError> {
    super::set_once_eq(cell, value, FilterExpressionErrorKind::InvalidStructure)
}

#[cfg(feature = "serde")]
fn has_canonical_digits(value: &str, signed: bool) -> bool {
    let digits = if signed && value.starts_with('-') {
        &value[1..]
    } else {
        value
    };
    if digits == "0" {
        return !value.starts_with('-');
    }
    let Some(first) = digits.as_bytes().first() else {
        return false;
    };
    matches!(first, b'1'..=b'9') && digits.as_bytes().iter().all(u8::is_ascii_digit)
}

#[cfg(feature = "serde")]
fn parse_signed(value: &str, minimum: i128, maximum: i128) -> Result<i128, FilterExpressionError> {
    if !has_canonical_digits(value, true) {
        return Err(FilterExpressionError::new(
            FilterExpressionErrorKind::InvalidLiteral,
        ));
    }
    value
        .parse::<i128>()
        .ok()
        .filter(|parsed| (minimum..=maximum).contains(parsed))
        .ok_or_else(|| FilterExpressionError::new(FilterExpressionErrorKind::InvalidLiteral))
}

#[cfg(feature = "serde")]
fn parse_unsigned(value: &str, maximum: u128) -> Result<u128, FilterExpressionError> {
    if value.starts_with('-') || !has_canonical_digits(value, false) {
        return Err(FilterExpressionError::new(
            FilterExpressionErrorKind::InvalidLiteral,
        ));
    }
    value
        .parse::<u128>()
        .ok()
        .filter(|parsed| *parsed <= maximum)
        .ok_or_else(|| FilterExpressionError::new(FilterExpressionErrorKind::InvalidLiteral))
}

#[cfg(feature = "serde")]
fn set_matches<T: PartialOrd>(operator: SetOperator, values: &[T], candidate: &T) -> bool {
    super::set_matches(operator, values, candidate)
}

fn validation_error(kind: FilterExpressionErrorKind) -> Result<(), FilterExpressionError> {
    Err(FilterExpressionError::new(kind))
}

/// Construct a text handle for generated code.
///
/// # Examples
///
/// ```
/// use libtmux::query::{TextField};
/// use libtmux::query::__private;
/// struct Row;
/// let field: TextField<Row> = __private::text_field("row", "name");
/// assert!(format!("{field:?}").contains("name"));
/// ```
#[must_use]
pub const fn text_field<T>(target: &'static str, field: &'static str) -> TextField<T> {
    TextField {
        id: FieldId { target, field },
        marker: PhantomData,
    }
}

/// Construct a boolean handle for generated code.
///
/// # Examples
///
/// ```
/// use libtmux::query::BoolField;
/// use libtmux::query::__private;
/// struct Row;
/// let field: BoolField<Row> = __private::bool_field("row", "done");
/// assert!(format!("{field:?}").contains("done"));
/// ```
#[must_use]
pub const fn bool_field<T>(target: &'static str, field: &'static str) -> BoolField<T> {
    BoolField {
        id: FieldId { target, field },
        marker: PhantomData,
    }
}

/// Construct an integer handle for generated code.
///
/// # Examples
///
/// ```
/// use libtmux::query::IntegerField;
/// use libtmux::query::__private;
/// struct Row;
/// let field: IntegerField<Row, i64> = __private::integer_field("row", "count");
/// assert!(format!("{field:?}").contains("count"));
/// ```
#[must_use]
pub const fn integer_field<T, N>(target: &'static str, field: &'static str) -> IntegerField<T, N> {
    IntegerField {
        id: FieldId { target, field },
        marker: PhantomData,
    }
}

/// Construct a custom enum handle for generated code.
///
/// # Examples
///
/// ```
/// use libtmux::query::{EnumField, FilterEnum};
/// use libtmux::query::__private;
/// enum State { Ready }
/// impl FilterEnum for State {
///     const FILTER_VARIANTS: &'static [&'static str] = &["ready"];
///     fn filter_name(&self) -> &'static str { "ready" }
/// }
/// struct Row;
/// let field: EnumField<Row, State> = __private::enum_field("row", "state");
/// assert!(format!("{field:?}").contains("state"));
/// ```
#[must_use]
pub const fn enum_field<T, E: FilterEnum>(
    target: &'static str,
    field: &'static str,
) -> EnumField<T, E> {
    EnumField {
        id: FieldId { target, field },
        marker: PhantomData,
    }
}

/// Construct a to-many relation handle for generated code.
///
/// # Examples
///
/// ```
/// use libtmux::query::ManyRelation;
/// use libtmux::query::__private;
/// struct Parent;
/// struct Child;
/// let field: ManyRelation<Parent, Child> =
///     __private::many_relation("parent", "children");
/// assert!(format!("{field:?}").contains("children"));
/// ```
#[must_use]
pub const fn many_relation<From, To>(
    target: &'static str,
    field: &'static str,
) -> ManyRelation<From, To> {
    ManyRelation {
        id: FieldId { target, field },
        marker: PhantomData,
    }
}

/// Construct a to-one relation handle for generated code.
///
/// # Examples
///
/// ```
/// use libtmux::query::OneRelation;
/// use libtmux::query::__private;
/// struct Parent;
/// struct Owner;
/// let field: OneRelation<Parent, Owner> = __private::one_relation("parent", "owner");
/// assert!(format!("{field:?}").contains("owner"));
/// ```
#[must_use]
pub const fn one_relation<From, To>(
    target: &'static str,
    field: &'static str,
) -> OneRelation<From, To> {
    OneRelation {
        id: FieldId { target, field },
        marker: PhantomData,
    }
}

/// Construct the source-less generated fallback for an unknown field.
///
/// # Examples
///
/// ```
/// use libtmux::query::FilterExpressionErrorKind;
/// use libtmux::query::__private;
/// let error = __private::unknown_field_error();
/// assert_eq!(error.kind(), FilterExpressionErrorKind::UnknownField);
/// ```
#[must_use]
pub const fn unknown_field_error() -> FilterExpressionError {
    FilterExpressionError::new(FilterExpressionErrorKind::UnknownField)
}
