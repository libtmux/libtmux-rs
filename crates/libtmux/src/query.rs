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
//! Rust authoring accepts any `IntoIterator`. Regex operators use Rust syntax
//! and reject Python-only look-around and backreferences with source-less,
//! value-free errors. Dynamic `field__operator` parsing and remote pushdown
//! belong to later ingress and execution layers.
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

#[derive(Clone, Copy, Eq, PartialEq)]
struct FieldId {
    target: &'static str,
    field: &'static str,
}

#[cfg(feature = "serde")]
fn set_once_eq<T: Eq>(
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

enum PredicateIdentity {
    Static(FieldId),
    #[cfg(feature = "serde")]
    Wire {
        target: OnceLock<&'static str>,
        field: String,
    },
}

impl PredicateIdentity {
    fn clone_internal(&self) -> Self {
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

    fn field(&self) -> &str {
        match self {
            Self::Static(id) => id.field,
            #[cfg(feature = "serde")]
            Self::Wire { field, .. } => field,
        }
    }

    fn target(&self) -> &str {
        match self {
            Self::Static(id) => id.target,
            #[cfg(feature = "serde")]
            Self::Wire { target, .. } => target.get().copied().unwrap_or_default(),
        }
    }

    fn bind_target(&self, target: &'static str) -> Result<(), FilterExpressionError> {
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

#[derive(Clone, Copy, Eq, PartialEq)]
#[cfg_attr(
    not(feature = "serde"),
    allow(
        dead_code,
        reason = "ordering reaches text only from the wire, which serde gates"
    )
)]
enum TextOperator {
    Eq,
    EqIgnoreCase,
    Contains,
    ContainsIgnoreCase,
    StartsWith,
    StartsWithIgnoreCase,
    EndsWith,
    EndsWithIgnoreCase,
    In,
    NotIn,
    Regex,
    RegexIgnoreCase,
    Lt,
    Lte,
    Gt,
    Gte,
}

impl TextOperator {
    const fn label(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::EqIgnoreCase => "eq_ignore_case",
            Self::Contains => "contains",
            Self::ContainsIgnoreCase => "contains_ignore_case",
            Self::StartsWith => "starts_with",
            Self::StartsWithIgnoreCase => "starts_with_ignore_case",
            Self::EndsWith => "ends_with",
            Self::EndsWithIgnoreCase => "ends_with_ignore_case",
            Self::In => "in",
            Self::NotIn => "not_in",
            Self::Regex => "regex",
            Self::RegexIgnoreCase => "regex_ignore_case",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Gt => "gt",
            Self::Gte => "gte",
        }
    }

    /// Report whether this operator compares order, which text cannot.
    const fn is_ordering(self) -> bool {
        matches!(self, Self::Lt | Self::Lte | Self::Gt | Self::Gte)
    }

    const fn is_regex(self) -> bool {
        matches!(self, Self::Regex | Self::RegexIgnoreCase)
    }

    #[cfg(feature = "serde")]
    const fn set_operator(self) -> Option<SetOperator> {
        match self {
            Self::Eq => Some(SetOperator::Eq),
            Self::In => Some(SetOperator::In),
            Self::NotIn => Some(SetOperator::NotIn),
            Self::Lt => Some(SetOperator::Lt),
            Self::Lte => Some(SetOperator::Lte),
            Self::Gt => Some(SetOperator::Gt),
            Self::Gte => Some(SetOperator::Gte),
            Self::EqIgnoreCase
            | Self::Contains
            | Self::ContainsIgnoreCase
            | Self::StartsWith
            | Self::StartsWithIgnoreCase
            | Self::EndsWith
            | Self::EndsWithIgnoreCase
            | Self::Regex
            | Self::RegexIgnoreCase => None,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SetOperator {
    Eq,
    In,
    NotIn,
    Lt,
    Lte,
    Gt,
    Gte,
}

impl SetOperator {
    /// Report whether this operator compares order rather than membership.
    ///
    /// An ordering operator takes exactly one bound, where `in` takes a set.
    const fn is_ordering(self) -> bool {
        matches!(self, Self::Lt | Self::Lte | Self::Gt | Self::Gte)
    }

    /// Report whether this operator's operand is a set rather than one value.
    ///
    /// Only the wire cares: the typed handles fix the arity at the call site.
    #[cfg(feature = "serde")]
    const fn takes_a_set(self) -> bool {
        matches!(self, Self::In | Self::NotIn)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RelationQuantifier {
    Any,
    All,
    None,
    Is,
}

impl RelationQuantifier {
    const fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
            Self::None => "none",
            Self::Is => "is",
        }
    }
}

impl SetOperator {
    const fn label(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::In => "in",
            Self::NotIn => "not_in",
            Self::Lt => "lt",
            Self::Lte => "lte",
            Self::Gt => "gt",
            Self::Gte => "gte",
        }
    }
}

#[derive(Clone)]
struct TextPredicate {
    operator: TextOperator,
    values: Vec<String>,
    compiled_regex: Option<Regex>,
}

impl PartialEq for TextPredicate {
    fn eq(&self, other: &Self) -> bool {
        self.operator == other.operator && self.values == other.values
    }
}

impl Eq for TextPredicate {}

#[derive(Clone, Eq, PartialEq)]
struct SetPredicate<T> {
    operator: SetOperator,
    values: Vec<T>,
}

#[derive(Clone, Eq, PartialEq)]
struct RelationPredicate {
    quantifier: RelationQuantifier,
    expression: Box<ExprData>,
}

#[cfg(feature = "serde")]
#[derive(Clone, Eq, PartialEq)]
enum WireStringResolved {
    Text(TextPredicate),
    Signed(SetPredicate<i128>),
    Unsigned(SetPredicate<u128>),
    Enum(SetPredicate<String>),
}

#[cfg(feature = "serde")]
struct WireStringPredicate {
    operator: TextOperator,
    values: Vec<String>,
    resolved: OnceLock<WireStringResolved>,
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
struct WireBoolPredicate {
    operator: SetOperator,
    values: Vec<bool>,
    resolved: OnceLock<()>,
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
enum WireEmptyResolved {
    Text,
    Bool,
    Signed,
    Unsigned,
    Enum,
}

#[cfg(feature = "serde")]
struct WireEmptyPredicate {
    operator: SetOperator,
    resolved: OnceLock<WireEmptyResolved>,
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
fn set_matches<T: PartialOrd>(operator: SetOperator, values: &[T], candidate: &T) -> bool {
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
    fn matches(&self, candidate: &T) -> bool {
        set_matches(self.operator, &self.values, candidate)
    }

    /// Report whether the operator and its operands agree.
    ///
    /// `eq` takes one value and an ordering operator takes one bound; only
    /// `in` and `not_in` take a set.
    fn has_valid_shape(&self) -> bool {
        if self.operator == SetOperator::Eq || self.operator.is_ordering() {
            self.values.len() == 1
        } else {
            true
        }
    }
}

#[derive(Clone)]
enum PredicateData {
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
    fn operator_label(&self) -> &str {
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
    fn scalar(&self) -> Option<ResolvedScalar<'_>> {
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
    fn is_resolved(&self) -> bool {
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

impl TextOperator {
    /// Return the text operator matching a set operator, when one exists.
    ///
    /// Text has no ordering, so an ordering operator has no text form. One
    /// cannot reach here in practice: this converts an empty operand list,
    /// and an ordering operator with no bound fails its shape check first.
    #[cfg(feature = "serde")]
    const fn from_set(operator: SetOperator) -> Option<Self> {
        match operator {
            SetOperator::Eq => Some(Self::Eq),
            SetOperator::In => Some(Self::In),
            SetOperator::NotIn => Some(Self::NotIn),
            SetOperator::Lt | SetOperator::Lte | SetOperator::Gt | SetOperator::Gte => None,
        }
    }
}

#[cfg(feature = "serde")]
#[derive(Clone, Copy)]
enum ResolvedScalar<'a> {
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

fn evaluate<T: Filterable>(expression: &ExprData, candidate: &T) -> bool {
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

fn validate_expression<T: Filterable>(expression: &ExprData) -> Result<(), FilterExpressionError> {
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
fn valid_wire_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(feature = "serde")]
fn validate_wire_targets(
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
fn validate_wire_fields(expression: &ExprData) -> Result<(), FilterExpressionError> {
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
fn expression_is_resolved(expression: &ExprData) -> bool {
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

enum ExprData {
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

struct RedactedExprDebug<'a>(&'a ExprData);

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
pub mod __private {
    use super::{
        BoolField, EnumField, FieldId, FilterEnum, FilterExpressionError,
        FilterExpressionErrorKind, Filterable, IntegerField, ManyRelation, OneRelation,
        PhantomData, PredicateData, PredicateIdentity, RelationPredicate, RelationQuantifier,
        SetOperator, TextField, TextOperator, default_case_fold_str, evaluate, validate_expression,
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
        fn signed_bounds(&self) -> Option<(i128, i128)> {
            match self {
                Self::I8 => Some((i128::from(i8::MIN), i128::from(i8::MAX))),
                Self::I16 => Some((i128::from(i16::MIN), i128::from(i16::MAX))),
                Self::I32 => Some((i128::from(i32::MIN), i128::from(i32::MAX))),
                Self::I64 => Some((i128::from(i64::MIN), i128::from(i64::MAX))),
                Self::I128 => Some((i128::MIN, i128::MAX)),
                Self::U8 | Self::U16 | Self::U32 | Self::U64 | Self::U128 => None,
            }
        }

        fn unsigned_max(&self) -> Option<u128> {
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
                TextOperator::Lt | TextOperator::Lte | TextOperator::Gt | TextOperator::Gte => {
                    false
                }
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
                (RelationQuantifier::Is, Some(candidate)) => {
                    evaluate(&relation.expression, candidate)
                }
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
                                .case_insensitive(
                                    predicate.operator == TextOperator::RegexIgnoreCase,
                                )
                                .build()
                                .map_err(|_| {
                                    FilterExpressionError::new(
                                        FilterExpressionErrorKind::InvalidRegex,
                                    )
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
                PredicateData::Bool(predicate) => validate_set_shape(predicate),
                #[cfg(feature = "serde")]
                PredicateData::WireBool(predicate) => {
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

    fn validate_enum_values(
        predicate: &super::SetPredicate<String>,
        variants: &[&str],
    ) -> Result<(), FilterExpressionError> {
        validate_set_shape(predicate)?;
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
    fn resolve_once<T: Eq>(
        cell: &super::OnceLock<T>,
        value: T,
    ) -> Result<(), FilterExpressionError> {
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
    fn parse_signed(
        value: &str,
        minimum: i128,
        maximum: i128,
    ) -> Result<i128, FilterExpressionError> {
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
    pub const fn integer_field<T, N>(
        target: &'static str,
        field: &'static str,
    ) -> IntegerField<T, N> {
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
}

#[cfg(feature = "serde")]
mod serde_v1 {
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

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
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

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
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

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
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
                "and" | "or" => {
                    self.has_only_args_members() && Self::validate_args(self.args.as_ref())
                }
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
                Some(WireValue::Strings(values)) => {
                    PredicateData::WireString(WireStringPredicate {
                        operator: wire_operator,
                        values,
                        resolved: super::OnceLock::new(),
                    })
                }
                Some(WireValue::Bool(value)) => PredicateData::WireBool(WireBoolPredicate {
                    operator: wire_operator.set_operator().ok_or_else(|| {
                        expression_error(FilterExpressionErrorKind::InvalidStructure)
                    })?,
                    values: vec![value],
                    resolved: super::OnceLock::new(),
                }),
                Some(WireValue::Bools(values)) => PredicateData::WireBool(WireBoolPredicate {
                    operator: wire_operator.set_operator().ok_or_else(|| {
                        expression_error(FilterExpressionErrorKind::InvalidStructure)
                    })?,
                    values,
                    resolved: super::OnceLock::new(),
                }),
                Some(WireValue::Empty) => PredicateData::WireEmpty(WireEmptyPredicate {
                    operator: wire_operator.set_operator().ok_or_else(|| {
                        expression_error(FilterExpressionErrorKind::InvalidStructure)
                    })?,
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
}

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
