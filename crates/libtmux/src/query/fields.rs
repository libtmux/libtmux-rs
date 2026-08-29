use std::fmt;
use std::marker::PhantomData;

use regex::{Regex, RegexBuilder};

use super::{
    __private, FieldId, FilterEnum, FilterExpr, FilterExpressionError, FilterExpressionErrorKind,
    PredicateData, RelationPredicate, RelationQuantifier, SetOperator, SetPredicate, TextOperator,
    TextPredicate,
};

/// A typed handle for a portable text field on `T`.
///
/// All operators require strict UTF-8 in the candidate and return `false` for
/// invalid bytes before any outer logical negation. Rust membership authoring
/// accepts any `IntoIterator`; the version 1 wire representation uses an
/// array. A scalar string is not substring membership: use
/// [`TextField::contains`] for that operation.
///
/// # Examples
///
/// ```
/// use libtmux::query::TextField;
/// use libtmux::query::__private;
///
/// struct Row;
/// let field: TextField<Row> = __private::text_field("row", "name");
/// let _ = field.contains("build");
/// ```
pub struct TextField<T> {
    pub(super) id: FieldId,
    pub(super) marker: PhantomData<fn() -> T>,
}

/// A typed handle for a portable boolean field on `T`.
///
/// # Examples
///
/// ```
/// use libtmux::query::BoolField;
/// use libtmux::query::__private;
///
/// struct Row;
/// let field: BoolField<Row> = __private::bool_field("row", "done");
/// let _ = field.eq(false);
/// ```
pub struct BoolField<T> {
    pub(super) id: FieldId,
    pub(super) marker: PhantomData<fn() -> T>,
}

/// A typed handle for a fixed-width integer field `N` on `T`.
///
/// Only the ten portable fixed-width Rust integer primitives have authoring
/// methods.
///
/// # Examples
///
/// ```
/// use libtmux::query::IntegerField;
/// use libtmux::query::__private;
///
/// struct Row;
/// let field: IntegerField<Row, i64> = __private::integer_field("row", "count");
/// let _ = field.eq(7_i64);
/// ```
pub struct IntegerField<T, N> {
    pub(super) id: FieldId,
    pub(super) marker: PhantomData<fn() -> (T, N)>,
}

/// A typed handle for a custom enum field `E` on `T`.
///
/// # Examples
///
/// ```
/// use libtmux::query::{EnumField, FilterEnum};
/// use libtmux::query::__private;
///
/// enum State { Ready }
/// impl FilterEnum for State {
///     const FILTER_VARIANTS: &'static [&'static str] = &["ready"];
///     fn filter_name(&self) -> &'static str { "ready" }
/// }
/// struct Row;
/// let field: EnumField<Row, State> = __private::enum_field("row", "state");
/// let _ = field.eq(State::Ready);
/// ```
pub struct EnumField<T, E> {
    pub(super) id: FieldId,
    pub(super) marker: PhantomData<fn() -> (T, E)>,
}

/// A value-only handle for a to-many relation from `From` to `To`.
///
/// Relation expressions inspect only the already-loaded related values on a
/// candidate and never perform I/O.
///
/// # Examples
///
/// ```
/// use libtmux::query::ManyRelation;
/// use libtmux::query::__private;
///
/// struct Parent;
/// struct Child;
/// let relation: ManyRelation<Parent, Child> =
///     __private::many_relation("parent", "children");
/// assert!(format!("{relation:?}").contains("children"));
/// ```
pub struct ManyRelation<From, To> {
    pub(super) id: FieldId,
    pub(super) marker: PhantomData<fn() -> (From, To)>,
}

/// A value-only handle for a to-one relation from `From` to `To`.
///
/// Relation expressions inspect only the already-loaded optional value on a
/// candidate and never perform I/O.
///
/// # Examples
///
/// ```
/// use libtmux::query::OneRelation;
/// use libtmux::query::__private;
///
/// struct Parent;
/// struct Owner;
/// let relation: OneRelation<Parent, Owner> =
///     __private::one_relation("parent", "owner");
/// assert!(format!("{relation:?}").contains("owner"));
/// ```
pub struct OneRelation<From, To> {
    pub(super) id: FieldId,
    pub(super) marker: PhantomData<fn() -> (From, To)>,
}

impl<From, To> ManyRelation<From, To> {
    fn expression(
        self,
        quantifier: RelationQuantifier,
        expression: FilterExpr<To>,
    ) -> FilterExpr<From> {
        FilterExpr::predicate(__private::Predicate::new(
            self.id,
            PredicateData::Relation(RelationPredicate {
                quantifier,
                expression: Box::new(expression.data),
            }),
        ))
    }

    /// Match when any related candidate satisfies `expression`.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{BoolField, ManyRelation};
    /// use libtmux::query::__private;
    ///
    /// struct Parent;
    /// struct Child;
    /// let relation: ManyRelation<Parent, Child> =
    ///     __private::many_relation("parent", "children");
    /// let done: BoolField<Child> = __private::bool_field("child", "done");
    /// assert_eq!(relation.any(done.eq(true)), relation.any(done.eq(true)));
    /// ```
    #[must_use]
    pub fn any(self, expression: FilterExpr<To>) -> FilterExpr<From> {
        self.expression(RelationQuantifier::Any, expression)
    }

    /// Match when every related candidate satisfies `expression`.
    ///
    /// This is true for an empty loaded relation.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{BoolField, ManyRelation};
    /// use libtmux::query::__private;
    ///
    /// struct Parent;
    /// struct Child;
    /// let relation: ManyRelation<Parent, Child> =
    ///     __private::many_relation("parent", "children");
    /// let done: BoolField<Child> = __private::bool_field("child", "done");
    /// assert_eq!(relation.all(done.eq(true)), relation.all(done.eq(true)));
    /// ```
    #[must_use]
    pub fn all(self, expression: FilterExpr<To>) -> FilterExpr<From> {
        self.expression(RelationQuantifier::All, expression)
    }

    /// Match when no related candidate satisfies `expression`.
    ///
    /// This is true for an empty loaded relation.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{BoolField, ManyRelation};
    /// use libtmux::query::__private;
    ///
    /// struct Parent;
    /// struct Child;
    /// let relation: ManyRelation<Parent, Child> =
    ///     __private::many_relation("parent", "children");
    /// let done: BoolField<Child> = __private::bool_field("child", "done");
    /// assert_eq!(relation.none(done.eq(true)), relation.none(done.eq(true)));
    /// ```
    #[must_use]
    pub fn none(self, expression: FilterExpr<To>) -> FilterExpr<From> {
        self.expression(RelationQuantifier::None, expression)
    }
}

impl<From, To> OneRelation<From, To> {
    /// Match when the related candidate exists and satisfies `expression`.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{BoolField, OneRelation};
    /// use libtmux::query::__private;
    ///
    /// struct Parent;
    /// struct Child;
    /// let relation: OneRelation<Parent, Child> =
    ///     __private::one_relation("parent", "child");
    /// let done: BoolField<Child> = __private::bool_field("child", "done");
    /// assert_eq!(relation.is(done.eq(true)), relation.is(done.eq(true)));
    /// ```
    #[must_use]
    pub fn is(self, expression: FilterExpr<To>) -> FilterExpr<From> {
        FilterExpr::predicate(__private::Predicate::new(
            self.id,
            PredicateData::Relation(RelationPredicate {
                quantifier: RelationQuantifier::Is,
                expression: Box::new(expression.data),
            }),
        ))
    }
}

macro_rules! impl_handle_traits {
    ($name:ident<$($type_parameter:ident),+>) => {
        impl<$($type_parameter),+> Copy for $name<$($type_parameter),+> {}

        impl<$($type_parameter),+> Clone for $name<$($type_parameter),+> {
            fn clone(&self) -> Self {
                *self
            }
        }

        impl<$($type_parameter),+> PartialEq for $name<$($type_parameter),+> {
            fn eq(&self, other: &Self) -> bool {
                self.id == other.id
            }
        }

        impl<$($type_parameter),+> Eq for $name<$($type_parameter),+> {}

        impl<$($type_parameter),+> fmt::Debug for $name<$($type_parameter),+> {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .field("target", &self.id.target)
                    .field("field", &self.id.field)
                    .finish()
            }
        }
    };
}

impl_handle_traits!(TextField<T>);
impl_handle_traits!(BoolField<T>);
impl_handle_traits!(IntegerField<T, N>);
impl_handle_traits!(EnumField<T, E>);
impl_handle_traits!(ManyRelation<From, To>);
impl_handle_traits!(OneRelation<From, To>);

impl<T> TextField<T> {
    fn expression(
        self,
        operator: TextOperator,
        values: Vec<String>,
        compiled_regex: Option<Regex>,
    ) -> FilterExpr<T> {
        FilterExpr::predicate(__private::Predicate::new(
            self.id,
            PredicateData::Text(TextPredicate {
                operator,
                values,
                compiled_regex,
            }),
        ))
    }

    fn one(self, operator: TextOperator, value: impl Into<String>) -> FilterExpr<T> {
        self.expression(operator, vec![value.into()], None)
    }

    /// Compare a candidate with one exact string.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// assert_eq!(field.eq("build"), field.eq(String::from("build")));
    /// ```
    #[must_use]
    pub fn eq(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::Eq, value)
    }

    /// Compare using Unicode default case folding without normalization.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.eq_ignore_case("BUILD");
    /// ```
    #[must_use]
    pub fn eq_ignore_case(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::EqIgnoreCase, value)
    }

    /// Test exact substring containment.
    ///
    /// This is the substring operator; membership never treats one string as
    /// a collection of candidate substrings.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.contains("build");
    /// ```
    #[must_use]
    pub fn contains(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::Contains, value)
    }

    /// Test folded substring containment without normalization.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.contains_ignore_case("BUILD");
    /// ```
    #[must_use]
    pub fn contains_ignore_case(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::ContainsIgnoreCase, value)
    }

    /// Test an exact string prefix.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.starts_with("build");
    /// ```
    #[must_use]
    pub fn starts_with(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::StartsWith, value)
    }

    /// Test a folded string prefix without normalization.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.starts_with_ignore_case("BUILD");
    /// ```
    #[must_use]
    pub fn starts_with_ignore_case(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::StartsWithIgnoreCase, value)
    }

    /// Test an exact string suffix.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.ends_with("build");
    /// ```
    #[must_use]
    pub fn ends_with(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::EndsWith, value)
    }

    /// Test a folded string suffix without normalization.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.ends_with_ignore_case("BUILD");
    /// ```
    #[must_use]
    pub fn ends_with_ignore_case(self, value: impl Into<String>) -> FilterExpr<T> {
        self.one(TextOperator::EndsWithIgnoreCase, value)
    }

    /// Test exact membership in ordered string values.
    ///
    /// Rust authoring accepts any `IntoIterator`; the version 1 wire value is
    /// an array, never a scalar string. An empty input never matches. Invalid
    /// candidate UTF-8 also never matches.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.is_in(["build", "test"]);
    /// ```
    #[must_use]
    pub fn is_in<I, S>(self, values: I) -> FilterExpr<T>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.expression(
            TextOperator::In,
            values.into_iter().map(Into::into).collect(),
            None,
        )
    }

    /// Test exact exclusion from ordered string values.
    ///
    /// Rust authoring accepts any `IntoIterator`; the version 1 wire value is
    /// an array, never a scalar string. An empty input matches every valid
    /// UTF-8 candidate, but invalid candidate bytes remain a nonmatch.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field.not_in(["done"]);
    /// ```
    #[must_use]
    pub fn not_in<I, S>(self, values: I) -> FilterExpr<T>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.expression(
            TextOperator::NotIn,
            values.into_iter().map(Into::into).collect(),
            None,
        )
    }

    fn regex_inner(
        self,
        pattern: String,
        ignore_case: bool,
    ) -> Result<FilterExpr<T>, FilterExpressionError> {
        let compiled_regex = RegexBuilder::new(&pattern)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|_| FilterExpressionError::new(FilterExpressionErrorKind::InvalidRegex))?;
        let operator = if ignore_case {
            TextOperator::RegexIgnoreCase
        } else {
            TextOperator::Regex
        };
        Ok(self.expression(operator, vec![pattern], Some(compiled_regex)))
    }

    /// Compile and match a case-sensitive Rust regular expression.
    ///
    /// Rust syntax excludes Python-only look-around and backreferences.
    ///
    /// # Errors
    ///
    /// Returns a source-less [`FilterExpressionErrorKind::InvalidRegex`] when
    /// `pattern` does not compile under the version 1 regex dialect.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let expression = field.regex("^build$").expect("the pattern is valid");
    /// assert_eq!(expression, field.regex("^build$").expect("the pattern is valid"));
    /// ```
    pub fn regex(self, pattern: impl Into<String>) -> Result<FilterExpr<T>, FilterExpressionError> {
        self.regex_inner(pattern.into(), false)
    }

    /// Compile and match a Unicode case-insensitive Rust regular expression.
    ///
    /// Rust syntax excludes Python-only look-around and backreferences.
    ///
    /// # Errors
    ///
    /// Returns a source-less [`FilterExpressionErrorKind::InvalidRegex`] when
    /// `pattern` does not compile under the version 1 regex dialect.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::TextField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: TextField<Row> = __private::text_field("row", "name");
    /// let _ = field
    ///     .regex_ignore_case("^build$")
    ///     .expect("the pattern is valid");
    /// ```
    pub fn regex_ignore_case(
        self,
        pattern: impl Into<String>,
    ) -> Result<FilterExpr<T>, FilterExpressionError> {
        self.regex_inner(pattern.into(), true)
    }
}

impl<T> BoolField<T> {
    fn expression(self, operator: SetOperator, values: Vec<bool>) -> FilterExpr<T> {
        FilterExpr::predicate(__private::Predicate::new(
            self.id,
            PredicateData::Bool(SetPredicate { operator, values }),
        ))
    }

    /// Compare with one boolean value.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::BoolField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: BoolField<Row> = __private::bool_field("row", "done");
    /// assert_eq!(field.eq(true), field.eq(true));
    /// ```
    #[must_use]
    pub fn eq(self, value: bool) -> FilterExpr<T> {
        self.expression(SetOperator::Eq, vec![value])
    }

    /// Test membership in an ordered set of booleans.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::BoolField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: BoolField<Row> = __private::bool_field("row", "done");
    /// let _ = field.is_in([false, true]);
    /// ```
    #[must_use]
    pub fn is_in(self, values: impl IntoIterator<Item = bool>) -> FilterExpr<T> {
        self.expression(SetOperator::In, values.into_iter().collect())
    }

    /// Test exclusion from an ordered set of booleans.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::BoolField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: BoolField<Row> = __private::bool_field("row", "done");
    /// let _ = field.not_in([true]);
    /// ```
    #[must_use]
    pub fn not_in(self, values: impl IntoIterator<Item = bool>) -> FilterExpr<T> {
        self.expression(SetOperator::NotIn, values.into_iter().collect())
    }
}

macro_rules! impl_integer_field {
    ($integer:ty, $variant:ident, $convert:path) => {
        impl<T> IntegerField<T, $integer> {
            fn expression(self, operator: SetOperator, values: Vec<$integer>) -> FilterExpr<T> {
                FilterExpr::predicate(__private::Predicate::new(
                    self.id,
                    PredicateData::$variant(SetPredicate {
                        operator,
                        values: values.into_iter().map($convert).collect(),
                    }),
                ))
            }

            /// Compare with one exact fixed-width integer value.
            ///
            /// # Examples
            ///
            /// ```
            /// use libtmux::query::IntegerField;
            /// use libtmux::query::__private;
            ///
            /// struct Row;
            /// let field: IntegerField<Row, i64> =
            ///     __private::integer_field("row", "count");
            /// assert_eq!(field.eq(1), field.eq(1));
            /// ```
            #[must_use]
            pub fn eq(self, value: $integer) -> FilterExpr<T> {
                self.expression(SetOperator::Eq, vec![value])
            }

            /// Test membership in an ordered set of fixed-width integers.
            ///
            /// # Examples
            ///
            /// ```
            /// use libtmux::query::IntegerField;
            /// use libtmux::query::__private;
            ///
            /// struct Row;
            /// let field: IntegerField<Row, i64> =
            ///     __private::integer_field("row", "count");
            /// let _ = field.is_in([1, 2]);
            /// ```
            #[must_use]
            pub fn is_in(self, values: impl IntoIterator<Item = $integer>) -> FilterExpr<T> {
                self.expression(SetOperator::In, values.into_iter().collect())
            }

            /// Test exclusion from an ordered set of fixed-width integers.
            ///
            /// # Examples
            ///
            /// ```
            /// use libtmux::query::IntegerField;
            /// use libtmux::query::__private;
            ///
            /// struct Row;
            /// let field: IntegerField<Row, i64> =
            ///     __private::integer_field("row", "count");
            /// let _ = field.not_in([1, 2]);
            /// ```
            #[must_use]
            pub fn not_in(self, values: impl IntoIterator<Item = $integer>) -> FilterExpr<T> {
                self.expression(SetOperator::NotIn, values.into_iter().collect())
            }

            /// Test that the field is below a bound.
            ///
            /// Ordering exists for integers and not for text or booleans, so
            /// a comparison that has no meaning for a field's type does not
            /// compile.
            ///
            /// # Examples
            ///
            /// ```
            /// use libtmux::query::IntegerField;
            /// use libtmux::query::__private;
            ///
            /// struct Row;
            /// let field: IntegerField<Row, i64> =
            ///     __private::integer_field("row", "count");
            /// let _ = field.lt(10);
            /// ```
            #[must_use]
            pub fn lt(self, bound: $integer) -> FilterExpr<T> {
                self.expression(SetOperator::Lt, vec![bound])
            }

            /// Test that the field is at or below a bound.
            ///
            /// # Examples
            ///
            /// ```
            /// use libtmux::query::IntegerField;
            /// use libtmux::query::__private;
            ///
            /// struct Row;
            /// let field: IntegerField<Row, i64> =
            ///     __private::integer_field("row", "count");
            /// let _ = field.lte(10);
            /// ```
            #[must_use]
            pub fn lte(self, bound: $integer) -> FilterExpr<T> {
                self.expression(SetOperator::Lte, vec![bound])
            }

            /// Test that the field is above a bound.
            ///
            /// # Examples
            ///
            /// ```
            /// use libtmux::query::IntegerField;
            /// use libtmux::query::__private;
            ///
            /// struct Row;
            /// let field: IntegerField<Row, i64> =
            ///     __private::integer_field("row", "count");
            /// let _ = field.gt(1);
            /// ```
            #[must_use]
            pub fn gt(self, bound: $integer) -> FilterExpr<T> {
                self.expression(SetOperator::Gt, vec![bound])
            }

            /// Test that the field is at or above a bound.
            ///
            /// # Examples
            ///
            /// ```
            /// use libtmux::query::IntegerField;
            /// use libtmux::query::__private;
            ///
            /// struct Row;
            /// let field: IntegerField<Row, i64> =
            ///     __private::integer_field("row", "count");
            /// let _ = field.gte(1);
            /// ```
            #[must_use]
            pub fn gte(self, bound: $integer) -> FilterExpr<T> {
                self.expression(SetOperator::Gte, vec![bound])
            }
        }
    };
}

impl_integer_field!(i8, Signed, i128::from);
impl_integer_field!(i16, Signed, i128::from);
impl_integer_field!(i32, Signed, i128::from);
impl_integer_field!(i64, Signed, i128::from);
impl_integer_field!(i128, Signed, i128::from);
impl_integer_field!(u8, Unsigned, u128::from);
impl_integer_field!(u16, Unsigned, u128::from);
impl_integer_field!(u32, Unsigned, u128::from);
impl_integer_field!(u64, Unsigned, u128::from);
impl_integer_field!(u128, Unsigned, u128::from);

impl<T, E: FilterEnum> EnumField<T, E> {
    fn expression(self, operator: SetOperator, values: Vec<E>) -> FilterExpr<T> {
        FilterExpr::predicate(__private::Predicate::new(
            self.id,
            PredicateData::Enum(SetPredicate {
                operator,
                values: values
                    .into_iter()
                    .map(|value| value.filter_name().to_owned())
                    .collect(),
            }),
        ))
    }

    /// Compare with one custom enum value's stable filter name.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{EnumField, FilterEnum};
    /// use libtmux::query::__private;
    ///
    /// enum State { Ready }
    /// impl FilterEnum for State {
    ///     const FILTER_VARIANTS: &'static [&'static str] = &["ready"];
    ///     fn filter_name(&self) -> &'static str { "ready" }
    /// }
    /// struct Row;
    /// let field: EnumField<Row, State> = __private::enum_field("row", "state");
    /// let _ = field.eq(State::Ready);
    /// ```
    #[must_use]
    pub fn eq(self, value: E) -> FilterExpr<T> {
        self.expression(SetOperator::Eq, vec![value])
    }

    /// Test membership using custom enum values' stable filter names.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{EnumField, FilterEnum};
    /// use libtmux::query::__private;
    ///
    /// enum State { Ready, Blocked }
    /// impl FilterEnum for State {
    ///     const FILTER_VARIANTS: &'static [&'static str] = &["ready", "blocked"];
    ///     fn filter_name(&self) -> &'static str {
    ///         match self { Self::Ready => "ready", Self::Blocked => "blocked" }
    ///     }
    /// }
    /// struct Row;
    /// let field: EnumField<Row, State> = __private::enum_field("row", "state");
    /// let _ = field.is_in([State::Ready, State::Blocked]);
    /// ```
    #[must_use]
    pub fn is_in(self, values: impl IntoIterator<Item = E>) -> FilterExpr<T> {
        self.expression(SetOperator::In, values.into_iter().collect())
    }

    /// Test exclusion using custom enum values' stable filter names.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{EnumField, FilterEnum};
    /// use libtmux::query::__private;
    ///
    /// enum State { Ready }
    /// impl FilterEnum for State {
    ///     const FILTER_VARIANTS: &'static [&'static str] = &["ready"];
    ///     fn filter_name(&self) -> &'static str { "ready" }
    /// }
    /// struct Row;
    /// let field: EnumField<Row, State> = __private::enum_field("row", "state");
    /// let _ = field.not_in([State::Ready]);
    /// ```
    #[must_use]
    pub fn not_in(self, values: impl IntoIterator<Item = E>) -> FilterExpr<T> {
        self.expression(SetOperator::NotIn, values.into_iter().collect())
    }
}
