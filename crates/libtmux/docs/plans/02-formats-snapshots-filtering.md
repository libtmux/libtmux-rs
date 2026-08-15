# Formats, snapshots, winlinks, and filtering plan

## Outcome

Build the byte-preserving format and snapshot core that discovery will use,
and ship the independent iterator/filtering API for downstream data. The
slice must not introduce `QueryList`, `QuerySet`, a server snapshot aggregate,
or a public query plan.

The public filtering path is:

```rust
let fields = Task::filter_fields();
let expression = fields.name.starts_with("build");
let task = tasks.iter().matching(&expression).exactly_one()?;
```

Ordinary closures stay on the standard library path:

```rust
let pending = tasks.iter().filter(|task| !task.done);
```

The private format path renders every trusted descriptor once through tmux's
`q` modifier, terminates each escaped field with an unescaped `%`, and parses
raw `CommandResult` bytes before any line or UTF-8 operation. Intrinsic object
snapshots and winlink projections remain crate-private until the discovery
slice gives them a public consumer.

## Fixed decisions

- Listing results are ordered `Vec<T>` snapshots. No collection wrapper is
  exported.
- `QueryIteratorExt` applies only to `Iterator<Item = &T>`.
- `Matcher<T>` has a zero-boxing blanket implementation for `Fn(&T) -> bool`.
  Documentation sends inline closures to native `.filter()` because the MSRV
  cannot infer an untyped closure through the blanket matcher bound.
- `FilterExpr<T>` is the only serializable or remotely lowerable predicate.
- Built-in field handles are generated inside `libtmux`; the optional
  downstream derive macro is not involved in built-in querying.
- A relation expression evaluates only data already held by its candidate.
  It never performs tmux I/O and never treats an unloaded relation as empty.
- Local `.matching()` never pushes down. Remote query execution and residual
  partitioning are deferred.
- `field__operator` parsing belongs to later CLI, MCP, or configuration edges.
- The format parser accepts all non-NUL bytes. Tmux uses C strings, so it
  cannot emit embedded NUL.
- Every list profile requires its baseline identity descriptor. A
  `FormatPlan` with no selected descriptor is invalid, so empty stdout means
  zero rows and never an ambiguous sequence of zero-slot rows.
- Fields unavailable under the detected capability policy are omitted from the
  rendered template. A release below a field's floor becomes `Unsupported`;
  a release-less development build becomes `Unproven` for every post-baseline
  field rather than claiming upstream absence. Zero-byte output from a
  supported field is interpreted by that descriptor's empty policy:
  optional numeric and ID values may become `Absent`, while empty-valid text
  remains `Available` with a zero-byte `TmuxText`. Scope-inapplicable fields make plan
  construction fail. Tmux does not expose whether zero bytes came from a
  callback returning `NULL` or an empty C string, so a descriptor must never
  claim to distinguish those cases.
- Public errors and `Debug` implementations never retain or render raw row
  bytes, snapshot text, regex literals, or serialized filter values.
- Regex validation converts `regex::Error` into a source-less typed kind; it
  never retains the pattern-bearing source.
- No commit, tag, push, or publication is part of this plan.

## Public surface for this slice

The root crate exports or documents these values:

- `TmuxText`;
- `query::Matcher`;
- `query::QueryIteratorExt`;
- `query::ExactlyOneError`;
- `query::MultipleItemsError`;
- `query::FilterExpr`;
- `query::Filterable`;
- `query::FilterEnum`;
- typed string, boolean, integer, enum, to-one, and to-many field handles;
- `query::FilterExpressionError`;
- `query::FilterExpressionErrorKind`;
- `#[derive(libtmux::Filterable)]` behind the `derive` feature;
- `Serialize` and `Deserialize` for `FilterExpr<T>` behind the `serde`
  feature.

All filter authoring types, errors, handles, traits, and the hidden expansion
ABI live under `libtmux::query`; only the optional derive macro is re-exported
from the crate root.

The extension trait stays in `libtmux::query`; there is no prelude and no root
glob import. This limits method-name collisions with `itertools::Itertools`.

The crate-private snapshot surface contains:

- `Availability<T>`;
- `SessionInfo`, `WindowInfo`, `PaneInfo`, and `ClientInfo`;
- `WindowLinkIdentity` and `WindowLink`;
- `WindowProjection` and `PaneProjection`;
- the descriptor catalog, `FormatPlan`, framed-row parser, decoders, and
  snapshot builders;
- built-in scalar field handles generated from the same catalog.

The crate-private availability state is exact:

```rust
pub(crate) enum Availability<T> {
    Unsupported,
    Unproven,
    Absent,
    Available(T),
}
```

`Unsupported` means a detected release is below the descriptor's floor;
`Unproven` means the release-less development identifier provides no truthful
version decision. Both are omitted from the wire format, but they remain
distinct in the built snapshot.

The discovery slice promotes only snapshot values used by its public handle
and listing APIs. This avoids exporting values that users cannot obtain.

`TmuxText` has one named raw constructor and three explicit views:

```rust
impl TmuxText {
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Self;
    pub fn as_bytes(&self) -> &[u8];
    pub fn as_str(&self) -> Result<&str, std::str::Utf8Error>;
    pub fn to_string_lossy(&self) -> std::borrow::Cow<'_, str>;
}
```

`From<&str>`, `From<String>`, and `From<Vec<u8>>` are provided. There is no
implicit text view or conversion from `TmuxText` into `String`.

The iterator contract is fixed to this shape:

```rust
pub trait Matcher<T> {
    fn matches(&self, candidate: &T) -> bool;
}

pub trait QueryIteratorExt<'a, T: 'a>:
    Iterator<Item = &'a T> + Sized
{
    fn matching<M: Matcher<T>>(
        self,
        matcher: M,
    ) -> impl Iterator<Item = &'a T>;

    fn exactly_one(self) -> Result<&'a T, ExactlyOneError>;

    fn one_or_none(
        self,
    ) -> Result<Option<&'a T>, MultipleItemsError>;
}
```

Only the blanket `Matcher<T> for F where F: Fn(&T) -> bool` and specific
owned and borrowed `FilterExpr<T>` implementations are provided. A blanket
implementation for `&M` overlaps the closure implementation on the MSRV and
is forbidden. `ExactlyOneError::{NoItems, MultipleItems}` is exhaustive;
both cardinality errors are value-free, `Copy + Eq`, implement `Display` and
`std::error::Error`, and retain no iterator items.

Expression validation uses one source-less, value-free error surface:

```rust
#[non_exhaustive]
pub enum FilterExpressionErrorKind {
    InvalidRegex,
    UnsupportedVersion,
    InvalidTarget,
    UnknownField,
    UnknownOperator,
    UnknownQuantifier,
    InvalidLiteral,
    InvalidStructure,
}

pub struct FilterExpressionError {
    kind: FilterExpressionErrorKind,
}

impl FilterExpressionError {
    pub const fn kind(&self) -> FilterExpressionErrorKind;
}
```

Both values are `Clone + Copy + Debug + Eq + Send + Sync`; the error
implements value-free `Display` and `std::error::Error`, and its source is
always `None`.

## Portable expression grammar

Serde uses an explicit adapter rather than deriving on the internal AST. A
top-level expression carries its schema version and stable target name:

```json
{
  "version": 1,
  "target": "task",
  "expr": {
    "op": "starts_with",
    "field": "name",
    "value": "build"
  }
}
```

The feature implements `Serialize` and `Deserialize` only for
`FilterExpr<T>` where `T: Filterable`. Deserialization validates the envelope
target and every field/operator/literal pairing through `T` before returning
the otherwise-infallible expression value.

Boolean nodes use ordered argument arrays:

```json
{
  "version": 1,
  "target": "task",
  "expr": {
    "op": "and",
    "args": [
      { "op": "eq", "field": "done", "value": false },
      { "op": "contains", "field": "name", "value": "build" }
    ]
  }
}
```

Relations carry an explicit quantifier and nested expression:

```json
{
  "version": 1,
  "target": "workspace",
  "expr": {
    "op": "relation",
    "field": "tasks",
    "quantifier": "any",
    "expr": { "op": "eq", "field": "done", "value": false }
  }
}
```

Membership always uses an array value:

```json
{
  "version": 1,
  "target": "task",
  "expr": {
    "op": "in",
    "field": "name",
    "value": ["build", "test"]
  }
}
```

Version 1 supports:

- `and`, `or`, and `not`;
- text `eq`, `eq_ignore_case`, `contains`, `contains_ignore_case`,
  `starts_with`, `starts_with_ignore_case`, `ends_with`,
  `ends_with_ignore_case`, `in`, `not_in`, `regex`, and
  `regex_ignore_case`;
- boolean, integer, and enum `eq`, `in`, and `not_in`;
- relation quantifiers `any`, `all`, `none`, and `is`.

String evaluation requires strict UTF-8 in the candidate. Invalid UTF-8 never
matches a portable string predicate; a native closure can inspect the raw
bytes. Rust regex syntax is authoritative. Invalid patterns fail during
construction or deserialization, not during `matches()`.

The scalar `*_ignore_case` operators apply Unicode 16.0 default case folding
to both operands, without normalization, through `caseless` 0.2.2. Prefix,
suffix, and substring tests run on those folded strings, so for example
`"Straße"` equals `"STRASSE"` ignoring case. `regex_ignore_case` instead uses
the exact Rust `regex` 1.13.1 grammar with `regex-syntax` 0.8.11 and its
Unicode 16.0 case tables because folding the pattern as plain text would
change regex syntax. Schema version 1 implies that dialect. These are
deliberately separate semantics; changing accepted regex syntax or either
Unicode table requires fixture review and a schema-version increment.

The wire `in` and `not_in` operators always take a JSON array and mean
candidate membership in that array. For a candidate that decodes under the
field type, `in []` is false and `not_in []` is true. A text candidate with
invalid UTF-8 fails the field precondition first, so every text predicate,
including `not_in`, returns false. A string right-hand side is invalid;
Python's distinct string-RHS substring behavior is an intentional delta and
the Rust `contains` operators cover that use case. Rust field handles name the
membership constructor `is_in()` because `in` is a keyword.

Boolean literals use JSON booleans. Text and enum literals use JSON strings.
The fixed-width integer types `i8`, `i16`, `i32`, `i64`, `i128`, `u8`, `u16`,
`u32`, `u64`, and `u128` use canonical base-10 JSON strings so TypeScript can
decode them as `BigInt` without precision loss. Leading plus signs, leading
zeroes other than `"0"`, `"-0"`, negative unsigned values, and values outside
the field's Rust type are invalid. Platform-width `isize` and `usize` are not
filterable.

Every wire object is closed: unknown members are rejected in the serde DTOs
and the JSON Schema uses `additionalProperties: false` recursively. A host
document embeds an expression as `"where": <FilterExpr envelope>`; `where`
is not an AST node or member. Any new member, literal representation, or node
form requires a schema-version increment.

The checked-in JSON Schema describes the target-independent version 1 grammar.
It can reject unknown operators, quantifiers, members, and structural literal
types, but it cannot know fields supplied by an arbitrary downstream
`Filterable` derive. Target names, field names, enum variants, and the exact
integer type range receive their authoritative validation during typed Rust
deserialization; tests must not claim the generic schema rejects those
application-specific semantic errors.

Duplicate member names are invalid at every object level. Rust deserializes
from raw JSON directly into duplicate-rejecting DTO visitors. JSON Schema
cannot enforce this after an ordinary parser has collapsed an object, so the
future TypeScript, MCP, CLI, and configuration edges must use a duplicate-aware
raw-text parse before creating their JSON data model; `JSON.parse` alone is
not a conforming ingress for this grammar.

Version 1 has exactly these closed node shapes:

| Node               | Exact members                       | Constraint                                 |
| ------------------ | ----------------------------------- | ------------------------------------------ |
| `and`, `or`        | `op`, `args`                        | At least two ordered expressions           |
| `not`              | `op`, `expr`                        | `op` is `not`                              |
| Scalar or regex    | `op`, `field`, `value`              | Literal and operator fit the typed field   |
| Relation           | `op`, `field`, `quantifier`, `expr` | `op` is `relation`; nested target is typed |
| Top-level envelope | `version`, `target`, `expr`         | `version` is `1`; target matches `T`       |

Empty and one-operand junctions are invalid rather than silently normalized.
The scalar field definition determines which literal form and operators are
legal; deserialization validates that typed pairing before it constructs
`FilterExpr<T>`.

Stable target and field names use the ASCII lower-snake grammar
`[a-z][a-z0-9_]*`. The derive strips Rust's raw-identifier prefix before
validating a default field name and validates every explicit target or rename
at compile time. The JSON Schema applies the same pattern before typed
deserialization resolves the name.

`and` and `or` flatten adjacent nodes of the same kind while preserving input
order. Evaluation short-circuits left to right. Equality is structural after
that flattening; it does not reorder commutative operands. `FilterExpr` does
not implement `Hash` because future engine fingerprints must come from the
canonical wire representation rather than Rust's process-local hashing.

## Typed authoring API

The public handle types are `TextField<T>`, `BoolField<T>`,
`IntegerField<T, N>`, and `EnumField<T, E>`, plus the relation handles below.
Constructors are hidden for generated code. Handles are `Clone + Copy + Eq`
and `Send + Sync`, contain only stable target/field identity, and use manual
`Debug` that exposes only those schema names. Their consuming methods are:

```rust
impl<T> TextField<T> {
    pub fn eq(self, value: impl Into<String>) -> FilterExpr<T>;
    pub fn eq_ignore_case(self, value: impl Into<String>) -> FilterExpr<T>;
    pub fn contains(self, value: impl Into<String>) -> FilterExpr<T>;
    pub fn contains_ignore_case(
        self,
        value: impl Into<String>,
    ) -> FilterExpr<T>;
    pub fn starts_with(self, value: impl Into<String>) -> FilterExpr<T>;
    pub fn starts_with_ignore_case(
        self,
        value: impl Into<String>,
    ) -> FilterExpr<T>;
    pub fn ends_with(self, value: impl Into<String>) -> FilterExpr<T>;
    pub fn ends_with_ignore_case(
        self,
        value: impl Into<String>,
    ) -> FilterExpr<T>;
    pub fn is_in<I, S>(self, values: I) -> FilterExpr<T>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>;
    pub fn not_in<I, S>(self, values: I) -> FilterExpr<T>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>;
    pub fn regex(
        self,
        pattern: impl Into<String>,
    ) -> Result<FilterExpr<T>, FilterExpressionError>;
    pub fn regex_ignore_case(
        self,
        pattern: impl Into<String>,
    ) -> Result<FilterExpr<T>, FilterExpressionError>;
}
```

```rust
impl<T> BoolField<T> {
    pub fn eq(self, value: bool) -> FilterExpr<T>;
    pub fn is_in(
        self,
        values: impl IntoIterator<Item = bool>,
    ) -> FilterExpr<T>;
    pub fn not_in(
        self,
        values: impl IntoIterator<Item = bool>,
    ) -> FilterExpr<T>;
}

impl<T, E: FilterEnum> EnumField<T, E> {
    pub fn eq(self, value: E) -> FilterExpr<T>;
    pub fn is_in(
        self,
        values: impl IntoIterator<Item = E>,
    ) -> FilterExpr<T>;
    pub fn not_in(
        self,
        values: impl IntoIterator<Item = E>,
    ) -> FilterExpr<T>;
}
```

`IntegerField<T, N>` has those same three signatures over its exact `N`,
with separate inherent implementations for the ten supported integer types.
No public scalar conversion trait or untyped field constructor is added.

```rust
impl<T> FilterExpr<T> {
    pub fn and(self, other: Self) -> Self;
    pub fn or(self, other: Self) -> Self;
    pub fn not(self) -> Self;
}

impl<T: Filterable> FilterExpr<T> {
    pub fn matches(&self, candidate: &T) -> bool;
}

impl<From, To> ManyRelation<From, To> {
    pub fn any(self, expression: FilterExpr<To>) -> FilterExpr<From>;
    pub fn all(self, expression: FilterExpr<To>) -> FilterExpr<From>;
    pub fn none(self, expression: FilterExpr<To>) -> FilterExpr<From>;
}

impl<From, To> OneRelation<From, To> {
    pub fn is(self, expression: FilterExpr<To>) -> FilterExpr<From>;
}
```

## Derive contract

The derive macro accepts named structs and requires an explicit stable target
name:

```rust
#[derive(libtmux::Filterable)]
#[filterable(target = "task")]
struct Task {
    name: String,
    done: bool,
}
```

It generates a companion fields value returned by
`Filterable::filter_fields()`:

```rust
let fields = Task::filter_fields();
let expression = fields.name.contains("build").and(fields.done.eq(false));
```

The macro infers `String`, `TmuxText`, `bool`, integer primitives, and their
`Option<T>` forms. Relations require explicit attributes so `Option<T>` and
`Vec<T>` are not guessed:

```rust
#[derive(libtmux::Filterable)]
#[filterable(target = "workspace")]
struct Workspace {
    #[filterable(many)]
    tasks: Vec<Task>,
    #[filterable(one)]
    owner: Option<User>,
}
```

`#[filterable(skip)]` omits local implementation fields. A field's stable wire
name defaults to its Rust identifier and may be fixed with
`#[filterable(rename = "stable_name")]`. An explicit crate-path override is
available for unusual macro environments; normal dependency renaming is
resolved through `proc-macro-crate`.

Custom enum fields require `#[filterable(enum)]` and an implementation of:

```rust
pub trait FilterEnum {
    const FILTER_VARIANTS: &'static [&'static str];

    fn filter_name(&self) -> &'static str;
}
```

Variant names are stable wire strings and must be unique. Every value's
`filter_name()` must be a member of `FILTER_VARIANTS`; this is the trait's
semantic law. The expression stores only that string; the enum itself is never
serialized. The derive does not infer enum semantics from Rust type names.

The generated companion type exposes documented public handle fields, has a
deterministic `<Struct>Fields` name, matches the input type's visibility, and
supports `#[filterable(fields = "CustomFields")]` for collisions. It is
`Clone + Copy + Debug + Eq + Send + Sync` without adding bounds to candidate
or related types. The macro preserves generics and existing where clauses.
Tuple structs, enums, unions, unsupported field types, duplicate wire names,
invalid target names, and incompatible relation annotations are compile
errors with field-local spans.

The runtime ABI used by generated code is deliberately small but
compatibility-sensitive. `Filterable` has this complete contract:

```rust
pub trait Filterable: Sized {
    type Fields;

    const FILTER_TARGET: &'static str;

    fn filter_fields() -> Self::Fields;

    #[doc(hidden)]
    fn __filter_matches(
        &self,
        predicate: &__private::Predicate,
    ) -> bool;

    #[doc(hidden)]
    fn __filter_validate(
        predicate: &__private::Predicate,
    ) -> Result<(), FilterExpressionError>;
}
```

The `query::__private` module exposes only the following expansion ABI;
`Predicate` itself remains opaque and typed handles have hidden constructors:

`PredicateData` below denotes private storage and is not part of the generated
code ABI.

```rust
pub struct Predicate {
    private: PredicateData,
}

pub enum IntegerKind {
    I8, I16, I32, I64, I128,
    U8, U16, U32, U64, U128,
}

pub const fn text_field<T>(
    target: &'static str,
    field: &'static str,
) -> TextField<T>;

pub const fn bool_field<T>(
    target: &'static str,
    field: &'static str,
) -> BoolField<T>;

pub const fn integer_field<T, N>(
    target: &'static str,
    field: &'static str,
) -> IntegerField<T, N>;

pub const fn enum_field<T, E: FilterEnum>(
    target: &'static str,
    field: &'static str,
) -> EnumField<T, E>;

pub const fn many_relation<From, To>(
    target: &'static str,
    field: &'static str,
) -> ManyRelation<From, To>;

pub const fn one_relation<From, To>(
    target: &'static str,
    field: &'static str,
) -> OneRelation<From, To>;

pub const fn unknown_field_error() -> FilterExpressionError;

impl Predicate {
    pub fn field(&self) -> &str;
    pub fn matches_text(&self, candidate: &[u8]) -> bool;
    pub fn matches_bool(&self, candidate: bool) -> bool;
    pub fn matches_signed(&self, candidate: i128) -> bool;
    pub fn matches_unsigned(&self, candidate: u128) -> bool;
    pub fn matches_enum(&self, candidate: &str) -> bool;
    pub fn matches_many<U: Filterable>(&self, values: &[U]) -> bool;
    pub fn matches_one<U: Filterable>(&self, value: Option<&U>) -> bool;

    pub fn validate_text(&self) -> Result<(), FilterExpressionError>;
    pub fn validate_bool(&self) -> Result<(), FilterExpressionError>;
    pub fn validate_integer(
        &self,
        kind: IntegerKind,
    ) -> Result<(), FilterExpressionError>;
    pub fn validate_enum(
        &self,
        variants: &[&str],
    ) -> Result<(), FilterExpressionError>;
    pub fn validate_many<U: Filterable>(
        &self,
    ) -> Result<(), FilterExpressionError>;
    pub fn validate_one<U: Filterable>(
        &self,
    ) -> Result<(), FilterExpressionError>;
}
```

Generated code dispatches only on `Predicate::field()` and these helpers.
`Predicate` carries stable target and field identity, never an access closure.
A generated `__filter_matches` unknown-field arm returns `false`; the matching
`__filter_validate` arm returns `unknown_field_error()`, so deserialization
rejects the same node before it can be evaluated.
Scalar validation classifies a predicate family that the field does not
support as `UnknownOperator`, malformed scalar cardinality or regex state as
`InvalidStructure`, and an integer range, signedness, or enum-variant mismatch
as `InvalidLiteral`. Regex compilation failure remains `InvalidRegex`.
A missing optional scalar always yields a nonmatch; explicit `not` can invert
that result. `ManyRelation<From, To>::any/all/none` and
`OneRelation<From, To>::is` accept only `FilterExpr<To>`. These hidden public
items are not an authoring API, but downstream expansions make them SemVer
sensitive; exact core/macro version alignment and compatibility fixtures guard
the boundary.

The crate override syntax is
`#[filterable(crate = "some::renamed_libtmux")]`. Its string value must parse
as a `syn::Path`; non-string and malformed-path values are compile errors.
Without the override, the macro resolves dependency renaming through
`proc-macro-crate`.

## Workspace and packaging

Add `libtmux-macros` as a sibling proc-macro package in the `rs/` workspace.
The root remains the `libtmux` package and both packages are default workspace
members. Shared package metadata and lints live at workspace scope.

The root dependency is exact-version and optional:

```toml
derive = ["dep:libtmux-macros"]
```

The `serde` feature is independent:

```toml
serde = ["dep:serde"]
```

The development Cargo toolchain packages and verifies the unpublished
dependency-ordered workspace. Rust 1.85 builds and tests every workspace
package but is not used for prepublication packaging because that Cargo
version cannot resolve the unpublished sibling from a temporary registry.
Publication order remains derive package before core package; publication
requires separate user authorization.

## Deferred work

- Public `Session`, `Window`, `Pane`, and `Client` handles and snapshot
  promotion.
- Built-in relation fields until a candidate type owns hydrated relation data.
- `server.query_sessions`, `query_windows`, `query_panes`, and
  `query_clients`.
- Native tmux `-f` lowering, residual partitioning, prepared work, ordering,
  and limits.
- Dynamic `field__operator` parsing.
- A public raw format-token selector.
- A monolithic snapshot graph or non-atomic server aggregate.
- Engine statement, program, batching, registry, or control-mode APIs.

## Task 1: Lock the iterator and cardinality contract

**Files:**

- Create: `rs/tests/query.rs`
- Modify: `rs/src/lib.rs`

Write the public integration RED before `src/query.rs` exists. Cover:

- `Matcher<T>` for a named downstream matcher;
- `Matcher<T>` for an explicitly typed closure;
- native `.filter()` with an untyped closure;
- `vec.iter().matching(matcher)` preserving order;
- matcher evaluation remains lazy;
- `exactly_one()` returns the sole borrowed item;
- zero and multiple inputs produce distinct `ExactlyOneError` variants;
- `one_or_none()` returns `None`, one item, or `MultipleItemsError`;
- both cardinality methods pull at most two items;
- `ExactlyOneError` remains exhaustive and both cardinality errors are
  source-less;
- trait adapters and errors are `Send + Sync` where their type parameters
  permit it;
- cardinality error `Display` output is value-free.

Add a compile-fail doctest proving `vec.into_iter().matching(...)` is outside
the extension trait's receiver contract. Add prose documenting UFCS when
`itertools::Itertools` is also imported.

Record the missing-API RED:

```console
$ cargo test \
    --locked \
    --manifest-path rs/Cargo.toml \
    --test query
```

## Task 2: Implement the borrowed iterator kernel

**Files:**

- Create: `rs/src/query.rs`
- Modify: `rs/src/lib.rs`

Implement:

- `Matcher<T>::matches(&self, &T) -> bool`;
- the blanket `Fn(&T) -> bool` implementation;
- `QueryIteratorExt<'a, T>` for `Iterator<Item = &'a T> + Sized`;
- an opaque `impl Iterator<Item = &'a T>` from `matching()`;
- `ExactlyOneError::{NoItems, MultipleItems}`;
- exhaustive, source-less `Copy + Eq + Display + Error` implementations for
  both cardinality errors;
- at-most-two-pull cardinality algorithms.

Do not add a closure-specific matching method, an owned-iterator
implementation, a prelude, `IntoIterator` bounds, collection methods, or an
itertools dependency.

Every public method gets an ordinary executable doctest. Run the focused
test, doctests, Rust 1.85 check, Clippy, and formatting before continuing.

```console
$ rustup run 1.85.0 cargo test \
    --locked \
    --manifest-path rs/Cargo.toml \
    --test query
```

## Task 3: Lock the public `TmuxText` boundary

**Files:**

- Create: `rs/tests/tmux_text.rs`
- Modify: `rs/src/lib.rs`

Record a public missing-API RED before `src/formats.rs` exists. Cover:

- construction from UTF-8 strings and arbitrary bytes;
- exact `as_bytes()`;
- borrowed strict `as_str()` returning `std::str::Utf8Error`;
- explicitly named lossy `to_string_lossy()`;
- raw-byte `Clone`, `Eq`, `Hash`, and `Ord` semantics;
- `Send + Sync`;
- value- and length-free manual `Debug`;
- absence of `Display`, `Deref<str>`, `AsRef<str>`, and conversion into
  `String`.

Name the mutation each test catches: implicit lossy conversion, Unicode-based
equality, or diagnostics retaining bytes or lengths. Record the focused RED:

```console
$ cargo test \
    --locked \
    --manifest-path rs/Cargo.toml \
    --test tmux_text
```

## Task 4: Implement `TmuxText`

**Files:**

- Create: `rs/src/formats.rs`
- Modify: `rs/src/lib.rs`

Implement immutable owned byte storage, bytewise value traits, explicit
byte/strict/lossy views, and a redacted manual `Debug`. Do not add implicit
string conversion, validation, normalization, or tmux parsing. Every public
method gets an ordinary executable doctest. Run the focused integration test,
doctests, Rust 1.85 check, Clippy, and formatting before continuing.

## Task 5: Lock scalar expression semantics

**Files:**

- Modify: `rs/tests/query.rs`
- Modify: `rs/src/query.rs`

Add RED tests for:

- string equality, containment, prefix, suffix, case-insensitive variants,
  array-valued membership, exclusion, and regex;
- Unicode 16.0 default-folding behavior including `Straße`/`STRASSE`, no
  implicit normalization of canonically equivalent strings, and the distinct
  Rust-regex Unicode case-insensitive semantics;
- boolean, every supported fixed-width integer (including both extrema), and
  enum equality and array-valued membership;
- no platform-width integer support; canonical decimal-string syntax remains
  part of the Task 10 serde RED;
- invalid regex construction;
- ordered `and`, `or`, and `not` evaluation with short-circuit probes;
- adjacent junction flattening and structural equality;
- `Clone`, `Debug`, `Eq`, `Send`, and `Sync` without adding bounds to `T`;
- field and relation handles retain their value traits without adding bounds
  to candidate, literal-enum, or related types;
- redacted `Debug` that omits literal values and lengths;
- owned and borrowed `FilterExpr<T>` matcher implementations;
- invalid UTF-8 candidate text is a portable nonmatch;
- no `Hash` or `Display` implementation;
- bool fields have no string operators through compile-fail documentation;
- every hidden field and relation constructor in the generated-code ABI
  produces the expected stable target/field identity;
- a hand-written downstream expansion compiles a generated-style validation
  fallback returning the hidden source-less unknown-field error, whose kind,
  source, and diagnostics are tested directly. Typed deserialization invokes
  that fallback in Task 10.

Use a private hand-written fixture implementation of `Filterable` to test the
public kernel before the derive macro exists. Record the behavioral REDs
before adding expression production.

## Task 6: Implement the opaque expression tree

**Files:**

- Modify: `rs/src/query.rs`
- Modify: `rs/src/error.rs`
- Modify: `rs/Cargo.toml`
- Modify: `rs/Cargo.lock`

Implement the smallest private AST that satisfies Task 5:

- stable field IDs and typed literals;
- scalar operators;
- boxed recursive negation nodes;
- ordered vector junctions;
- validated compiled regex state excluded from equality and diagnostics;
- a public `Filterable` trait with documented derive-first usage and the exact
  hidden evaluation/schema signatures frozen above;
- the public `FilterEnum` contract for explicit custom-enum fields;
- typed field handles with hidden constructors for generated code;
- total, infallible `FilterExpr::matches()` after construction validation;
- manual structural trait implementations that do not constrain `T`.

Add exact `regex = "=1.13.1"`, `regex-syntax = "=0.8.11"`, and
`caseless = "=0.2.2"` normal dependencies only when their failing tests
require them. The direct syntax constraint keeps the version 1 regex dialect
stable for downstream Cargo resolution. Keep `regex::Regex` private and store
compiled state separately from the portable pattern and flags used for
equality and serialization. Use `caseless` only for Unicode 16.0 default
folding; do not normalize candidate or literal text.
Map validation failures to source-less error kinds without retaining
`regex::Error`.

Keep hidden expansion support in the specified `query::__private` module. It
is public only because a downstream proc macro must reference it; it is not a
second supported authoring API, but its names and signatures are a
compatibility-sensitive generated-code ABI.

Implement relation handle value types and hidden constructors in this task,
but defer relation AST nodes plus `matches_many`, `matches_one`,
`validate_many`, and `validate_one` behavior until Task 7 records its
truth-table RED. Every public authoring method has an ordinary executable
doctest. Each doc-hidden ABI function and scalar method introduced here is
covered by executable documentation of the hand-written expansion; Task 7
does the same for relation methods, so hidden generated-code support is not
exempt from the repository's documentation gate.

## Task 7: Add relation semantics before the derive

**Files:**

- Modify: `rs/tests/query.rs`
- Modify: `rs/src/query.rs`

Add hand-written downstream-style fixtures containing `Vec<T>` and
`Option<T>`. Record RED tests for:

- `any(empty) == false`;
- `all(empty) == true`;
- `none(empty) == true`;
- `is(None) == false`;
- nested scalar expressions under every quantifier;
- a relation accepts only `FilterExpr<Related>`, not closures or arbitrary
  matchers;
- a relation never invokes an async or external callback;
- relation AST equality and redacted diagnostics.

Implement
`ManyRelation<From, To>::any/all/none(FilterExpr<To>) -> FilterExpr<From>` and
`OneRelation<From, To>::is(FilterExpr<To>) -> FilterExpr<From>` only after the
truth-table RED is recorded. Add the private recursive relation AST nodes and
the hidden `Predicate::{matches_many,matches_one,validate_many,validate_one}`
methods in the same GREEN. Do not expose built-in hierarchy relations yet.

## Task 8: Convert the package into a publishable workspace

**Files:**

- Modify: `rs/Cargo.toml`
- Modify: `rs/Cargo.lock`
- Modify: `rs/justfile`
- Modify: `.github/workflows/rust.yml`
- Create: `rs/libtmux-macros/Cargo.toml`
- Create: `rs/libtmux-macros/src/lib.rs`
- Create: `rs/scripts/check-package-contents.sh`

Add the sibling proc-macro package with exact version alignment, Edition 2024,
MSRV 1.85, inherited metadata, inherited lints, and a narrow include list.
Move shared lints and package values to workspace tables without changing the
published core metadata.

Add optional `derive` and optional `serde` dependencies to the core while
retaining the proven normal regex and `caseless` dependencies. Keep
`serde_json`, `jsonschema` 0.49.9 with default features disabled, and macro
test tooling in development dependencies. The validator retains Rust 1.85
support and validates Draft 2020-12 without network resolvers.
Update every aggregate command to select the workspace where required,
including `cargo hack --workspace`. Preserve the explicit Rust 1.85 commands
in CI so the closer `rust-toolchain.toml` cannot select the development
compiler accidentally. The `just check` aggregate invokes the portable Bash
package-content checker after Cargo's package verification, and development CI
runs that same script directly without assuming just exists on the runner.

Before macro production exists, record a feature RED that imports
`libtmux::Filterable` as a derive macro. Then prove:

- current Cargo can package and verify both unpublished packages;
- Rust 1.85 checks and tests both packages;
- `cargo check -p libtmux --no-default-features` source-gates serde and derive,
  while a normal-edge `cargo tree` assertion proves neither optional package
  is linked and separate workspace checks still compile the macro package;
- each-feature and all-feature workspace checks include the macro package;
- the packaged core does not contain the macro source tree;
- the packaged macro has no normal or build dependency back to the core;
- a Bash 3.2-compatible package-content check uses `nullglob`, explicit
  one-level globs, and newline-delimited membership checks to compare both
  `cargo package --list` outputs to the live schema, JSON fixture, and
  trybuild `.rs`/`.stderr` trees, requires every such file in its owning
  tarball, and rejects the macro tree from the core tarball.

The macro package may use a path-only development dependency on `libtmux` for
trybuild and executable documentation. This test-only backedge is permitted
and stripped from the published dependency graph; a normal or build
dependency would create the forbidden publication cycle.

## Task 9: Implement `#[derive(Filterable)]`

**Files:**

- Modify: `rs/libtmux-macros/src/lib.rs`
- Create: `rs/libtmux-macros/tests/derive.rs`
- Create: `rs/libtmux-macros/tests/ui/pass/*.rs`
- Create: `rs/libtmux-macros/tests/ui/fail/*.rs`
- Create: `rs/libtmux-macros/tests/ui/fail/*.stderr`
- Create: `rs/tests/filter_derive.rs`
- Modify: `rs/Cargo.toml`
- Modify: `rs/src/lib.rs`

Record trybuild failures before macro expansion code. Cover:

- named scalar struct pass;
- generic struct and where-clause pass;
- stable target and field rename pass;
- explicit enum field plus `FilterEnum` pass;
- crate-path override pass;
- malformed crate-path override fail;
- skipped field pass;
- one and many relation pass;
- tuple struct, enum, union, missing target, duplicate wire name, unsupported
  field, invalid target or field rename, conflicting relation annotation, and
  invalid attribute fail;
- bool handle has no `contains` method;
- relation handle requires a portable expression of the related type.

Generate one companion fields type and the `Filterable` implementation. Use
`proc-macro-crate` for dependency renaming and `syn::Error` aggregation for
field-local diagnostics. Do not parse Rust type names beyond the supported
scalar wrappers and explicit relation attributes.

The public integration test must exercise the macro through the root re-export
and query a downstream `Vec<T>`. The macro package's narrow include list must
contain every UI `.rs` and checked-in `.stderr` file. A renamed path-only
development dependency exercises `proc-macro-crate`; a hand-written expansion
fixture in the core integration tests locks the hidden ABI used by previously
expanded downstream code. Every exported macro and generated public method
has executable documentation.

## Task 10: Freeze serde version 1

**Files:**

- Create: `rs/schema/filter-v1.schema.json`
- Create: `rs/tests/filter_serde.rs`
- Create: `rs/tests/fixtures/filter-v1/*.json`
- Modify: `rs/Cargo.toml`
- Modify: `rs/src/query.rs`
- Modify: `rs/README.md`

Record feature-gated RED tests for every node form and literal type. Cover:

- exact golden JSON output;
- deserialize and reserialize stability;
- duplicate `version`, `op`, `field`, `value`, `args`, `quantifier`, and
  `expr` members rejected from raw text across every node class;
- target validation against `T`;
- an unknown field reaches the hand-written downstream validation fallback
  and returns the hidden source-less `UnknownField` error;
- lower-snake target and field-name grammar in both schema and serde;
- unknown version, target, field, operator, quantifier, and extra required
  shape failures;
- literal type mismatch;
- `in` and `not_in` accept arrays only, define empty-array truth values,
  reject string RHS, and retain array order;
- canonical signed and unsigned decimal strings, every fixed-width boundary,
  and out-of-range rejection;
- stable enum variant strings and unknown-variant rejection;
- Unicode 16.0 default-folding fixtures for `*_ignore_case`, including
  multi-character folds, plus a distinct regex-ignore-case fixture;
- invalid regex rejection during deserialization;
- nested relations;
- redacted errors and `Debug` across sentinel values;
- serde disabled under no-default-features;
- schema and fixtures included in the package.

Compile the checked-in Draft 2020-12 schema with the development-only
`jsonschema` validator, validate the schema against its meta-schema, validate
every golden fixture against it, and prove representative missing, extra, and
structurally wrong-type members fail both schema validation and serde decoding.
Prove target, field, enum, and integer-range errors separately through typed
serde decoding. Every object in the schema has `additionalProperties: false`.

Implement serde through private wire DTOs. Do not derive serde directly on the
internal AST and do not add `serde_json` to normal dependencies.

## Task 11: Record the format codec RED

**Files:**

- Modify: `rs/src/formats.rs`
- Create: `rs/src/snapshot.rs`
- Modify: `rs/src/lib.rs`

Keep format production absent while adding private unit tests for:

- empty stream as zero rows;
- rejection of a zero-descriptor plan before execution;
- zero-length field preservation;
- colon and old Unicode separator bytes;
- embedded LF and CR;
- invalid UTF-8;
- a payload-ending LF followed by `%` and then the row LF;
- literal backslash, percent, and every punctuation byte escaped by tmux's
  `q` modifier;
- multiple fields and rows in exact order;
- a dangling backslash escape and missing unescaped `%` field terminator;
- missing row LF, CRLF terminator, and trailing garbage;
- ASCII decoder rejection of non-ASCII input;
- text decoder acceptance of arbitrary non-NUL bytes;
- a template that evaluates every descriptor exactly once;
- a plan and parser that cannot disagree on descriptor order.

Run the private test and record failure on missing `FormatPlan`, parser, and
decoder symbols before adding production.

## Task 12: Implement byte-preserving format plans

**Files:**

- Modify: `rs/src/formats.rs`
- Modify: `rs/src/snapshot.rs`
- Modify: `rs/src/lib.rs`
- Modify: `rs/src/internal/core.rs`
- Modify: `rs/src/error.rs`

Implement:

- private `ListProfile`, semantic owner, context requirement, decoder kind,
  and empty-policy values;
- private catalog entries with verified minimum versions;
- `FormatPlan` owning descriptor order, profile, version, nonempty baseline
  identity, template, and parser;
- `#{q:field}%` rendering from trusted ASCII field names;
- byte scanning in which backslash consumes exactly one following byte and
  only an unescaped `%` ends the field;
- exact LF row termination;
- one unescaped immutable buffer per decoded row, with raw slots represented
  as ranges into that shared buffer so quoting bytes never enter `TmuxText`;
- field-specific typed decoding;
- source-less safe error kinds containing row, field, expected type, offset,
  and framing phase only;
- conservative baseline-only field selection for development versions that do
  not identify a release.

Tmux 3.2a's `q` modifier evaluates the callback once, prefixes every literal
backslash and percent with a backslash, and leaves arbitrary non-NUL payload
bytes available to the parser. The decoder removes each quoting backslash and
uses only the unescaped `%` added by the template as the field terminator.
Embedded LF is therefore payload until the final field terminator; exactly one
following LF ends the row. A dangling escape, missing terminator, missing row
LF, or extra byte is a loud framing error. There is no field-level retry or
splice path.

Do not accept caller-supplied field names or expose the internal plan. Do not
use `split`, `split_lines`, lossy decoding, owned UTF-8 errors, or raw bytes in
error sources.

## Task 13: Build the modeled snapshot catalog

**Files:**

- Modify: `rs/src/formats.rs`
- Modify: `rs/src/snapshot.rs`
- Modify: `rs/src/target.rs`
- Modify: `rs/docs/parity.md`

Audit the pinned tmux static format tables and list-context defaults at tags
3.2a, 3.3, 3.4, 3.5, 3.6, and 3.7. Do not infer ownership solely from token
prefixes.

For every cataloged field, record:

- stable tmux/wire name;
- semantic owner;
- required context;
- admitted list profiles;
- minimum release;
- typed decoder;
- empty policy.

Correct the Python version table for the six missed 3.3 fields:
`client_uid`, `client_user`, `next_session_id`, `pane_start_path`, `uid`, and
`user`. Exclude buffer, event-only, and config-only fields from object
snapshots even when `format_defaults()` can populate them incidentally.

Treat every comma-joined name callback such as `session_*_list` and
`window_*_list` as opaque `TmuxText`. Tmux does not escape commas in names, so
these values are not a lossless `Vec<T>` grammar. High-level relations derive
from typed projection rows and IDs instead. Numeric comma grammars such as
`pane_tabs` and `session_stack` remain opaque until separately audited.

Generate crate-private intrinsic info values and scalar field handles only for
fields admitted to `SessionInfo`, `WindowInfo`, `PaneInfo`, or `ClientInfo`.
Server/global, list-metadata, buffer, command/config, event, and copy-mode
entries remain checked catalog classifications in this slice; there is no
otherwise-unused `ServerInfo` or public handle for them. Keep active-child
values from session and client rows out of the parent's intrinsic data.
Client attachment/history fields, client-specific window-view fields, and the
six window relation aggregates are likewise catalog-only until a discovery
consumer defines their exact projection types. Task 14 may inspect the raw
linked-session aggregate semantics, but it does not silently store those
values in `ClientInfo` or `WindowInfo`.

Make the corrected semantic-owner, required-context, and version matrix in
`parity.md` a checked fixture. Add table-driven availability REDs proving:

- a 3.3-only field on 3.2a is omitted and becomes `Unsupported`;
- a post-baseline field on a release-less development build is omitted and
  becomes `Unproven`, never `Unsupported`;
- supported optional numeric or ID zero bytes become `Absent`;
- supported empty-valid text becomes `Available` with a zero-byte `TmuxText`;
- invalid typed bytes are a decode error;
- a scope-inapplicable field is a plan-construction error;
- two text descriptors interpret the same zero-byte slot differently:
  `pane_mode` becomes `Absent`, while `pane_path` becomes
  `Available` with a zero-byte `TmuxText`;
- a comma-containing Session name remains one exact opaque `_list` text value
  and is never split into invented relation items.

## Task 14: Lock winlink projection behavior

**Files:**

- Modify: `rs/src/snapshot.rs`
- Modify: `rs/src/target.rs`

Add private parser and real-tmux RED tests proving:

- one window linked at indices 1 and 5 in one session yields two ordered
  `WindowProjection` values with one `WindowId` and distinct link identities;
- server-wide pane enumeration yields one `PaneProjection` per winlink;
- intrinsic window values agree across projections;
- point lookup follows current-link-else-lowest-index behavior within a
  session;
- the local point selector rejects mixed-session rows rather than selecting a
  server-wide projection;
- enumeration never applies point-lookup deduplication;
- linked holder sessions deduplicate in first-seen order;
- with two same-Session links, raw `window_linked_sessions` is `2` through
  tmux 3.5 but `1` from 3.6, where it counts each containing Session group and
  each ungrouped Session once; its `_list` form repeats the Session name across
  the entire supported range;
- `window_linked` uses the current Session-group-relative meaning through 3.5
  and the global greater-than-one-winlink meaning from 3.6, while remaining a
  `WindowLink` field across versions;
- the high-level holder-session relation derives from projections and
  deduplicates independently of both raw aggregate callbacks;
- client attachment fields do not become intrinsic client identity.

Implement `WindowLinkIdentity` from server identity, session ID, index, and
window ID. Keep `WindowInfo` free of session/index/link flags and keep
`PaneInfo` free of session/index discovery edges.

Explicitly defer cross-session point resolution to the discovery slice. Its
real-tmux RED must link one window into multiple sessions and issue the
targeted lookup to tmux (`-t @id`); Rust must not choose a row from a
server-wide `-a` listing locally.

## Task 15: Prove the real tmux byte path

**Files:**

- Modify: `rs/src/formats.rs`
- Modify: `rs/src/snapshot.rs`
- Create: `rs/scripts/test-tmux-format-compat.sh`
- Modify: `.github/workflows/rust.yml`

Use `TestServer` and real tmux 3.2a to place colon, embedded LF, literal `%`,
literal backslash, the old separator bytes, and invalid UTF-8 in a format
value. Execute the listing through `Server::cmd`, parse
`CommandResult::stdout()` directly, and assert the exact original bytes. This
proves the floor tmux's quoting transform as well as the Rust decoder.

Add one Linux compatibility harness that builds annotated tag 3.2a and tag
3.6, verifies their peeled commits (`3b929f3` and `0dac7fe`) and exact
`tmux -V` output, then runs the same `real_tmux_compat_` format, aggregate,
and projection tests with each binary supplied explicitly to `TestServer`.
The script owns a temporary build root and cleans it on every exit. It never
falls back to ambient `tmux`, skips a version, or mutates process-global PATH.
Add a dedicated workflow job for this harness; distro tmux in development or
MSRV jobs is not evidence for either side of the 3.6 semantic boundary.

The test must use an observable ready condition, a unique socket, and bounded
cleanup. It must not mutate process-global environment or use fixed sleeps.

## Task 16: Documentation and parity closure for the slice

**Files:**

- Modify: `rs/README.md`
- Modify: `rs/docs/design.md`
- Modify: `rs/docs/roadmap.md`
- Modify: `rs/docs/parity.md`
- Modify: `rs/src/lib.rs`
- Modify: public module documentation

Document:

- `Vec<T>` and `.iter()` as the collection floor;
- `.filter()` versus `.matching()`;
- the `itertools` method-name caveat;
- raw `TmuxText` views;
- exact cardinality boundaries;
- portable filter grammar and feature flags;
- strict candidate UTF-8 semantics;
- relation hydration requirements;
- non-atomic listing snapshots;
- Python QueryList defect corrections and regex syntax delta;
- private snapshot status pending the discovery consumer.

Advance parity rows only for symbols and behavior present at this gate. Keep
future remote query, dynamic parser, discovery, and public snapshot rows
planned.

## Task 17: Independent reviews and full gate

Run independent read-only reviews for:

- public API, SemVer, trait coherence, and method discoverability;
- parser safety, integer bounds, byte preservation, and diagnostics;
- proc-macro diagnostics, generated-code hygiene, and dependency renaming;
- serde schema stability and TypeScript implementability;
- winlink ownership and real-tmux behavior;
- MSRV, feature powerset, workspace packaging, and package contents;
- engine-ops compatibility without adding a planner.

Resolve every Critical or Important finding test-first. Then run:

```console
$ cargo fmt \
    --manifest-path rs/Cargo.toml \
    --all \
    -- \
    --check
```

```console
$ cargo clippy \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --all-targets \
    --all-features \
    -- \
    -D warnings
```

```console
$ cargo test \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --all-targets \
    --all-features
```

```console
$ cargo test \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --doc \
    --all-features
```

```console
$ RUSTDOCFLAGS='-D warnings' cargo doc \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --all-features \
    --no-deps
```

```console
$ cargo check \
    --locked \
    --manifest-path rs/Cargo.toml \
    --package libtmux \
    --all-targets \
    --no-default-features
```

```console
$ bash -o pipefail -c \
    'tree="$(cargo tree --locked --manifest-path rs/Cargo.toml --package libtmux --no-default-features --edges normal --prefix none)" && \
    ! printf "%s\n" "$tree" | rg -q "^(serde|serde_core|serde_derive|libtmux-macros) v"'
```

```console
$ cargo hack check \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --each-feature \
    --all-targets
```

```console
$ rustup run 1.85.0 cargo test \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --all-targets \
    --all-features
```

```console
$ rustup run 1.85.0 cargo hack check \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --each-feature \
    --all-targets
```

```console
$ rustup run 1.97.1 cargo package \
    --locked \
    --manifest-path rs/Cargo.toml \
    --workspace \
    --all-features \
    --allow-dirty
```

```console
$ just --justfile rs/justfile package
```

```console
$ just --justfile rs/justfile check
```

Run the full suite against the pinned tmux 3.2a binary after the final source
change, then run the pinned 3.2a/3.6 format compatibility harness:

```console
$ bash rs/scripts/test-tmux-format-compat.sh
```

Confirm no owned daemon, client, or descendant process remains. Do not claim
the slice complete while any test, lint, documentation, feature, MSRV,
package, or real-tmux gate fails.
