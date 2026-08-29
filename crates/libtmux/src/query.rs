//! Predicates and cardinality helpers for borrowed iterators.
//!
//! Start with a replayable collection, borrow it with `.iter()`, and keep
//! inline closures on native [`Iterator::filter`]. Use
//! [`QueryIteratorExt::matching`] for a named [`Matcher`] or typed
//! [`FilterExpr`], then apply exact cardinality without collecting or
//! exhausting more than two items:
//!
//! ```
//! use libtmux::query::{Matcher, QueryIteratorExt};
//!
//! struct IsPending;
//!
//! impl Matcher<(&'static str, bool)> for IsPending {
//!     fn matches(&self, candidate: &(&'static str, bool)) -> bool {
//!         !candidate.1
//!     }
//! }
//!
//! let tasks = vec![("build", false), ("test", true)];
//! let visible = tasks.iter().filter(|task| task.0.starts_with('b'));
//! assert_eq!(visible.collect::<Vec<_>>(), vec![&tasks[0]]);
//! assert_eq!(tasks.iter().matching(IsPending).exactly_one(), Ok(&tasks[0]));
//! ```
//!
//! [`QueryIteratorExt::exactly_one`] distinguishes zero from multiple items;
//! [`QueryIteratorExt::one_or_none`] permits zero but rejects multiple items.
//! Both return borrowed values and pull at most two items.
//!
//! Portable expressions are owned, inert local values. With `derive`, typed
//! handles can be generated for downstream data without exposing the hidden
//! expansion constructors:
//!
//! ```
//! # #[cfg(feature = "derive")]
//! # {
//! use libtmux::query::{Filterable as _, QueryIteratorExt as _};
//!
//! #[derive(libtmux::Filterable)]
//! #[filterable(target = "task")]
//! # #[filterable(crate = "libtmux")]
//! struct Task {
//!     name: String,
//!     done: bool,
//! }
//!
//! let tasks = vec![Task { name: "build".into(), done: false }];
//! let fields = Task::filter_fields();
//! let expression = fields.name.eq("build").and(fields.done.eq(false));
//! assert_eq!(tasks.iter().matching(&expression).count(), 1);
//! # }
//! ```
//!
//! Local matching is synchronous, ordered, and never performs tmux I/O or
//! native pushdown. Text predicates require strict candidate UTF-8, so every
//! text predicate returns false for invalid bytes, including `not_in([])`;
//! an outer [`FilterExpr::not`] may invert that result. Relations inspect only
//! already-hydrated candidate data: empty to-many data makes `any` false and
//! `all` and `none` true, while absent to-one data makes `is` false.
//!
//! The optional `derive` and `serde` features are independent. Serde uses the
//! closed version 1 grammar, whose membership values are arrays even though
//! Rust authoring accepts any `IntoIterator`. Decoding accepts at most 64
//! expression levels, 4,096 expression nodes, and 4,096 membership values.
//! Regex operators use Rust syntax and reject Python-only look-around and
//! backreferences with source-less, value-free errors. Dynamic
//! `field__operator` parsing and remote pushdown belong to later ingress and
//! execution layers.
//!
//! Scalar field handles expose only operators valid for their field type. A
//! boolean field rejects string equality and membership:
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.eq("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.is_in(["true"]);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.not_in(["true"]);
//! ```
//!
//! Boolean fields also have none of the text-only operators:
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.eq_ignore_case("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.contains("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.contains_ignore_case("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.starts_with("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.starts_with_ignore_case("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.ends_with("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.ends_with_ignore_case("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.regex("true");
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::bool_field::<Row>("row", "done");
//! let _ = field.regex_ignore_case("true");
//! ```
//!
//! Platform-width integers are intentionally outside the portable grammar:
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::integer_field::<Row, isize>("row", "count");
//! let _ = field.eq(0_isize);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::integer_field::<Row, isize>("row", "count");
//! let _ = field.is_in([0_isize]);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::integer_field::<Row, isize>("row", "count");
//! let _ = field.not_in([0_isize]);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::integer_field::<Row, usize>("row", "count");
//! let _ = field.eq(0_usize);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::integer_field::<Row, usize>("row", "count");
//! let _ = field.is_in([0_usize]);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Row;
//! let field = __private::integer_field::<Row, usize>("row", "count");
//! let _ = field.not_in([0_usize]);
//! ```
//!
//! Relation handles accept only portable expressions for their related type,
//! never closures or arbitrary matchers:
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Parent;
//! struct Child;
//! let relation = __private::many_relation::<Parent, Child>("parent", "children");
//! let _ = relation.any(|_: &Child| true);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::{Matcher, OneRelation};
//! use libtmux::query::__private;
//!
//! struct Parent;
//! struct Child;
//! struct ChildMatcher;
//! impl Matcher<Child> for ChildMatcher {
//!     fn matches(&self, _: &Child) -> bool { true }
//! }
//! let relation: OneRelation<Parent, Child> =
//!     __private::one_relation("parent", "child");
//! let _ = relation.is(ChildMatcher);
//! ```
//!
//! A relation expression cannot cross related schemas:
//!
//! ```compile_fail
//! use libtmux::query::{FilterExpressionError, Filterable};
//! use libtmux::query::__private::{self, Predicate};
//!
//! struct Parent;
//! struct Child;
//! struct Other;
//! macro_rules! filterable {
//!     ($type:ty, $target:literal) => {
//!         impl Filterable for $type {
//!             type Fields = ();
//!             const FILTER_TARGET: &'static str = $target;
//!             fn filter_fields() {}
//!             fn __filter_matches(&self, _: &Predicate) -> bool { false }
//!             fn __filter_validate(_: &Predicate) -> Result<(), FilterExpressionError> {
//!                 Ok(())
//!             }
//!         }
//!     };
//! }
//! filterable!(Child, "child");
//! filterable!(Other, "other");
//! let relation = __private::many_relation::<Parent, Child>("parent", "children");
//! let other = __private::bool_field::<Other>("other", "enabled").eq(true);
//! let _ = relation.any(other);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::{FilterExpressionError, Filterable};
//! use libtmux::query::__private::{self, Predicate};
//!
//! struct Parent;
//! struct Child;
//! struct Other;
//! macro_rules! filterable {
//!     ($type:ty, $target:literal) => {
//!         impl Filterable for $type {
//!             type Fields = ();
//!             const FILTER_TARGET: &'static str = $target;
//!             fn filter_fields() {}
//!             fn __filter_matches(&self, _: &Predicate) -> bool { false }
//!             fn __filter_validate(_: &Predicate) -> Result<(), FilterExpressionError> {
//!                 Ok(())
//!             }
//!         }
//!     };
//! }
//! filterable!(Child, "child");
//! filterable!(Other, "other");
//! let relation = __private::one_relation::<Parent, Child>("parent", "child");
//! let other = __private::bool_field::<Other>("other", "enabled").eq(true);
//! let _ = relation.is(other);
//! ```
//!
//! To-many and to-one relations expose disjoint quantifier families:
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Parent;
//! struct Child;
//! let relation = __private::many_relation::<Parent, Child>("parent", "children");
//! let child = __private::bool_field::<Child>("child", "enabled").eq(true);
//! let _ = relation.is(child);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Parent;
//! struct Child;
//! let relation = __private::one_relation::<Parent, Child>("parent", "child");
//! let child = __private::bool_field::<Child>("child", "enabled").eq(true);
//! let _ = relation.any(child);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Parent;
//! struct Child;
//! let relation = __private::one_relation::<Parent, Child>("parent", "child");
//! let child = __private::bool_field::<Child>("child", "enabled").eq(true);
//! let _ = relation.all(child);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::__private;
//!
//! struct Parent;
//! struct Child;
//! let relation = __private::one_relation::<Parent, Child>("parent", "child");
//! let child = __private::bool_field::<Child>("child", "enabled").eq(true);
//! let _ = relation.none(child);
//! ```
//!
//! Expression error kinds are non-exhaustive for downstream callers:
//!
//! ```compile_fail
//! use libtmux::query::FilterExpressionErrorKind;
//!
//! fn label(kind: FilterExpressionErrorKind) -> &'static str {
//!     match kind {
//!         FilterExpressionErrorKind::InvalidRegex => "invalid regex",
//!         FilterExpressionErrorKind::UnsupportedVersion => "unsupported version",
//!         FilterExpressionErrorKind::InvalidTarget => "invalid target",
//!         FilterExpressionErrorKind::UnknownField => "unknown field",
//!         FilterExpressionErrorKind::UnknownOperator => "unknown operator",
//!         FilterExpressionErrorKind::UnknownQuantifier => "unknown quantifier",
//!         FilterExpressionErrorKind::InvalidLiteral => "invalid literal",
//!         FilterExpressionErrorKind::InvalidStructure => "invalid structure",
//!     }
//! }
//! ```

use std::fmt;
use std::marker::PhantomData;
#[cfg(feature = "serde")]
use std::sync::OnceLock;

use caseless::default_case_fold_str;
use regex::{Regex, RegexBuilder};

mod grammar;
use grammar::{RelationQuantifier, SetOperator, TextOperator};
mod matching;
use matching::{
    ExprData, FieldId, PredicateData, PredicateIdentity, RedactedExprDebug, RelationPredicate,
    SetPredicate, TextPredicate, evaluate, validate_expression,
};
#[cfg(feature = "serde")]
use matching::{
    ResolvedScalar, WireBoolPredicate, WireEmptyPredicate, WireEmptyResolved, WireStringPredicate,
    WireStringResolved, expression_is_resolved, set_matches, set_once_eq, valid_wire_name,
    validate_wire_fields, validate_wire_targets,
};
mod schema;
pub use schema::FilterSchema;

/// A predicate that evaluates a borrowed candidate.
///
/// Functions and closures with the same signature implement this trait.
///
/// # Examples
///
/// ```
/// use libtmux::query::Matcher;
///
/// struct IsEven;
///
/// impl Matcher<i32> for IsEven {
///     fn matches(&self, candidate: &i32) -> bool {
///         candidate % 2 == 0
///     }
/// }
///
/// assert!(IsEven.matches(&2));
/// assert!(!IsEven.matches(&3));
/// ```
pub trait Matcher<T> {
    /// Return whether `candidate` matches this predicate.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::Matcher;
    ///
    /// let is_positive = |candidate: &i32| *candidate > 0;
    /// assert!(is_positive.matches(&1));
    /// assert!(!is_positive.matches(&-1));
    /// ```
    fn matches(&self, candidate: &T) -> bool;
}

impl<T, F> Matcher<T> for F
where
    F: Fn(&T) -> bool,
{
    fn matches(&self, candidate: &T) -> bool {
        self(candidate)
    }
}

/// The ways an iterator can fail to contain exactly one item.
///
/// # Examples
///
/// ```
/// use libtmux::query::{ExactlyOneError, QueryIteratorExt};
///
/// let values: Vec<i32> = Vec::new();
/// assert_eq!(values.iter().exactly_one(), Err(ExactlyOneError::NoItems));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactlyOneError {
    /// The iterator contained no items.
    NoItems,
    /// The iterator contained more than one item.
    MultipleItems,
}

impl fmt::Display for ExactlyOneError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoItems => formatter.write_str("expected exactly one item, found none"),
            Self::MultipleItems => formatter.write_str("expected exactly one item, found multiple"),
        }
    }
}

impl std::error::Error for ExactlyOneError {}

/// An error indicating that an iterator contained more than one item.
///
/// # Examples
///
/// ```
/// use libtmux::query::{MultipleItemsError, QueryIteratorExt};
///
/// let values = [1, 2];
/// assert_eq!(values.iter().one_or_none(), Err(MultipleItemsError));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultipleItemsError;

impl fmt::Display for MultipleItemsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected at most one item, found multiple")
    }
}

impl std::error::Error for MultipleItemsError {}

/// Cardinality and named-predicate operations for borrowed iterators.
///
/// # Examples
///
/// ```
/// use libtmux::query::QueryIteratorExt;
///
/// let values = [1, 2, 3];
/// assert_eq!(values.iter().exactly_one(), Err(libtmux::query::ExactlyOneError::MultipleItems));
/// ```
#[allow(clippy::module_name_repetitions)]
pub trait QueryIteratorExt<'a, T: 'a>: Iterator<Item = &'a T> + Sized {
    /// Lazily yield candidates accepted by `matcher`.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{Matcher, QueryIteratorExt};
    ///
    /// struct IsEven;
    ///
    /// impl Matcher<i32> for IsEven {
    ///     fn matches(&self, candidate: &i32) -> bool {
    ///         candidate % 2 == 0
    ///     }
    /// }
    ///
    /// let values = [1, 2, 3, 4];
    /// let selected = values.iter().matching(IsEven).copied().collect::<Vec<_>>();
    /// assert_eq!(selected, [2, 4]);
    /// ```
    fn matching<M: Matcher<T>>(self, matcher: M) -> impl Iterator<Item = &'a T> {
        self.filter(move |candidate| matcher.matches(*candidate))
    }

    /// Return the only item, or an error for zero or multiple items.
    ///
    /// At most two items are pulled from the iterator.
    ///
    /// # Errors
    ///
    /// Returns [`ExactlyOneError::NoItems`] for an empty iterator and
    /// [`ExactlyOneError::MultipleItems`] when a second item is present.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::QueryIteratorExt;
    ///
    /// let values = [7];
    /// assert_eq!(values.iter().exactly_one(), Ok(&values[0]));
    /// ```
    fn exactly_one(mut self) -> Result<&'a T, ExactlyOneError> {
        let Some(item) = self.next() else {
            return Err(ExactlyOneError::NoItems);
        };

        if self.next().is_some() {
            Err(ExactlyOneError::MultipleItems)
        } else {
            Ok(item)
        }
    }

    /// Return zero or one item, or an error for multiple items.
    ///
    /// At most two items are pulled from the iterator.
    ///
    /// # Errors
    ///
    /// Returns [`MultipleItemsError`] when a second item is present.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::QueryIteratorExt;
    ///
    /// let empty: Vec<i32> = Vec::new();
    /// assert_eq!(empty.iter().one_or_none(), Ok(None));
    ///
    /// let values = [7];
    /// assert_eq!(values.iter().one_or_none(), Ok(Some(&values[0])));
    /// ```
    fn one_or_none(mut self) -> Result<Option<&'a T>, MultipleItemsError> {
        let item = self.next();
        if item.is_some() && self.next().is_some() {
            Err(MultipleItemsError)
        } else {
            Ok(item)
        }
    }
}

impl<'a, T: 'a, I> QueryIteratorExt<'a, T> for I where I: Iterator<Item = &'a T> + Sized {}

/// The category of an invalid portable filter expression.
///
/// Callers must retain a wildcard arm because future schema versions may add
/// more source-less validation categories.
///
/// # Examples
///
/// ```
/// use libtmux::query::FilterExpressionErrorKind;
///
/// let kind = FilterExpressionErrorKind::InvalidRegex;
/// assert_eq!(kind, FilterExpressionErrorKind::InvalidRegex);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FilterExpressionErrorKind {
    /// A regular expression did not compile under the version 1 dialect.
    InvalidRegex,
    /// The serialized expression schema version is unsupported.
    UnsupportedVersion,
    /// The expression target does not match the requested candidate type.
    InvalidTarget,
    /// The candidate schema has no field with the requested stable name.
    UnknownField,
    /// The field type does not support the requested operator.
    UnknownOperator,
    /// A relation does not support the requested quantifier.
    UnknownQuantifier,
    /// A literal cannot be represented by the field type.
    InvalidLiteral,
    /// The serialized expression exceeds a fixed decoder budget.
    ComplexityLimit,
    /// The expression tree has an invalid shape.
    InvalidStructure,
}

/// A source-less, value-free portable expression validation error.
///
/// The error retains only its category. In particular, it never retains a
/// regex pattern, field literal, or rejected serialized value.
///
/// # Examples
///
/// ```
/// use libtmux::query::{FilterExpressionErrorKind, TextField};
/// use libtmux::query::__private;
///
/// struct Row;
/// let field: TextField<Row> = __private::text_field("row", "name");
/// let error = field.regex("[").expect_err("the pattern is invalid");
/// assert_eq!(error.kind(), FilterExpressionErrorKind::InvalidRegex);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilterExpressionError {
    kind: FilterExpressionErrorKind,
}

impl FilterExpressionError {
    const fn new(kind: FilterExpressionErrorKind) -> Self {
        Self { kind }
    }

    /// Return the value-free validation category.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::FilterExpressionErrorKind;
    /// use libtmux::query::__private;
    ///
    /// let error = __private::unknown_field_error();
    /// assert_eq!(error.kind(), FilterExpressionErrorKind::UnknownField);
    /// ```
    #[must_use]
    pub const fn kind(&self) -> FilterExpressionErrorKind {
        self.kind
    }
}

impl fmt::Display for FilterExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            FilterExpressionErrorKind::InvalidRegex => "invalid regular expression",
            FilterExpressionErrorKind::UnsupportedVersion => "unsupported expression version",
            FilterExpressionErrorKind::InvalidTarget => "invalid expression target",
            FilterExpressionErrorKind::UnknownField => "unknown expression field",
            FilterExpressionErrorKind::UnknownOperator => "unknown expression operator",
            FilterExpressionErrorKind::UnknownQuantifier => "unknown relation quantifier",
            FilterExpressionErrorKind::InvalidLiteral => "invalid expression literal",
            FilterExpressionErrorKind::ComplexityLimit => "expression exceeds complexity limits",
            FilterExpressionErrorKind::InvalidStructure => "invalid expression structure",
        })
    }
}

impl std::error::Error for FilterExpressionError {}

/// Stable string semantics for a custom enum filter field.
///
/// Every value returned by [`FilterEnum::filter_name`] must be present in
/// [`FilterEnum::FILTER_VARIANTS`], and the variant names must be unique.
///
/// # Examples
///
/// ```
/// use libtmux::query::FilterEnum;
///
/// enum State {
///     Ready,
///     Blocked,
/// }
///
/// impl FilterEnum for State {
///     const FILTER_VARIANTS: &'static [&'static str] = &["ready", "blocked"];
///
///     fn filter_name(&self) -> &'static str {
///         match self {
///             Self::Ready => "ready",
///             Self::Blocked => "blocked",
///         }
///     }
/// }
///
/// assert_eq!(State::Ready.filter_name(), "ready");
/// # let _ = State::Blocked;
/// ```
pub trait FilterEnum {
    /// Every stable string accepted for this enum field.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::FilterEnum;
    ///
    /// enum State { Ready }
    /// impl FilterEnum for State {
    ///     const FILTER_VARIANTS: &'static [&'static str] = &["ready"];
    ///     fn filter_name(&self) -> &'static str { "ready" }
    /// }
    /// assert_eq!(State::FILTER_VARIANTS, ["ready"]);
    /// # let _ = State::Ready;
    /// ```
    const FILTER_VARIANTS: &'static [&'static str];

    /// Return this value's stable filter string.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::FilterEnum;
    ///
    /// enum State { Ready }
    /// impl FilterEnum for State {
    ///     const FILTER_VARIANTS: &'static [&'static str] = &["ready"];
    ///     fn filter_name(&self) -> &'static str { "ready" }
    /// }
    /// assert_eq!(State::Ready.filter_name(), "ready");
    /// ```
    fn filter_name(&self) -> &'static str;
}

/// A type with a stable schema that can evaluate portable filter predicates.
///
/// This trait is designed for generated implementations; the example spells
/// out that ABI by hand. Methods prefixed with `__filter_` are not ordinary
/// authoring hooks.
///
/// # Examples
///
/// ```
/// use std::error::Error as _;
///
/// use libtmux::query::{
///     BoolField, EnumField, FilterEnum, FilterExpressionError,
///     FilterExpressionErrorKind, Filterable, IntegerField, TextField,
/// };
/// use libtmux::query::__private::{self, IntegerKind, Predicate};
///
/// enum State {
///     Ready,
/// }
///
/// impl FilterEnum for State {
///     const FILTER_VARIANTS: &'static [&'static str] = &["ready"];
///
///     fn filter_name(&self) -> &'static str {
///         match self {
///             Self::Ready => "ready",
///         }
///     }
/// }
///
/// struct Task {
///     name: &'static [u8],
///     done: bool,
///     priority: i8,
///     retries: u8,
///     state: State,
/// }
///
/// struct TaskFields {
///     name: TextField<Task>,
///     done: BoolField<Task>,
///     priority: IntegerField<Task, i8>,
///     retries: IntegerField<Task, u8>,
///     state: EnumField<Task, State>,
/// }
///
/// impl Filterable for Task {
///     type Fields = TaskFields;
///     const FILTER_TARGET: &'static str = "task";
///
///     fn filter_fields() -> Self::Fields {
///         TaskFields {
///             name: __private::text_field(Self::FILTER_TARGET, "name"),
///             done: __private::bool_field(Self::FILTER_TARGET, "done"),
///             priority: __private::integer_field(Self::FILTER_TARGET, "priority"),
///             retries: __private::integer_field(Self::FILTER_TARGET, "retries"),
///             state: __private::enum_field(Self::FILTER_TARGET, "state"),
///         }
///     }
///
///     fn __filter_matches(&self, predicate: &Predicate) -> bool {
///         Self::__filter_validate(predicate)
///             .expect("typed field expressions must validate before matching");
///         match predicate.field() {
///             "name" => predicate.matches_text(self.name),
///             "done" => predicate.matches_bool(self.done),
///             "priority" => predicate.matches_signed(i128::from(self.priority)),
///             "retries" => predicate.matches_unsigned(u128::from(self.retries)),
///             "state" => predicate.matches_enum(self.state.filter_name()),
///             _ => false,
///         }
///     }
///
///     fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
///         match predicate.field() {
///             "name" => predicate.validate_text(),
///             "done" => predicate.validate_bool(),
///             "priority" => predicate.validate_integer(IntegerKind::I8),
///             "retries" => predicate.validate_integer(IntegerKind::U8),
///             "state" => predicate.validate_enum(State::FILTER_VARIANTS),
///             _ => Err(__private::unknown_field_error()),
///         }
///     }
/// }
///
/// let task = Task {
///     name: b"build",
///     done: false,
///     priority: -1,
///     retries: 2,
///     state: State::Ready,
/// };
/// let fields = Task::filter_fields();
/// assert!(fields.name.eq("build").matches(&task));
/// assert!(fields.done.eq(false).matches(&task));
/// assert!(fields.priority.eq(-1).matches(&task));
/// assert!(fields.retries.eq(2).matches(&task));
/// assert!(fields.state.eq(State::Ready).matches(&task));
///
/// let error = __private::unknown_field_error();
/// assert_eq!(error.kind(), FilterExpressionErrorKind::UnknownField);
/// assert!(error.source().is_none());
/// ```
///
/// Generated relation dispatch uses the same opaque predicate ABI for
/// already-loaded `Vec<T>` and `Option<T>` fields:
///
/// ```
/// use libtmux::query::{
///     BoolField, FilterExpressionError, Filterable, ManyRelation, OneRelation,
/// };
/// use libtmux::query::__private::{self, Predicate};
///
/// struct Child {
///     done: bool,
/// }
///
/// struct ChildFields {
///     done: BoolField<Child>,
/// }
///
/// impl Filterable for Child {
///     type Fields = ChildFields;
///     const FILTER_TARGET: &'static str = "child";
///
///     fn filter_fields() -> Self::Fields {
///         ChildFields {
///             done: __private::bool_field(Self::FILTER_TARGET, "done"),
///         }
///     }
///
///     fn __filter_matches(&self, predicate: &Predicate) -> bool {
///         assert!(Self::__filter_validate(predicate).is_ok());
///         match predicate.field() {
///             "done" => predicate.matches_bool(self.done),
///             _ => false,
///         }
///     }
///
///     fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
///         match predicate.field() {
///             "done" => predicate.validate_bool(),
///             _ => Err(__private::unknown_field_error()),
///         }
///     }
/// }
///
/// struct Parent {
///     children: Vec<Child>,
///     favorite: Option<Child>,
/// }
///
/// struct ParentFields {
///     children: ManyRelation<Parent, Child>,
///     favorite: OneRelation<Parent, Child>,
/// }
///
/// impl Filterable for Parent {
///     type Fields = ParentFields;
///     const FILTER_TARGET: &'static str = "parent";
///
///     fn filter_fields() -> Self::Fields {
///         ParentFields {
///             children: __private::many_relation(Self::FILTER_TARGET, "children"),
///             favorite: __private::one_relation(Self::FILTER_TARGET, "favorite"),
///         }
///     }
///
///     fn __filter_matches(&self, predicate: &Predicate) -> bool {
///         assert!(Self::__filter_validate(predicate).is_ok());
///         match predicate.field() {
///             "children" => predicate.matches_many(&self.children),
///             "favorite" => predicate.matches_one(self.favorite.as_ref()),
///             _ => false,
///         }
///     }
///
///     fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
///         match predicate.field() {
///             "children" => predicate.validate_many::<Child>(),
///             "favorite" => predicate.validate_one::<Child>(),
///             _ => Err(__private::unknown_field_error()),
///         }
///     }
/// }
///
/// let parent = Parent {
///     children: vec![Child { done: false }, Child { done: true }],
///     favorite: Some(Child { done: false }),
/// };
/// let parent_fields = Parent::filter_fields();
/// let child_done = Child::filter_fields().done;
/// assert!(parent_fields.children.any(child_done.eq(false)).matches(&parent));
/// assert!(
///     parent_fields
///         .children
///         .all(child_done.is_in([false, true]))
///         .matches(&parent)
/// );
/// assert!(
///     parent_fields
///         .children
///         .none(child_done.not_in([false, true]))
///         .matches(&parent)
/// );
/// assert!(parent_fields.favorite.is(child_done.eq(false)).matches(&parent));
/// ```
pub trait Filterable: Sized {
    /// The generated companion value containing this type's field handles.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::Filterable;
    ///
    /// fn generated_fields<T: Filterable>() -> T::Fields {
    ///     T::filter_fields()
    /// }
    /// let _ = generated_fields::<T>;
    /// # struct T;
    /// # impl Filterable for T {
    /// #     type Fields = ();
    /// #     const FILTER_TARGET: &'static str = "t";
    /// #     fn filter_fields() {}
    /// #     fn __filter_matches(&self, _: &libtmux::query::__private::Predicate) -> bool { false }
    /// #     fn __filter_validate(_: &libtmux::query::__private::Predicate) -> Result<(), libtmux::query::FilterExpressionError> { Ok(()) }
    /// # }
    /// ```
    type Fields;

    /// The stable target name used by portable expression envelopes.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::Filterable;
    ///
    /// fn target<T: Filterable>() -> &'static str {
    ///     T::FILTER_TARGET
    /// }
    /// # let _ = target::<T>;
    /// # struct T;
    /// # impl Filterable for T {
    /// #     type Fields = ();
    /// #     const FILTER_TARGET: &'static str = "t";
    /// #     fn filter_fields() {}
    /// #     fn __filter_matches(&self, _: &libtmux::query::__private::Predicate) -> bool { false }
    /// #     fn __filter_validate(_: &libtmux::query::__private::Predicate) -> Result<(), libtmux::query::FilterExpressionError> { Ok(()) }
    /// # }
    /// ```
    const FILTER_TARGET: &'static str;

    /// Return typed handles for this candidate schema.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::Filterable;
    ///
    /// fn generated_fields<T: Filterable>() -> T::Fields {
    ///     T::filter_fields()
    /// }
    /// # let _ = generated_fields::<T>;
    /// # struct T;
    /// # impl Filterable for T {
    /// #     type Fields = ();
    /// #     const FILTER_TARGET: &'static str = "t";
    /// #     fn filter_fields() {}
    /// #     fn __filter_matches(&self, _: &libtmux::query::__private::Predicate) -> bool { false }
    /// #     fn __filter_validate(_: &libtmux::query::__private::Predicate) -> Result<(), libtmux::query::FilterExpressionError> { Ok(()) }
    /// # }
    /// ```
    #[must_use]
    fn filter_fields() -> Self::Fields;

    /// Evaluate one already-validated predicate against this candidate.
    ///
    /// The trait-level example executes this method for every scalar field
    /// family in a hand-written generated-code expansion.
    #[doc(hidden)]
    fn __filter_matches(&self, predicate: &__private::Predicate) -> bool;

    /// Validate one opaque predicate against this candidate schema.
    ///
    /// # Errors
    ///
    /// Returns a source-less validation category for a schema mismatch.
    ///
    /// The trait-level example executes this method for every scalar field
    /// family in a hand-written generated-code expansion.
    #[doc(hidden)]
    fn __filter_validate(predicate: &__private::Predicate) -> Result<(), FilterExpressionError>;
}

/// An opaque, portable predicate over candidates of type `T`.
///
/// Expressions have ordered structural equality after adjacent `and` and
/// `or` nodes are flattened. Their debug representation includes structure
/// and stable schema names, but never literal values or lengths.
///
/// # Examples
///
/// ```
/// use libtmux::query::{FilterExpr, TextField};
/// use libtmux::query::__private;
///
/// struct Row;
/// let field: TextField<Row> = __private::text_field("row", "name");
/// let expression: FilterExpr<Row> = field.eq("build");
/// assert_eq!(expression, field.eq("build"));
/// ```
pub struct FilterExpr<T> {
    data: ExprData,
    marker: PhantomData<fn() -> T>,
}

impl<T> FilterExpr<T> {
    fn predicate(predicate: __private::Predicate) -> Self {
        Self {
            data: ExprData::Predicate(predicate),
            marker: PhantomData,
        }
    }

    /// Combine two expressions with ordered short-circuiting conjunction.
    ///
    /// Adjacent conjunction nodes are flattened without reordering operands.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::BoolField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: BoolField<Row> = __private::bool_field("row", "done");
    /// let expression = field.eq(false).and(field.eq(false));
    /// assert_eq!(expression, field.eq(false).and(field.eq(false)));
    /// ```
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        let mut expressions = match self.data {
            ExprData::And(expressions) => expressions,
            expression => vec![expression],
        };
        match other.data {
            ExprData::And(other_expressions) => expressions.extend(other_expressions),
            expression => expressions.push(expression),
        }
        Self {
            data: ExprData::And(expressions),
            marker: PhantomData,
        }
    }

    /// Combine two expressions with ordered short-circuiting disjunction.
    ///
    /// Adjacent disjunction nodes are flattened without reordering operands.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::BoolField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: BoolField<Row> = __private::bool_field("row", "done");
    /// let expression = field.eq(false).or(field.eq(true));
    /// assert_ne!(expression, field.eq(true).or(field.eq(false)));
    /// ```
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        let mut expressions = match self.data {
            ExprData::Or(expressions) => expressions,
            expression => vec![expression],
        };
        match other.data {
            ExprData::Or(other_expressions) => expressions.extend(other_expressions),
            expression => expressions.push(expression),
        }
        Self {
            data: ExprData::Or(expressions),
            marker: PhantomData,
        }
    }

    /// Negate this expression.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::BoolField;
    /// use libtmux::query::__private;
    ///
    /// struct Row;
    /// let field: BoolField<Row> = __private::bool_field("row", "done");
    /// assert_eq!(field.eq(false).not(), field.eq(false).not());
    /// ```
    #[must_use]
    // The named method mirrors the portable grammar; implementing `Not`
    // would add an operator surface outside this contract.
    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        Self {
            data: ExprData::Not(Box::new(self.data)),
            marker: PhantomData,
        }
    }
}

impl<T: Filterable> FilterExpr<T> {
    /// Evaluate this validated expression against `candidate`.
    ///
    /// Evaluation is infallible, ordered, and short-circuiting. Every text
    /// predicate first requires strict candidate UTF-8 and returns `false` for
    /// invalid bytes, including empty exclusion. An outer [`Self::not`] can
    /// invert that result.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::query::{BoolField, FilterExpressionError, Filterable};
    /// use libtmux::query::__private::{self, Predicate};
    ///
    /// struct Task(bool);
    /// struct Fields(BoolField<Task>);
    ///
    /// impl Filterable for Task {
    ///     type Fields = Fields;
    ///     const FILTER_TARGET: &'static str = "task";
    ///
    ///     fn filter_fields() -> Self::Fields {
    ///         Fields(__private::bool_field(Self::FILTER_TARGET, "done"))
    ///     }
    ///
    ///     fn __filter_matches(&self, predicate: &Predicate) -> bool {
    ///         predicate.matches_bool(self.0)
    ///     }
    ///
    ///     fn __filter_validate(_: &Predicate) -> Result<(), FilterExpressionError> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let expression = Task::filter_fields().0.eq(true);
    /// assert!(expression.matches(&Task(true)));
    /// ```
    #[must_use]
    pub fn matches(&self, candidate: &T) -> bool {
        evaluate(&self.data, candidate)
    }
}

impl<T> Clone for FilterExpr<T> {
    fn clone(&self) -> Self {
        Self {
            data: self.data.clone(),
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for FilterExpr<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("FilterExpr")
            .field(&RedactedExprDebug(&self.data))
            .finish()
    }
}

impl<T> PartialEq for FilterExpr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl<T> Eq for FilterExpr<T> {}

impl<T: Filterable> Matcher<T> for FilterExpr<T> {
    fn matches(&self, candidate: &T) -> bool {
        Self::matches(self, candidate)
    }
}

impl<T: Filterable> Matcher<T> for &FilterExpr<T> {
    fn matches(&self, candidate: &T) -> bool {
        FilterExpr::matches(self, candidate)
    }
}

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
    id: FieldId,
    marker: PhantomData<fn() -> T>,
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
    id: FieldId,
    marker: PhantomData<fn() -> T>,
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
    id: FieldId,
    marker: PhantomData<fn() -> (T, N)>,
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
    id: FieldId,
    marker: PhantomData<fn() -> (T, E)>,
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
    id: FieldId,
    marker: PhantomData<fn() -> (From, To)>,
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
    id: FieldId,
    marker: PhantomData<fn() -> (From, To)>,
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

/// Compatibility-sensitive support used by generated `Filterable` code.
///
/// These values are public only so generated implementations can name them.
/// Applications author expressions through typed field handles.
#[doc(hidden)]
#[path = "query/private.rs"]
pub mod __private;

#[cfg(feature = "serde")]
mod serde_v1;

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    #[cfg(feature = "serde")]
    use std::sync::OnceLock;

    #[cfg(feature = "serde")]
    use super::set_once_eq;
    use super::{
        __private, FieldId, FilterExpressionErrorKind, PredicateData, SetOperator, SetPredicate,
        TextOperator, TextPredicate,
    };

    const TEST_FIELD: FieldId = FieldId {
        target: "sentinel-target",
        field: "sentinel-field",
    };

    fn predicate(data: PredicateData) -> __private::Predicate {
        __private::Predicate::new(TEST_FIELD, data)
    }

    #[cfg(feature = "serde")]
    #[test]
    fn once_lock_set_failures_compare_the_installed_value() {
        let target = OnceLock::new();
        assert_eq!(
            set_once_eq(&target, "record", FilterExpressionErrorKind::InvalidTarget,),
            Ok(())
        );
        assert_eq!(
            set_once_eq(&target, "record", FilterExpressionErrorKind::InvalidTarget,),
            Ok(())
        );
        assert_eq!(
            set_once_eq(&target, "other", FilterExpressionErrorKind::InvalidTarget,)
                .map_err(|error| error.kind()),
            Err(FilterExpressionErrorKind::InvalidTarget)
        );

        let family = OnceLock::new();
        assert_eq!(
            set_once_eq(&family, 1_u8, FilterExpressionErrorKind::InvalidStructure,),
            Ok(())
        );
        assert_eq!(
            set_once_eq(&family, 1_u8, FilterExpressionErrorKind::InvalidStructure,),
            Ok(())
        );
        assert_eq!(
            set_once_eq(&family, 2_u8, FilterExpressionErrorKind::InvalidStructure,)
                .map_err(|error| error.kind()),
            Err(FilterExpressionErrorKind::InvalidStructure)
        );
    }

    fn assert_validation_error(
        result: Result<(), super::FilterExpressionError>,
        expected: FilterExpressionErrorKind,
    ) {
        assert_eq!(
            result.as_ref().map_err(super::FilterExpressionError::kind),
            Err(expected)
        );
        if let Err(error) = result {
            assert!(error.source().is_none());
            assert!(!format!("{error:?}").contains("sentinel"));
            assert!(!error.to_string().contains("sentinel"));
        }
    }

    #[test]
    fn scalar_validation_reports_wrong_predicate_families_as_unknown_operators() {
        let text = predicate(PredicateData::Text(TextPredicate {
            operator: TextOperator::Eq,
            values: vec![String::from("sentinel-text")],
            compiled_regex: None,
        }));
        let boolean = predicate(PredicateData::Bool(SetPredicate {
            operator: SetOperator::Eq,
            values: vec![true],
        }));

        assert_validation_error(
            boolean.validate_text(),
            FilterExpressionErrorKind::UnknownOperator,
        );
        assert_validation_error(
            text.validate_bool(),
            FilterExpressionErrorKind::UnknownOperator,
        );
        assert_validation_error(
            text.validate_integer(__private::IntegerKind::I8),
            FilterExpressionErrorKind::UnknownOperator,
        );
        assert_validation_error(
            text.validate_enum(&["sentinel-text"]),
            FilterExpressionErrorKind::UnknownOperator,
        );
    }

    #[test]
    fn scalar_validation_reports_malformed_shapes_as_invalid_structure() {
        let boolean_eq_without_value = predicate(PredicateData::Bool(SetPredicate {
            operator: SetOperator::Eq,
            values: Vec::new(),
        }));
        let signed_eq_with_two_values = predicate(PredicateData::Signed(SetPredicate {
            operator: SetOperator::Eq,
            values: vec![1, 2],
        }));
        let unsigned_eq_without_value = predicate(PredicateData::Unsigned(SetPredicate {
            operator: SetOperator::Eq,
            values: Vec::new(),
        }));
        let enum_eq_with_two_values = predicate(PredicateData::Enum(SetPredicate {
            operator: SetOperator::Eq,
            values: vec![String::from("ready"), String::from("blocked")],
        }));
        let text_eq_without_value = predicate(PredicateData::Text(TextPredicate {
            operator: TextOperator::Eq,
            values: Vec::new(),
            compiled_regex: None,
        }));
        let regex_without_compiled_state = predicate(PredicateData::Text(TextPredicate {
            operator: TextOperator::Regex,
            values: vec![String::from("sentinel-pattern")],
            compiled_regex: None,
        }));

        for result in [
            boolean_eq_without_value.validate_bool(),
            signed_eq_with_two_values.validate_integer(__private::IntegerKind::I8),
            unsigned_eq_without_value.validate_integer(__private::IntegerKind::U8),
            enum_eq_with_two_values.validate_enum(&["ready", "blocked"]),
            text_eq_without_value.validate_text(),
            regex_without_compiled_state.validate_text(),
        ] {
            assert_validation_error(result, FilterExpressionErrorKind::InvalidStructure);
        }
    }

    #[test]
    fn scalar_validation_reports_literal_mismatches_as_invalid_literal() {
        let signed_for_unsigned = predicate(PredicateData::Signed(SetPredicate {
            operator: SetOperator::Eq,
            values: vec![1],
        }));
        let signed_out_of_range = predicate(PredicateData::Signed(SetPredicate {
            operator: SetOperator::Eq,
            values: vec![i128::from(i8::MAX) + 1],
        }));
        let unsigned_out_of_range = predicate(PredicateData::Unsigned(SetPredicate {
            operator: SetOperator::Eq,
            values: vec![u128::from(u8::MAX) + 1],
        }));
        let unknown_enum_variant = predicate(PredicateData::Enum(SetPredicate {
            operator: SetOperator::Eq,
            values: vec![String::from("sentinel-variant")],
        }));

        for result in [
            signed_for_unsigned.validate_integer(__private::IntegerKind::U8),
            signed_out_of_range.validate_integer(__private::IntegerKind::I8),
            unsigned_out_of_range.validate_integer(__private::IntegerKind::U8),
            unknown_enum_variant.validate_enum(&["ready", "blocked"]),
        ] {
            assert_validation_error(result, FilterExpressionErrorKind::InvalidLiteral);
        }
    }
}
