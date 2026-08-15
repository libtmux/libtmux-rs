//! Public contract tests for borrowed query iterator extensions.

#![cfg(feature = "query")]

use std::cell::{Cell, RefCell};
use std::error::Error as StdError;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::rc::Rc;

use libtmux::TmuxText;
use libtmux::query::{
    __private::{self, IntegerKind, Predicate},
    BoolField, EnumField, ExactlyOneError, FilterEnum, FilterExpr, FilterExpressionError,
    FilterExpressionErrorKind, Filterable, IntegerField, ManyRelation, Matcher, MultipleItemsError,
    OneRelation, QueryIteratorExt, TextField,
};
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(ExactlyOneError: Clone, Copy, Debug, Display, Eq, StdError, Send, Sync);
assert_impl_all!(MultipleItemsError: Clone, Copy, Debug, Display, Eq, StdError, Send, Sync);
assert_impl_all!(FilterExpressionErrorKind: Clone, Copy, Debug, Eq, Send, Sync);
assert_impl_all!(FilterExpressionError: Clone, Copy, Debug, Display, Eq, StdError, Send, Sync);

#[allow(dead_code)]
struct NoTraits(Rc<()>);

#[allow(dead_code)]
struct RelatedNoTraits(Rc<()>);

#[allow(dead_code)]
struct EnumNoTraits(Rc<()>);

impl FilterEnum for EnumNoTraits {
    const FILTER_VARIANTS: &'static [&'static str] = &["only"];

    fn filter_name(&self) -> &'static str {
        "only"
    }
}

assert_impl_all!(FilterExpr<NoTraits>: Clone, Debug, Eq, Send, Sync);
assert_not_impl_any!(FilterExpr<String>: Display, Hash);
assert_impl_all!(TextField<NoTraits>: Clone, Copy, Debug, Eq, Send, Sync);
assert_impl_all!(BoolField<NoTraits>: Clone, Copy, Debug, Eq, Send, Sync);
assert_impl_all!(IntegerField<NoTraits, i64>: Clone, Copy, Debug, Eq, Send, Sync);
assert_impl_all!(EnumField<NoTraits, EnumNoTraits>: Clone, Copy, Debug, Eq, Send, Sync);
assert_impl_all!(ManyRelation<NoTraits, RelatedNoTraits>: Clone, Copy, Debug, Eq, Send, Sync);
assert_impl_all!(OneRelation<NoTraits, RelatedNoTraits>: Clone, Copy, Debug, Eq, Send, Sync);

const TEXT_HANDLE: TextField<NoTraits> = __private::text_field("task", "name");
const BOOL_HANDLE: BoolField<NoTraits> = __private::bool_field("task", "done");
const INTEGER_HANDLE: IntegerField<NoTraits, i64> = __private::integer_field("task", "count");
const ENUM_HANDLE: EnumField<NoTraits, EnumNoTraits> = __private::enum_field("task", "state");
const MANY_HANDLE: ManyRelation<NoTraits, RelatedNoTraits> =
    __private::many_relation("task", "children");
const ONE_HANDLE: OneRelation<NoTraits, RelatedNoTraits> = __private::one_relation("task", "owner");
const UNKNOWN_FIELD_ERROR: FilterExpressionError = __private::unknown_field_error();
const UNKNOWN_FIELD_KIND: FilterExpressionErrorKind = UNKNOWN_FIELD_ERROR.kind();

#[test]
fn scalar_authoring_does_not_require_candidate_traits() {
    let text = TEXT_HANDLE;
    let _ = text.eq(IntoStringOnly("name"));
    let _ = text.eq_ignore_case(IntoStringOnly("name"));
    let _ = text.contains(IntoStringOnly("name"));
    let _ = text.contains_ignore_case(IntoStringOnly("name"));
    let _ = text.starts_with(IntoStringOnly("name"));
    let _ = text.starts_with_ignore_case(IntoStringOnly("name"));
    let _ = text.ends_with(IntoStringOnly("name"));
    let _ = text.ends_with_ignore_case(IntoStringOnly("name"));
    let _ = text.is_in([IntoStringOnly("name")]);
    let _ = text.not_in([IntoStringOnly("name")]);
    let _ = text.regex(IntoStringOnly("^name$"));
    let _ = text.regex_ignore_case(IntoStringOnly("^name$"));

    let _ = BOOL_HANDLE.eq(true);
    let _ = BOOL_HANDLE.is_in([true]);
    let _ = BOOL_HANDLE.not_in([true]);

    macro_rules! assert_integer_authoring {
        ($integer:ty) => {{
            let field = __private::integer_field::<NoTraits, $integer>("task", "count");
            let _ = field.eq(<$integer>::MIN);
            let _ = field.is_in([<$integer>::MIN]);
            let _ = field.not_in([<$integer>::MIN]);
        }};
    }
    assert_integer_authoring!(i8);
    assert_integer_authoring!(i16);
    assert_integer_authoring!(i32);
    assert_integer_authoring!(i64);
    assert_integer_authoring!(i128);
    assert_integer_authoring!(u8);
    assert_integer_authoring!(u16);
    assert_integer_authoring!(u32);
    assert_integer_authoring!(u64);
    assert_integer_authoring!(u128);

    let _ = ENUM_HANDLE.eq(EnumNoTraits(Rc::new(())));
    let _ = ENUM_HANDLE.is_in([EnumNoTraits(Rc::new(()))]);
    let _ = ENUM_HANDLE.not_in([EnumNoTraits(Rc::new(()))]);

    let left = TEXT_HANDLE.eq("left");
    let right = TEXT_HANDLE.eq("right");
    let _ = left.clone().and(right.clone());
    let _ = left.clone().or(right);
    let _ = left.not();
}

#[test]
fn relation_authoring_does_not_require_candidate_or_related_traits() {
    let related = __private::bool_field::<RelatedNoTraits>("related", "flag").eq(true);

    let _ = MANY_HANDLE.any(related.clone());
    let _ = MANY_HANDLE.all(related.clone());
    let _ = MANY_HANDLE.none(related.clone());
    let _ = ONE_HANDLE.is(related);
}

#[derive(Clone, Copy)]
enum Phase {
    Ready,
    Blocked,
}

impl FilterEnum for Phase {
    const FILTER_VARIANTS: &'static [&'static str] = &["ready", "blocked"];

    fn filter_name(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
        }
    }
}

struct IntoStringOnly(&'static str);

impl From<IntoStringOnly> for String {
    fn from(value: IntoStringOnly) -> Self {
        Self::from(value.0)
    }
}

struct ScalarFields {
    raw: TextField<ScalarCandidate>,
    text: TextField<ScalarCandidate>,
    flag: BoolField<ScalarCandidate>,
    i8_value: IntegerField<ScalarCandidate, i8>,
    i16_value: IntegerField<ScalarCandidate, i16>,
    i32_value: IntegerField<ScalarCandidate, i32>,
    i64_value: IntegerField<ScalarCandidate, i64>,
    i128_value: IntegerField<ScalarCandidate, i128>,
    u8_value: IntegerField<ScalarCandidate, u8>,
    u16_value: IntegerField<ScalarCandidate, u16>,
    u32_value: IntegerField<ScalarCandidate, u32>,
    u64_value: IntegerField<ScalarCandidate, u64>,
    u128_value: IntegerField<ScalarCandidate, u128>,
    phase: EnumField<ScalarCandidate, Phase>,
}

struct ScalarCandidate {
    raw: TmuxText,
    text: String,
    flag: bool,
    i8_value: i8,
    i16_value: i16,
    i32_value: i32,
    i64_value: i64,
    i128_value: i128,
    u8_value: u8,
    u16_value: u16,
    u32_value: u32,
    u64_value: u64,
    u128_value: u128,
    phase: Phase,
    visits: RefCell<Vec<&'static str>>,
}

impl ScalarCandidate {
    fn extrema(maximum: bool) -> Self {
        Self {
            raw: TmuxText::from("raw"),
            text: String::from("Straße"),
            flag: true,
            i8_value: if maximum { i8::MAX } else { i8::MIN },
            i16_value: if maximum { i16::MAX } else { i16::MIN },
            i32_value: if maximum { i32::MAX } else { i32::MIN },
            i64_value: if maximum { i64::MAX } else { i64::MIN },
            i128_value: if maximum { i128::MAX } else { i128::MIN },
            u8_value: if maximum { u8::MAX } else { u8::MIN },
            u16_value: if maximum { u16::MAX } else { u16::MIN },
            u32_value: if maximum { u32::MAX } else { u32::MIN },
            u64_value: if maximum { u64::MAX } else { u64::MIN },
            u128_value: if maximum { u128::MAX } else { u128::MIN },
            phase: Phase::Ready,
            visits: RefCell::new(Vec::new()),
        }
    }

    fn observe(&self, expression: &FilterExpr<Self>) -> (bool, Vec<&'static str>) {
        self.visits.borrow_mut().clear();
        let result = expression.matches(self);
        (result, self.visits.borrow().clone())
    }
}

impl Filterable for ScalarCandidate {
    type Fields = ScalarFields;

    const FILTER_TARGET: &'static str = "scalar_candidate";

    fn filter_fields() -> Self::Fields {
        ScalarFields {
            raw: __private::text_field::<Self>(Self::FILTER_TARGET, "raw"),
            text: __private::text_field::<Self>(Self::FILTER_TARGET, "text"),
            flag: __private::bool_field::<Self>(Self::FILTER_TARGET, "flag"),
            i8_value: __private::integer_field::<Self, i8>(Self::FILTER_TARGET, "i8_value"),
            i16_value: __private::integer_field::<Self, i16>(Self::FILTER_TARGET, "i16_value"),
            i32_value: __private::integer_field::<Self, i32>(Self::FILTER_TARGET, "i32_value"),
            i64_value: __private::integer_field::<Self, i64>(Self::FILTER_TARGET, "i64_value"),
            i128_value: __private::integer_field::<Self, i128>(Self::FILTER_TARGET, "i128_value"),
            u8_value: __private::integer_field::<Self, u8>(Self::FILTER_TARGET, "u8_value"),
            u16_value: __private::integer_field::<Self, u16>(Self::FILTER_TARGET, "u16_value"),
            u32_value: __private::integer_field::<Self, u32>(Self::FILTER_TARGET, "u32_value"),
            u64_value: __private::integer_field::<Self, u64>(Self::FILTER_TARGET, "u64_value"),
            u128_value: __private::integer_field::<Self, u128>(Self::FILTER_TARGET, "u128_value"),
            phase: __private::enum_field::<Self, Phase>(Self::FILTER_TARGET, "phase"),
        }
    }

    fn __filter_matches(&self, predicate: &Predicate) -> bool {
        let field = match predicate.field() {
            "raw" => "raw",
            "text" => "text",
            "flag" => "flag",
            "i8_value" => "i8_value",
            "i16_value" => "i16_value",
            "i32_value" => "i32_value",
            "i64_value" => "i64_value",
            "i128_value" => "i128_value",
            "u8_value" => "u8_value",
            "u16_value" => "u16_value",
            "u32_value" => "u32_value",
            "u64_value" => "u64_value",
            "u128_value" => "u128_value",
            "phase" => "phase",
            _ => return false,
        };
        self.visits.borrow_mut().push(field);

        match field {
            "raw" => predicate.matches_text(self.raw.as_bytes()),
            "text" => predicate.matches_text(self.text.as_bytes()),
            "flag" => predicate.matches_bool(self.flag),
            "i8_value" => predicate.matches_signed(i128::from(self.i8_value)),
            "i16_value" => predicate.matches_signed(i128::from(self.i16_value)),
            "i32_value" => predicate.matches_signed(i128::from(self.i32_value)),
            "i64_value" => predicate.matches_signed(i128::from(self.i64_value)),
            "i128_value" => predicate.matches_signed(self.i128_value),
            "u8_value" => predicate.matches_unsigned(u128::from(self.u8_value)),
            "u16_value" => predicate.matches_unsigned(u128::from(self.u16_value)),
            "u32_value" => predicate.matches_unsigned(u128::from(self.u32_value)),
            "u64_value" => predicate.matches_unsigned(u128::from(self.u64_value)),
            "u128_value" => predicate.matches_unsigned(self.u128_value),
            "phase" => predicate.matches_enum(self.phase.filter_name()),
            _ => false,
        }
    }

    fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
        match predicate.field() {
            "raw" | "text" => predicate.validate_text(),
            "flag" => predicate.validate_bool(),
            "i8_value" => predicate.validate_integer(IntegerKind::I8),
            "i16_value" => predicate.validate_integer(IntegerKind::I16),
            "i32_value" => predicate.validate_integer(IntegerKind::I32),
            "i64_value" => predicate.validate_integer(IntegerKind::I64),
            "i128_value" => predicate.validate_integer(IntegerKind::I128),
            "u8_value" => predicate.validate_integer(IntegerKind::U8),
            "u16_value" => predicate.validate_integer(IntegerKind::U16),
            "u32_value" => predicate.validate_integer(IntegerKind::U32),
            "u64_value" => predicate.validate_integer(IntegerKind::U64),
            "u128_value" => predicate.validate_integer(IntegerKind::U128),
            "phase" => predicate.validate_enum(Phase::FILTER_VARIANTS),
            _ => Err(__private::unknown_field_error()),
        }
    }
}

type RelationVisits = Rc<RefCell<Vec<&'static str>>>;
type RelationValidations = Rc<RefCell<Vec<Result<(), FilterExpressionError>>>>;

struct RelationLeafFields {
    label: TextField<RelationLeaf>,
}

struct RelationLeaf {
    label: &'static [u8],
    visit: &'static str,
    visits: RelationVisits,
}

impl Filterable for RelationLeaf {
    type Fields = RelationLeafFields;

    const FILTER_TARGET: &'static str = "relation_leaf";

    fn filter_fields() -> Self::Fields {
        RelationLeafFields {
            label: __private::text_field(Self::FILTER_TARGET, "label"),
        }
    }

    fn __filter_matches(&self, predicate: &Predicate) -> bool {
        if Self::__filter_validate(predicate).is_err() {
            return false;
        }
        match predicate.field() {
            "label" => {
                self.visits.borrow_mut().push(self.visit);
                predicate.matches_text(self.label)
            }
            _ => false,
        }
    }

    fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
        match predicate.field() {
            "label" => predicate.validate_text(),
            _ => Err(__private::unknown_field_error()),
        }
    }
}

struct RelationGroupFields {
    name: TextField<RelationGroup>,
    members: ManyRelation<RelationGroup, RelationLeaf>,
    featured: OneRelation<RelationGroup, RelationLeaf>,
}

struct RelationGroup {
    name: &'static [u8],
    visit: &'static str,
    visits: RelationVisits,
    members: Vec<RelationLeaf>,
    featured: Option<RelationLeaf>,
}

impl Filterable for RelationGroup {
    type Fields = RelationGroupFields;

    const FILTER_TARGET: &'static str = "relation_group";

    fn filter_fields() -> Self::Fields {
        RelationGroupFields {
            name: __private::text_field(Self::FILTER_TARGET, "name"),
            members: __private::many_relation(Self::FILTER_TARGET, "members"),
            featured: __private::one_relation(Self::FILTER_TARGET, "featured"),
        }
    }

    fn __filter_matches(&self, predicate: &Predicate) -> bool {
        if Self::__filter_validate(predicate).is_err() {
            return false;
        }
        match predicate.field() {
            "name" => {
                self.visits.borrow_mut().push(self.visit);
                predicate.matches_text(self.name)
            }
            "members" => predicate.matches_many(&self.members),
            "featured" => predicate.matches_one(self.featured.as_ref()),
            _ => false,
        }
    }

    fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
        match predicate.field() {
            "name" => predicate.validate_text(),
            "members" => predicate.validate_many::<RelationLeaf>(),
            "featured" => predicate.validate_one::<RelationLeaf>(),
            _ => Err(__private::unknown_field_error()),
        }
    }
}

struct RelationRootFields {
    groups: ManyRelation<RelationRoot, RelationGroup>,
    archive: ManyRelation<RelationRoot, RelationGroup>,
    primary: OneRelation<RelationRoot, RelationGroup>,
    secondary: OneRelation<RelationRoot, RelationGroup>,
}

struct RelationRoot {
    groups: Vec<RelationGroup>,
    archive: Vec<RelationGroup>,
    primary: Option<RelationGroup>,
    secondary: Option<RelationGroup>,
    validations: RelationValidations,
}

impl Filterable for RelationRoot {
    type Fields = RelationRootFields;

    const FILTER_TARGET: &'static str = "relation_root";

    fn filter_fields() -> Self::Fields {
        RelationRootFields {
            groups: __private::many_relation(Self::FILTER_TARGET, "groups"),
            archive: __private::many_relation(Self::FILTER_TARGET, "archive"),
            primary: __private::one_relation(Self::FILTER_TARGET, "primary"),
            secondary: __private::one_relation(Self::FILTER_TARGET, "secondary"),
        }
    }

    fn __filter_matches(&self, predicate: &Predicate) -> bool {
        self.validations
            .borrow_mut()
            .push(Self::__filter_validate(predicate));
        match predicate.field() {
            "groups" => predicate.matches_many(&self.groups),
            "archive" => predicate.matches_many(&self.archive),
            "primary" => predicate.matches_one(self.primary.as_ref()),
            "secondary" => predicate.matches_one(self.secondary.as_ref()),
            _ => false,
        }
    }

    fn __filter_validate(predicate: &Predicate) -> Result<(), FilterExpressionError> {
        match predicate.field() {
            "groups" | "archive" => predicate.validate_many::<RelationGroup>(),
            "primary" | "secondary" => predicate.validate_one::<RelationGroup>(),
            _ => Err(__private::unknown_field_error()),
        }
    }
}

fn relation_leaf(
    label: &'static str,
    visit: &'static str,
    visits: &RelationVisits,
) -> RelationLeaf {
    RelationLeaf {
        label: label.as_bytes(),
        visit,
        visits: Rc::clone(visits),
    }
}

fn relation_group(
    name: &'static str,
    visit: &'static str,
    visits: &RelationVisits,
) -> RelationGroup {
    RelationGroup {
        name: name.as_bytes(),
        visit,
        visits: Rc::clone(visits),
        members: vec![relation_leaf(
            "loaded-but-unrelated",
            "unrelated-loaded-child",
            visits,
        )],
        featured: None,
    }
}

fn empty_relation_root() -> RelationRoot {
    RelationRoot {
        groups: Vec::new(),
        archive: Vec::new(),
        primary: None,
        secondary: None,
        validations: Rc::new(RefCell::new(Vec::new())),
    }
}

fn assert_only_relation_ok(root: &RelationRoot) {
    assert_eq!(*root.validations.borrow(), [Ok(())]);
    root.validations.borrow_mut().clear();
}

#[test]
fn relation_quantifiers_define_empty_and_absent_truth_values() {
    let root = empty_relation_root();
    let root_fields = RelationRoot::filter_fields();
    let group_name = RelationGroup::filter_fields().name.eq("never-evaluated");

    assert!(!root_fields.groups.any(group_name.clone()).matches(&root));
    assert_only_relation_ok(&root);
    assert!(root_fields.groups.all(group_name.clone()).matches(&root));
    assert_only_relation_ok(&root);
    assert!(root_fields.groups.none(group_name.clone()).matches(&root));
    assert_only_relation_ok(&root);
    assert!(!root_fields.primary.is(group_name.clone()).matches(&root));
    assert_only_relation_ok(&root);
    assert!(root_fields.primary.is(group_name).not().matches(&root));
    assert_only_relation_ok(&root);
}

#[test]
fn relation_quantifiers_evaluate_matching_and_nonmatching_scalar_children() {
    let visits = Rc::new(RefCell::new(Vec::new()));
    let root = RelationRoot {
        groups: vec![
            relation_group("quantifier-alpha", "alpha", &visits),
            relation_group("quantifier-bravo", "bravo", &visits),
        ],
        archive: Vec::new(),
        primary: Some(relation_group("primary-charlie", "primary", &visits)),
        secondary: None,
        validations: Rc::new(RefCell::new(Vec::new())),
    };
    let root_fields = RelationRoot::filter_fields();
    let group_name = RelationGroup::filter_fields().name;

    assert!(
        root_fields
            .groups
            .any(group_name.eq("quantifier-bravo"))
            .matches(&root)
    );
    assert!(
        !root_fields
            .groups
            .any(group_name.eq("absent-any-sentinel"))
            .matches(&root)
    );
    assert!(
        root_fields
            .groups
            .all(group_name.starts_with("quantifier-"))
            .matches(&root)
    );
    assert!(
        !root_fields
            .groups
            .all(group_name.eq("quantifier-alpha"))
            .matches(&root)
    );
    assert!(
        root_fields
            .groups
            .none(group_name.eq("absent-none-sentinel"))
            .matches(&root)
    );
    assert!(
        !root_fields
            .groups
            .none(group_name.eq("quantifier-bravo"))
            .matches(&root)
    );
    assert!(
        root_fields
            .primary
            .is(group_name.eq("primary-charlie"))
            .matches(&root)
    );
    assert!(
        !root_fields
            .primary
            .is(group_name.eq("absent-is-sentinel"))
            .matches(&root)
    );
}

#[test]
fn many_relation_quantifiers_short_circuit_loaded_children_left_to_right() {
    let visits = Rc::new(RefCell::new(Vec::new()));
    let root_fields = RelationRoot::filter_fields();
    let group_name = RelationGroup::filter_fields().name;

    let any_root = RelationRoot {
        groups: vec![
            relation_group("any-hit", "any-first", &visits),
            relation_group("any-later", "any-must-not-run", &visits),
        ],
        ..empty_relation_root()
    };
    assert!(
        root_fields
            .groups
            .any(group_name.eq("any-hit"))
            .matches(&any_root)
    );
    assert_eq!(*visits.borrow(), ["any-first"]);

    visits.borrow_mut().clear();
    let all_root = RelationRoot {
        groups: vec![
            relation_group("all-allowed", "all-first", &visits),
            relation_group("all-stop", "all-second", &visits),
            relation_group("all-allowed-later", "all-must-not-run", &visits),
        ],
        ..empty_relation_root()
    };
    assert!(
        !root_fields
            .groups
            .all(group_name.starts_with("all-allowed"))
            .matches(&all_root)
    );
    assert_eq!(*visits.borrow(), ["all-first", "all-second"]);

    visits.borrow_mut().clear();
    let none_root = RelationRoot {
        groups: vec![
            relation_group("none-safe", "none-first", &visits),
            relation_group("none-block", "none-second", &visits),
            relation_group("none-later", "none-must-not-run", &visits),
        ],
        ..empty_relation_root()
    };
    assert!(
        !root_fields
            .groups
            .none(group_name.eq("none-block"))
            .matches(&none_root)
    );
    assert_eq!(*visits.borrow(), ["none-first", "none-second"]);
}

#[test]
fn recursive_relation_evaluation_walks_only_the_loaded_fixture_graph() {
    let visits = Rc::new(RefCell::new(Vec::new()));
    let mut first = relation_group("outer-first", "outer-first", &visits);
    first.members = vec![
        relation_leaf("deep-miss-one", "deep-miss-one", &visits),
        relation_leaf("deep-miss-two", "deep-miss-two", &visits),
    ];
    let mut second = relation_group("outer-second", "outer-second", &visits);
    second.members = vec![
        relation_leaf("deep-target", "deep-target", &visits),
        relation_leaf("deep-later", "deep-inner-must-not-run", &visits),
    ];
    let mut third = relation_group("outer-third", "outer-third", &visits);
    third.members = vec![relation_leaf(
        "deep-outer-later",
        "deep-outer-must-not-run",
        &visits,
    )];
    let root = RelationRoot {
        groups: vec![first, second, third],
        ..empty_relation_root()
    };
    let leaf_label = RelationLeaf::filter_fields().label;
    let nested_leaf = leaf_label
        .eq("deep-target")
        .or(leaf_label.eq("deep-alternate"))
        .and(leaf_label.eq("deep-forbidden").not());
    let nested = RelationGroup::filter_fields().members.any(nested_leaf);
    let expression = RelationRoot::filter_fields().groups.any(nested);

    assert!(expression.matches(&root));
    assert_only_relation_ok(&root);
    assert_eq!(
        *visits.borrow(),
        [
            "deep-miss-one",
            "deep-miss-one",
            "deep-miss-two",
            "deep-miss-two",
            "deep-target",
            "deep-target",
        ]
    );

    visits.borrow_mut().clear();
    let nested_nonmatching_leaf = leaf_label
        .eq("deep-absent")
        .or(leaf_label.eq("deep-other-absent"))
        .and(leaf_label.eq("deep-forbidden").not());
    let nested_nonmatch = RelationGroup::filter_fields()
        .members
        .any(nested_nonmatching_leaf);
    let nonmatch = RelationRoot::filter_fields().groups.any(nested_nonmatch);
    assert!(!nonmatch.matches(&root));
    assert_only_relation_ok(&root);
    assert_eq!(
        *visits.borrow(),
        [
            "deep-miss-one",
            "deep-miss-one",
            "deep-miss-two",
            "deep-miss-two",
            "deep-target",
            "deep-target",
            "deep-inner-must-not-run",
            "deep-inner-must-not-run",
            "deep-outer-must-not-run",
            "deep-outer-must-not-run",
        ]
    );
}

#[test]
fn one_relation_recurses_through_relation_and_logical_children() {
    let visits = Rc::new(RefCell::new(Vec::new()));
    let mut primary = relation_group("one-recursive-group", "one-group", &visits);
    primary.members = vec![relation_leaf("one-recursive-target", "one-leaf", &visits)];
    let root = RelationRoot {
        primary: Some(primary),
        ..empty_relation_root()
    };
    let leaf_label = RelationLeaf::filter_fields().label;
    let member = leaf_label
        .eq("one-recursive-target")
        .and(leaf_label.eq("one-recursive-forbidden").not());
    let group = RelationGroup::filter_fields();
    let nested = group
        .members
        .any(member)
        .and(group.name.eq("one-recursive-group"));
    let expression = RelationRoot::filter_fields().primary.is(nested);

    assert!(expression.matches(&root));
    assert_only_relation_ok(&root);
    assert_eq!(*visits.borrow(), ["one-leaf", "one-leaf", "one-group"]);
}

#[test]
fn nested_one_relation_recurses_beneath_a_many_relation() {
    let visits = Rc::new(RefCell::new(Vec::new()));
    let mut group = relation_group("featured-group", "featured-group", &visits);
    group.featured = Some(relation_leaf("featured-target", "featured-leaf", &visits));
    let root = RelationRoot {
        groups: vec![group],
        ..empty_relation_root()
    };
    let leaf_label = RelationLeaf::filter_fields().label;
    let featured_leaf = leaf_label
        .eq("featured-target")
        .and(leaf_label.eq("featured-forbidden").not());
    let nested = RelationGroup::filter_fields().featured.is(featured_leaf);
    let expression = RelationRoot::filter_fields().groups.any(nested);

    assert!(expression.matches(&root));
    assert_only_relation_ok(&root);
    assert_eq!(*visits.borrow(), ["featured-leaf", "featured-leaf"]);

    visits.borrow_mut().clear();
    let absent_literal = RelationGroup::filter_fields()
        .featured
        .is(leaf_label.eq("featured-absent"));
    assert!(
        !RelationRoot::filter_fields()
            .groups
            .any(absent_literal)
            .matches(&root)
    );
    assert_only_relation_ok(&root);
    assert_eq!(*visits.borrow(), ["featured-leaf"]);

    visits.borrow_mut().clear();
    let none_root = RelationRoot {
        groups: vec![relation_group("featured-none", "featured-none", &visits)],
        ..empty_relation_root()
    };
    let absent_relation = RelationGroup::filter_fields()
        .featured
        .is(leaf_label.eq("featured-target"));
    assert!(
        !RelationRoot::filter_fields()
            .groups
            .any(absent_relation)
            .matches(&none_root)
    );
    assert_only_relation_ok(&none_root);
    assert!(visits.borrow().is_empty());
}

fn assert_only_relation_error(root: &RelationRoot, expected: FilterExpressionErrorKind) {
    {
        let validations = root.validations.borrow();
        assert_eq!(validations.len(), 1);
        assert_eq!(
            validations[0].as_ref().map_err(FilterExpressionError::kind),
            Err(expected)
        );
        if let Err(error) = validations[0] {
            assert!(StdError::source(&error).is_none());
        }
    }
    root.validations.borrow_mut().clear();
}

#[test]
fn relation_dispatch_rejects_invalid_family_cardinality_and_nested_schema() {
    let visits = Rc::new(RefCell::new(Vec::new()));
    let validation_leaf = || relation_leaf("loaded-but-unrelated", "validation-leaf", &visits);
    let mut validation_group = relation_group("validation-group", "validation-group", &visits);
    validation_group.featured = Some(validation_leaf());
    let mut validation_primary =
        relation_group("validation-primary", "validation-primary", &visits);
    validation_primary.featured = Some(validation_leaf());
    let root = RelationRoot {
        groups: vec![validation_group],
        primary: Some(validation_primary),
        ..empty_relation_root()
    };
    let group_name = RelationGroup::filter_fields().name;

    let scalar_on_many =
        __private::text_field::<RelationRoot>(RelationRoot::FILTER_TARGET, "groups")
            .eq("scalar-many");
    assert!(!scalar_on_many.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownOperator);

    let scalar_on_one =
        __private::text_field::<RelationRoot>(RelationRoot::FILTER_TARGET, "primary")
            .eq("scalar-one");
    assert!(!scalar_on_one.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownOperator);

    let one_quantifier_on_many = __private::one_relation::<RelationRoot, RelationGroup>(
        RelationRoot::FILTER_TARGET,
        "groups",
    )
    .is(group_name.eq("validation-group"));
    assert!(!one_quantifier_on_many.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownQuantifier);

    let many_quantifier_on_one = __private::many_relation::<RelationRoot, RelationGroup>(
        RelationRoot::FILTER_TARGET,
        "primary",
    )
    .any(group_name.eq("validation-primary"));
    assert!(!many_quantifier_on_one.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownQuantifier);

    let unknown_group_field =
        __private::text_field::<RelationGroup>(RelationGroup::FILTER_TARGET, "unknown-child-field")
            .eq("nested-unknown");
    let nested_unknown_many = RelationRoot::filter_fields()
        .groups
        .any(unknown_group_field.clone());
    assert!(!nested_unknown_many.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownField);

    let nested_unknown_one = RelationRoot::filter_fields()
        .primary
        .is(unknown_group_field);
    assert!(!nested_unknown_one.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownField);

    let unknown_leaf_field = __private::text_field::<RelationLeaf>(
        RelationLeaf::FILTER_TARGET,
        "unknown-grandchild-field",
    )
    .eq("nested-grandchild-unknown");
    let valid_leaf_field = RelationLeaf::filter_fields()
        .label
        .eq("loaded-but-unrelated");
    let invalid_leaf_junction = valid_leaf_field.and(unknown_leaf_field.not());
    let nested_unknown_leaf = RelationGroup::filter_fields()
        .members
        .any(invalid_leaf_junction.clone());
    let recursively_unknown = RelationRoot::filter_fields()
        .groups
        .any(nested_unknown_leaf);
    assert!(!recursively_unknown.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownField);

    let nested_unknown_featured = RelationGroup::filter_fields()
        .featured
        .is(invalid_leaf_junction);
    let recursively_unknown_one = RelationRoot::filter_fields()
        .primary
        .is(nested_unknown_featured);
    assert!(!recursively_unknown_one.matches(&root));
    assert_only_relation_error(&root, FilterExpressionErrorKind::UnknownField);
}

#[test]
fn relation_expression_equality_is_fully_structural_and_ordered() {
    let root = RelationRoot::filter_fields();
    let group = RelationGroup::filter_fields();
    let leaf = RelationLeaf::filter_fields();
    let nested = group.members.any(leaf.label.eq("equality-alpha"));
    let other_nested = group.members.any(leaf.label.eq("equality-bravo"));

    assert_eq!(
        root.groups.any(nested.clone()),
        root.groups.any(nested.clone())
    );
    assert_ne!(
        root.groups.any(nested.clone()),
        __private::many_relation::<RelationRoot, RelationGroup>("other_root", "groups")
            .any(nested.clone())
    );
    assert_ne!(
        root.groups.any(nested.clone()),
        root.archive.any(nested.clone())
    );
    assert_ne!(
        root.groups.any(nested.clone()),
        root.groups.all(nested.clone())
    );
    assert_ne!(
        root.groups.any(nested.clone()),
        root.groups.none(nested.clone())
    );
    assert_ne!(
        root.groups.any(nested.clone()),
        root.groups.any(other_nested)
    );
    assert_ne!(
        root.primary.is(group.name.eq("same-child")),
        root.secondary.is(group.name.eq("same-child"))
    );
    assert_ne!(
        __private::many_relation::<RelationRoot, RelationGroup>(
            RelationRoot::FILTER_TARGET,
            "same-polymorphic-field",
        )
        .any(group.name.eq("same-nested-expression")),
        __private::one_relation::<RelationRoot, RelationGroup>(
            RelationRoot::FILTER_TARGET,
            "same-polymorphic-field",
        )
        .is(group.name.eq("same-nested-expression"))
    );
    let featured = group
        .featured
        .is(leaf.label.eq("featured-equality-sentinel"));
    assert_eq!(featured, featured.clone());

    let left = root.groups.any(nested);
    let right = root.primary.is(group.name.eq("ordered-right"));
    assert_ne!(
        left.clone().and(right.clone()),
        right.clone().and(left.clone())
    );
}

fn assert_relation_debug_case(
    first: &FilterExpr<RelationRoot>,
    second: &FilterExpr<RelationRoot>,
    structure: &[&str],
    secrets: &[&str],
) {
    let first_debug = format!("{first:?}");
    let second_debug = format!("{second:?}");

    assert_eq!(first_debug, second_debug);
    for expected in structure {
        assert!(first_debug.contains(expected), "missing {expected:?}");
    }
    for secret in secrets {
        assert!(!first_debug.contains(secret));
        assert!(!second_debug.contains(secret));
        assert!(!first_debug.contains(&secret.len().to_string()));
        assert!(!second_debug.contains(&secret.len().to_string()));
    }
}

#[test]
fn relation_expression_debug_keeps_schema_structure_but_redacts_literals() {
    let root = RelationRoot::filter_fields();
    let group = RelationGroup::filter_fields();
    let leaf = RelationLeaf::filter_fields();
    let short_secret = "redacted-relation-literal";
    let long_secret = "second-hidden-relation-payload-with-more-bytes";
    let first = root
        .groups
        .any(group.members.none(leaf.label.eq(short_secret)));
    let second = root
        .groups
        .any(group.members.none(leaf.label.eq(long_secret)));
    assert_relation_debug_case(
        &first,
        &second,
        &[
            "Relation",
            "relation_root",
            "groups",
            "any",
            "relation_group",
            "members",
            "none",
            "relation_leaf",
            "label",
            "eq",
        ],
        &[short_secret, long_secret],
    );

    assert_relation_debug_case(
        &root.groups.all(group.name.eq(short_secret)),
        &root.groups.all(group.name.eq(long_secret)),
        &[
            "Relation",
            "relation_root",
            "groups",
            "all",
            "relation_group",
            "name",
            "eq",
        ],
        &[short_secret, long_secret],
    );
    assert_relation_debug_case(
        &root.primary.is(group.name.eq(short_secret)),
        &root.primary.is(group.name.eq(long_secret)),
        &[
            "Relation",
            "relation_root",
            "primary",
            "is",
            "relation_group",
            "name",
            "eq",
        ],
        &[short_secret, long_secret],
    );
}

#[allow(clippy::needless_pass_by_value)]
fn run_relation_matcher<M: Matcher<RelationRoot>>(matcher: M, candidate: &RelationRoot) -> bool {
    matcher.matches(candidate)
}

#[test]
fn relation_expressions_are_owned_and_borrowed_matchers() {
    let visits = Rc::new(RefCell::new(Vec::new()));
    let root = RelationRoot {
        groups: vec![relation_group("matcher-target", "matcher", &visits)],
        ..empty_relation_root()
    };
    let expression = RelationRoot::filter_fields()
        .groups
        .any(RelationGroup::filter_fields().name.eq("matcher-target"));

    assert!(run_relation_matcher(expression.clone(), &root));
    assert!(run_relation_matcher(&expression, &root));
}

#[derive(Clone, Copy)]
struct IsEven;

impl Matcher<i32> for IsEven {
    fn matches(&self, candidate: &i32) -> bool {
        candidate % 2 == 0
    }
}

struct CountingMatcher<'a> {
    evaluations: &'a Cell<usize>,
}

impl Matcher<i32> for CountingMatcher<'_> {
    fn matches(&self, candidate: &i32) -> bool {
        self.evaluations.set(self.evaluations.get() + 1);
        candidate % 2 == 0
    }
}

fn assert_matcher<T, M: Matcher<T>>(_: &M) {}

fn assert_send_sync<T: Send + Sync>(_: &T) {}

fn exactly_one_error_label(error: ExactlyOneError) -> &'static str {
    match error {
        ExactlyOneError::NoItems => "no items",
        ExactlyOneError::MultipleItems => "multiple items",
    }
}

#[test]
fn named_matchers_filter_borrowed_items_in_order() {
    let values = Vec::from([4, 1, 2, 3, 6]);
    assert_matcher::<i32, _>(&IsEven);
    let selected = values.iter().matching(IsEven).copied().collect::<Vec<_>>();

    assert_eq!(selected, [4, 2, 6]);
}

#[test]
fn explicitly_typed_closures_are_matchers() {
    let values = [1, 2, 3, 4];
    let greater_than_two = |candidate: &i32| *candidate > 2;
    assert_matcher::<i32, _>(&greater_than_two);
    let selected = values
        .iter()
        .matching(greater_than_two)
        .copied()
        .collect::<Vec<_>>();

    assert_eq!(selected, [3, 4]);
}

#[test]
fn native_filter_infers_an_untyped_closure() {
    let values = [1, 2, 3, 4];
    let selected = values
        .iter()
        .filter(|candidate| **candidate > 2)
        .copied()
        .collect::<Vec<_>>();

    assert_eq!(selected, [3, 4]);
}

#[test]
fn matching_is_lazy_and_its_adapter_is_send_sync() {
    let values = [1, 3, 4, 6];
    let evaluations = Cell::new(0);
    let matcher = CountingMatcher {
        evaluations: &evaluations,
    };
    let mut selected = values.iter().matching(matcher);

    assert_eq!(evaluations.get(), 0);
    assert_eq!(selected.next(), Some(&4));
    assert_eq!(evaluations.get(), 3);

    let send_sync_adapter = values.iter().matching(IsEven);
    assert_send_sync(&send_sync_adapter);
}

#[test]
#[allow(clippy::expect_used)]
fn exactly_one_returns_the_only_borrowed_item() {
    let values = [String::from("only")];
    let expected = values.first().expect("fixture contains one item");
    let item = values
        .iter()
        .exactly_one()
        .expect("iterator contains one item");

    assert!(std::ptr::eq(item, expected));
}

#[test]
fn exactly_one_distinguishes_empty_and_multiple_inputs() {
    let empty: Vec<i32> = Vec::new();
    let multiple = [1, 2];
    assert_eq!(empty.iter().exactly_one(), Err(ExactlyOneError::NoItems));
    assert_eq!(
        multiple.iter().exactly_one(),
        Err(ExactlyOneError::MultipleItems)
    );
}

#[test]
fn one_or_none_covers_all_cardinalities() {
    let empty: Vec<i32> = Vec::new();
    let one = [7];
    let multiple = [7, 8];

    assert_eq!(empty.iter().one_or_none(), Ok(None));
    assert_eq!(one.iter().one_or_none(), Ok(Some(&one[0])));
    assert_eq!(multiple.iter().one_or_none(), Err(MultipleItemsError));
}

#[test]
fn cardinality_methods_pull_at_most_two_items() {
    let values = [7, 8, 9];
    let exactly_one_pulls = Cell::new(0);
    let one_or_none_pulls = Cell::new(0);

    let exactly_one = values
        .iter()
        .inspect(|_| exactly_one_pulls.set(exactly_one_pulls.get() + 1))
        .exactly_one();
    let one_or_none = values
        .iter()
        .inspect(|_| one_or_none_pulls.set(one_or_none_pulls.get() + 1))
        .one_or_none();

    assert_eq!(exactly_one, Err(ExactlyOneError::MultipleItems));
    assert_eq!(one_or_none, Err(MultipleItemsError));
    assert_eq!(exactly_one_pulls.get(), 2);
    assert_eq!(one_or_none_pulls.get(), 2);
}

#[test]
fn cardinality_error_display_is_value_free() {
    let values = ["secret-alpha", "secret-beta"];
    assert_eq!(
        values.iter().exactly_one(),
        Err(ExactlyOneError::MultipleItems)
    );
    assert_eq!(values.iter().one_or_none(), Err(MultipleItemsError));
    let exactly_one = ExactlyOneError::MultipleItems.to_string();
    let one_or_none = MultipleItemsError.to_string();

    assert!(!exactly_one.is_empty());
    assert!(!one_or_none.is_empty());
    for value in values {
        assert!(!exactly_one.contains(value));
        assert!(!one_or_none.contains(value));
    }
}

#[test]
fn cardinality_errors_are_exhaustive_and_source_less() {
    assert_eq!(
        exactly_one_error_label(ExactlyOneError::NoItems),
        "no items"
    );
    assert_eq!(
        exactly_one_error_label(ExactlyOneError::MultipleItems),
        "multiple items"
    );
    assert!(StdError::source(&ExactlyOneError::NoItems).is_none());
    assert!(StdError::source(&ExactlyOneError::MultipleItems).is_none());
    assert!(StdError::source(&MultipleItemsError).is_none());
}

macro_rules! assert_scalar_ops {
    ($field:expr, $candidate:expr, $value:expr, $other:expr) => {{
        assert!($field.eq($value).matches($candidate));
        assert!(!$field.eq($other).matches($candidate));
        assert!($field.is_in([$other, $value]).matches($candidate));
        assert!(!$field.is_in([$other]).matches($candidate));
        assert!(!$field.not_in([$other, $value]).matches($candidate));
        assert!($field.not_in([$other]).matches($candidate));
        assert!(!$field.is_in(std::iter::empty()).matches($candidate));
        assert!($field.not_in(std::iter::empty()).matches($candidate));
    }};
}

macro_rules! assert_integer_width {
    ($fields:expr, $minimums:expr, $maximums:expr, $field:ident, $ty:ty) => {
        assert_scalar_ops!($fields.$field, $minimums, <$ty>::MIN, <$ty>::MAX);
        assert_scalar_ops!($fields.$field, $maximums, <$ty>::MAX, <$ty>::MIN);
    };
}

#[test]
fn discrete_scalar_operators_cover_empty_sets_and_integer_extrema() {
    let fields = ScalarCandidate::filter_fields();
    let minimums = ScalarCandidate::extrema(false);
    let maximums = ScalarCandidate::extrema(true);

    assert_scalar_ops!(fields.flag, &minimums, true, false);
    assert_scalar_ops!(fields.phase, &minimums, Phase::Ready, Phase::Blocked);
    assert_integer_width!(fields, &minimums, &maximums, i8_value, i8);
    assert_integer_width!(fields, &minimums, &maximums, i16_value, i16);
    assert_integer_width!(fields, &minimums, &maximums, i32_value, i32);
    assert_integer_width!(fields, &minimums, &maximums, i64_value, i64);
    assert_integer_width!(fields, &minimums, &maximums, i128_value, i128);
    assert_integer_width!(fields, &minimums, &maximums, u8_value, u8);
    assert_integer_width!(fields, &minimums, &maximums, u16_value, u16);
    assert_integer_width!(fields, &minimums, &maximums, u32_value, u32);
    assert_integer_width!(fields, &minimums, &maximums, u64_value, u64);
    assert_integer_width!(fields, &minimums, &maximums, u128_value, u128);
}

#[test]
#[allow(clippy::expect_used)]
fn text_operators_keep_scalar_and_regex_case_semantics_distinct() {
    let fields = ScalarCandidate::filter_fields();
    let candidate = ScalarCandidate::extrema(false);

    assert!(fields.raw.eq("raw").matches(&candidate));
    assert!(fields.text.eq("Straße").matches(&candidate));
    assert!(fields.text.contains("tra").matches(&candidate));
    assert!(fields.text.starts_with("Str").matches(&candidate));
    assert!(fields.text.ends_with("ße").matches(&candidate));
    assert!(!fields.text.eq("STRASSE").matches(&candidate));
    assert!(!fields.text.contains("TRA").matches(&candidate));
    assert!(!fields.text.starts_with("STR").matches(&candidate));
    assert!(!fields.text.ends_with("SSE").matches(&candidate));
    assert!(fields.text.eq_ignore_case("STRASSE").matches(&candidate));
    assert!(
        fields
            .text
            .contains_ignore_case("TRASS")
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .starts_with_ignore_case("STR")
            .matches(&candidate)
    );
    assert!(fields.text.ends_with_ignore_case("SSE").matches(&candidate));
    assert!(!fields.text.eq_ignore_case("missing").matches(&candidate));
    assert!(
        !fields
            .text
            .contains_ignore_case("missing")
            .matches(&candidate)
    );
    assert!(
        !fields
            .text
            .starts_with_ignore_case("missing")
            .matches(&candidate)
    );
    assert!(
        !fields
            .text
            .ends_with_ignore_case("missing")
            .matches(&candidate)
    );
    assert!(fields.text.is_in(["other", "Straße"]).matches(&candidate));
    assert!(!fields.text.is_in(["other"]).matches(&candidate));
    assert!(!fields.text.not_in(["other", "Straße"]).matches(&candidate));
    assert!(fields.text.not_in(["other"]).matches(&candidate));
    assert!(!fields.text.is_in(Vec::<String>::new()).matches(&candidate));
    assert!(fields.text.not_in(Vec::<String>::new()).matches(&candidate));
    assert!(
        fields
            .text
            .regex("^Straße$")
            .expect("pattern is valid")
            .matches(&candidate)
    );
    assert!(
        !fields
            .text
            .regex_ignore_case("^STRASSE$")
            .expect("pattern is valid")
            .matches(&candidate)
    );

    let mut composed = ScalarCandidate::extrema(false);
    composed.text = String::from("\u{e9}");
    assert!(!fields.text.eq_ignore_case("e\u{301}").matches(&composed));
}

#[test]
#[allow(clippy::expect_used)]
fn every_text_operator_accepts_generic_string_inputs() {
    let fields = ScalarCandidate::filter_fields();
    let candidate = ScalarCandidate::extrema(false);

    assert!(fields.text.eq(IntoStringOnly("Straße")).matches(&candidate));
    assert!(
        fields
            .text
            .eq_ignore_case(IntoStringOnly("STRASSE"))
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .contains(IntoStringOnly("tra"))
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .contains_ignore_case(IntoStringOnly("TRASS"))
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .starts_with(IntoStringOnly("Str"))
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .starts_with_ignore_case(IntoStringOnly("STR"))
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .ends_with(IntoStringOnly("ße"))
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .ends_with_ignore_case(IntoStringOnly("SSE"))
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .is_in([IntoStringOnly("other"), IntoStringOnly("Straße")])
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .not_in([IntoStringOnly("other")])
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .regex(IntoStringOnly("^Straße$"))
            .expect("pattern is valid")
            .matches(&candidate)
    );
    assert!(
        fields
            .text
            .regex_ignore_case(IntoStringOnly("^stra(?:ße)$"))
            .expect("pattern is valid")
            .matches(&candidate)
    );
}

#[test]
#[allow(clippy::expect_used)]
fn invalid_utf8_is_not_decoded_lossily() {
    let fields = ScalarCandidate::filter_fields();
    let mut candidate = ScalarCandidate::extrema(false);
    candidate.raw = TmuxText::from_bytes([b'x', 0xff]);

    assert!(!fields.raw.eq("x\u{fffd}").matches(&candidate));
    assert!(!fields.raw.eq_ignore_case("").matches(&candidate));
    assert!(!fields.raw.contains("").matches(&candidate));
    assert!(!fields.raw.contains_ignore_case("").matches(&candidate));
    assert!(!fields.raw.starts_with("").matches(&candidate));
    assert!(!fields.raw.starts_with_ignore_case("").matches(&candidate));
    assert!(!fields.raw.ends_with("").matches(&candidate));
    assert!(!fields.raw.ends_with_ignore_case("").matches(&candidate));
    assert!(!fields.raw.is_in(["x\u{fffd}"]).matches(&candidate));
    assert!(!fields.raw.not_in(Vec::<String>::new()).matches(&candidate));
    assert!(
        !fields
            .raw
            .regex(".*")
            .expect("pattern is valid")
            .matches(&candidate)
    );
    assert!(
        !fields
            .raw
            .regex_ignore_case(".*")
            .expect("pattern is valid")
            .matches(&candidate)
    );
}

#[test]
#[allow(clippy::expect_used)]
fn junctions_preserve_order_short_circuit_and_flattening() {
    let fields = ScalarCandidate::filter_fields();
    let candidate = ScalarCandidate::extrema(false);
    let yes = fields.flag.eq(true);
    let no = fields.i8_value.eq(i8::MAX);
    let later = fields.phase.eq(Phase::Ready);

    assert_eq!(
        candidate.observe(&no.clone().and(later.clone())),
        (false, Vec::from(["i8_value"]))
    );
    assert_eq!(
        candidate.observe(&yes.clone().or(later.clone())),
        (true, Vec::from(["flag"]))
    );
    assert_eq!(
        candidate.observe(&no.clone().or(later.clone())),
        (true, Vec::from(["i8_value", "phase"]))
    );
    assert_eq!(
        candidate.observe(&yes.clone().and(later.clone())),
        (true, Vec::from(["flag", "phase"]))
    );
    assert_eq!(
        candidate.observe(&yes.clone().not()),
        (false, Vec::from(["flag"]))
    );
    assert_eq!(
        yes.clone().and(no.clone()).and(later.clone()),
        yes.clone().and(no.clone().and(later.clone()))
    );
    assert_eq!(
        yes.clone().or(no.clone()).or(later.clone()),
        yes.clone().or(no.clone().or(later.clone()))
    );
    assert_ne!(yes.clone().and(no.clone()), no.clone().and(yes));

    let regex = fields.text.regex("^Straße$").expect("pattern is valid");
    let same_regex = fields.text.regex("^Straße$").expect("pattern is valid");
    let other_regex = fields.text.regex("^other$").expect("pattern is valid");
    let insensitive_regex = fields
        .text
        .regex_ignore_case("^Straße$")
        .expect("pattern is valid");
    assert_eq!(regex, same_regex);
    assert_ne!(regex, other_regex);
    assert_ne!(regex, insensitive_regex);
}

#[test]
fn scalar_expression_equality_is_structural() {
    let fields = ScalarCandidate::filter_fields();
    let a = fields.text.eq("alpha");
    let b = fields.flag.eq(true);
    let c = fields.i64_value.eq(7);

    assert_eq!(a, fields.text.eq("alpha"));
    assert_ne!(a, fields.text.eq("bravo"));
    assert_eq!(b, fields.flag.eq(true));
    assert_ne!(b, fields.flag.eq(false));
    assert_eq!(c, fields.i64_value.eq(7));
    assert_ne!(c, fields.i64_value.eq(8));
    assert_eq!(fields.phase.eq(Phase::Ready), fields.phase.eq(Phase::Ready));
    assert_ne!(
        fields.phase.eq(Phase::Ready),
        fields.phase.eq(Phase::Blocked)
    );
    assert_eq!(
        fields.text.is_in(["alpha", "bravo"]),
        fields.text.is_in(["alpha", "bravo"])
    );
    assert_ne!(
        fields.text.is_in(["alpha", "bravo"]),
        fields.text.is_in(["bravo", "alpha"])
    );
    assert_ne!(a.clone().and(b.clone().or(c.clone())), a.and(b).or(c));
}

#[allow(clippy::needless_pass_by_value)]
fn run_scalar_matcher<M: Matcher<ScalarCandidate>>(
    matcher: M,
    candidate: &ScalarCandidate,
) -> bool {
    matcher.matches(candidate)
}

#[test]
fn filter_expressions_are_owned_and_borrowed_matchers() {
    let candidate = ScalarCandidate::extrema(false);
    let expression = ScalarCandidate::filter_fields().flag.eq(true);

    assert!(run_scalar_matcher(expression.clone(), &candidate));
    assert!(run_scalar_matcher(&expression, &candidate));
}

fn assert_handle_identity<H>(
    handle: H,
    same: H,
    other_target: H,
    other_field: H,
    type_sentinels: &[&str],
) where
    H: Copy + Debug + Eq,
{
    assert_eq!(handle, same);
    assert_ne!(handle, other_target);
    assert_ne!(handle, other_field);

    let debug = format!("{handle:?}");
    let same_debug = format!("{same:?}");
    let other_target_debug = format!("{other_target:?}");
    let other_field_debug = format!("{other_field:?}");
    assert_eq!(debug, same_debug);
    assert_ne!(debug, other_target_debug);
    assert_ne!(debug, other_field_debug);
    assert!(debug.contains("task"));
    assert!(debug.contains("expected_field"));
    assert!(other_target_debug.contains("other"));
    assert!(other_target_debug.contains("expected_field"));
    assert!(other_field_debug.contains("task"));
    assert!(other_field_debug.contains("other_field"));
    for sentinel in type_sentinels {
        assert!(!debug.contains(sentinel));
        assert!(!other_target_debug.contains(sentinel));
        assert!(!other_field_debug.contains(sentinel));
    }
}

fn assert_expression_debug_redacted(
    left: &FilterExpr<ScalarCandidate>,
    right: &FilterExpr<ScalarCandidate>,
    secrets: &[&str],
) {
    let left_debug = format!("{left:?}");
    let right_debug = format!("{right:?}");
    assert_eq!(left_debug, right_debug);
    for secret in secrets {
        assert!(!left_debug.contains(secret));
        assert!(!right_debug.contains(secret));
        assert!(!left_debug.contains(&secret.len().to_string()));
        assert!(!right_debug.contains(&secret.len().to_string()));
    }
}

#[test]
fn hidden_constructors_preserve_stable_target_and_field_identity() {
    assert_handle_identity(
        __private::text_field::<NoTraits>("task", "expected_field"),
        __private::text_field::<NoTraits>("task", "expected_field"),
        __private::text_field::<NoTraits>("other", "expected_field"),
        __private::text_field::<NoTraits>("task", "other_field"),
        &["NoTraits"],
    );
    assert_handle_identity(
        __private::bool_field::<NoTraits>("task", "expected_field"),
        __private::bool_field::<NoTraits>("task", "expected_field"),
        __private::bool_field::<NoTraits>("other", "expected_field"),
        __private::bool_field::<NoTraits>("task", "other_field"),
        &["NoTraits"],
    );
    assert_handle_identity(
        __private::integer_field::<NoTraits, i64>("task", "expected_field"),
        __private::integer_field::<NoTraits, i64>("task", "expected_field"),
        __private::integer_field::<NoTraits, i64>("other", "expected_field"),
        __private::integer_field::<NoTraits, i64>("task", "other_field"),
        &["NoTraits"],
    );
    assert_handle_identity(
        __private::enum_field::<NoTraits, EnumNoTraits>("task", "expected_field"),
        __private::enum_field::<NoTraits, EnumNoTraits>("task", "expected_field"),
        __private::enum_field::<NoTraits, EnumNoTraits>("other", "expected_field"),
        __private::enum_field::<NoTraits, EnumNoTraits>("task", "other_field"),
        &["NoTraits", "EnumNoTraits"],
    );
    assert_handle_identity(
        __private::many_relation::<NoTraits, RelatedNoTraits>("task", "expected_field"),
        __private::many_relation::<NoTraits, RelatedNoTraits>("task", "expected_field"),
        __private::many_relation::<NoTraits, RelatedNoTraits>("other", "expected_field"),
        __private::many_relation::<NoTraits, RelatedNoTraits>("task", "other_field"),
        &["NoTraits", "RelatedNoTraits"],
    );
    assert_handle_identity(
        __private::one_relation::<NoTraits, RelatedNoTraits>("task", "expected_field"),
        __private::one_relation::<NoTraits, RelatedNoTraits>("task", "expected_field"),
        __private::one_relation::<NoTraits, RelatedNoTraits>("other", "expected_field"),
        __private::one_relation::<NoTraits, RelatedNoTraits>("task", "other_field"),
        &["NoTraits", "RelatedNoTraits"],
    );

    let _ = (
        TEXT_HANDLE,
        BOOL_HANDLE,
        INTEGER_HANDLE,
        ENUM_HANDLE,
        MANY_HANDLE,
        ONE_HANDLE,
    );
}

#[test]
fn scalar_expression_debug_omits_literals_and_lengths() {
    let fields = ScalarCandidate::filter_fields();
    assert_expression_debug_redacted(
        &fields.flag.eq(true),
        &fields.flag.eq(false),
        &["true", "false"],
    );
    assert_expression_debug_redacted(
        &fields.i64_value.eq(7),
        &fields.i64_value.eq(-123_456_789),
        &["123456789"],
    );
    assert_expression_debug_redacted(
        &fields.u128_value.eq(7),
        &fields.u128_value.eq(123_456_789),
        &["123456789"],
    );
    assert_expression_debug_redacted(
        &fields.phase.eq(Phase::Ready),
        &fields.phase.eq(Phase::Blocked),
        &["ready", "blocked"],
    );
    assert_expression_debug_redacted(
        &fields.flag.is_in([true]),
        &fields.flag.is_in([false, true]),
        &["true", "false"],
    );
    assert_expression_debug_redacted(
        &fields.flag.not_in([true]),
        &fields.flag.not_in([false, true]),
        &["true", "false"],
    );
    assert_expression_debug_redacted(
        &fields.i64_value.is_in([7]),
        &fields.i64_value.is_in([-123_456_789, 8]),
        &["123456789"],
    );
    assert_expression_debug_redacted(
        &fields.i64_value.not_in([7]),
        &fields.i64_value.not_in([-123_456_789, 8]),
        &["123456789"],
    );
    assert_expression_debug_redacted(
        &fields.u128_value.is_in([7]),
        &fields.u128_value.is_in([123_456_789, 8]),
        &["123456789"],
    );
    assert_expression_debug_redacted(
        &fields.u128_value.not_in([7]),
        &fields.u128_value.not_in([123_456_789, 8]),
        &["123456789"],
    );
    assert_expression_debug_redacted(
        &fields.phase.is_in([Phase::Ready]),
        &fields.phase.is_in([Phase::Blocked, Phase::Ready]),
        &["ready", "blocked"],
    );
    assert_expression_debug_redacted(
        &fields.phase.not_in([Phase::Ready]),
        &fields.phase.not_in([Phase::Blocked, Phase::Ready]),
        &["ready", "blocked"],
    );
}

#[test]
#[allow(clippy::expect_used)]
fn text_expression_debug_omits_literals_and_lengths() {
    let fields = ScalarCandidate::filter_fields();
    assert_expression_debug_redacted(
        &fields.text.eq("alpha-secret"),
        &fields.text.eq("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.eq_ignore_case("alpha-secret"),
        &fields
            .text
            .eq_ignore_case("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.contains("alpha-secret"),
        &fields.text.contains("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.contains_ignore_case("alpha-secret"),
        &fields
            .text
            .contains_ignore_case("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.starts_with("alpha-secret"),
        &fields
            .text
            .starts_with("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.starts_with_ignore_case("alpha-secret"),
        &fields
            .text
            .starts_with_ignore_case("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.ends_with("alpha-secret"),
        &fields.text.ends_with("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.ends_with_ignore_case("alpha-secret"),
        &fields
            .text
            .ends_with_ignore_case("bravo-value-with-a-distinct-length"),
        &["alpha-secret", "bravo-value-with-a-distinct-length"],
    );
    assert_expression_debug_redacted(
        &fields.text.is_in(["one"]),
        &fields.text.is_in(["two", "three", "four"]),
        &["one", "two", "three", "four"],
    );
    assert_expression_debug_redacted(
        &fields.text.not_in(["one"]),
        &fields.text.not_in(["two", "three", "four"]),
        &["one", "two", "three", "four"],
    );
    assert_expression_debug_redacted(
        &fields.text.regex("^short$").expect("pattern is valid"),
        &fields
            .text
            .regex("^a-much-longer-secret-pattern$")
            .expect("pattern is valid"),
        &["short", "a-much-longer-secret-pattern"],
    );
    assert_expression_debug_redacted(
        &fields
            .text
            .regex_ignore_case("^short$")
            .expect("pattern is valid"),
        &fields
            .text
            .regex_ignore_case("^a-much-longer-secret-pattern$")
            .expect("pattern is valid"),
        &["short", "a-much-longer-secret-pattern"],
    );
}

#[test]
fn junction_debug_omits_nested_literals_and_lengths() {
    let fields = ScalarCandidate::filter_fields();
    assert_expression_debug_redacted(
        &fields.text.eq("nested-short").and(fields.flag.eq(true)),
        &fields
            .text
            .eq("nested-secret-with-a-distinct-length")
            .and(fields.flag.eq(false)),
        &[
            "nested-short",
            "nested-secret-with-a-distinct-length",
            "true",
            "false",
        ],
    );
    assert_expression_debug_redacted(
        &fields.text.eq("nested-short").or(fields.flag.eq(true)),
        &fields
            .text
            .eq("nested-secret-with-a-distinct-length")
            .or(fields.flag.eq(false)),
        &[
            "nested-short",
            "nested-secret-with-a-distinct-length",
            "true",
            "false",
        ],
    );
    assert_expression_debug_redacted(
        &fields.text.eq("nested-short").not(),
        &fields.text.eq("nested-secret-with-a-distinct-length").not(),
        &["nested-short", "nested-secret-with-a-distinct-length"],
    );
}

#[test]
#[allow(clippy::expect_used)]
fn validation_errors_omit_literals_and_lengths() {
    let fields = ScalarCandidate::filter_fields();
    let short_error = fields.text.regex("[").expect_err("pattern is invalid");
    let long_error = fields
        .text
        .regex("(?P<bravo-secret>")
        .expect_err("pattern is invalid");
    assert_eq!(short_error.kind(), FilterExpressionErrorKind::InvalidRegex);
    assert_eq!(long_error.kind(), FilterExpressionErrorKind::InvalidRegex);
    assert_eq!(format!("{short_error:?}"), format!("{long_error:?}"));
    assert_eq!(short_error.to_string(), long_error.to_string());
    assert!(StdError::source(&short_error).is_none());
    assert!(!format!("{long_error:?}").contains("bravo-secret"));
    assert!(!long_error.to_string().contains("bravo-secret"));

    assert_eq!(UNKNOWN_FIELD_KIND, FilterExpressionErrorKind::UnknownField);
    assert!(StdError::source(&UNKNOWN_FIELD_ERROR).is_none());
    assert!(!format!("{UNKNOWN_FIELD_ERROR:?}").is_empty());
    assert!(!UNKNOWN_FIELD_ERROR.to_string().is_empty());
}

#[test]
fn rust_regex_rejects_python_lookaround_and_backreferences() -> Result<(), &'static str> {
    let field = ScalarCandidate::filter_fields().text;

    for pattern in ["a(?=b)", r"(a)\1"] {
        for result in [field.regex(pattern), field.regex_ignore_case(pattern)] {
            let error = result.err().ok_or("Python-only pattern compiled")?;
            assert_eq!(error.kind(), FilterExpressionErrorKind::InvalidRegex);
            assert!(StdError::source(&error).is_none());
            assert!(!error.to_string().contains(pattern));
            assert!(!format!("{error:?}").contains(pattern));
        }
    }

    Ok(())
}
