//! Public generated-behavior tests for the optional derive feature.

#![cfg(feature = "query")]
#![allow(dead_code)]

use libtmux::Filterable;
use libtmux::query::{FilterEnum, Filterable as _, QueryIteratorExt};

#[derive(Clone, Copy)]
enum WorkflowState {
    Ready,
    Blocked,
}

impl FilterEnum for WorkflowState {
    const FILTER_VARIANTS: &'static [&'static str] = &["ready", "blocked"];

    fn filter_name(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Filterable)]
#[filterable(target = "child", crate = "libtmux")]
struct Child {
    label: String,
    active: bool,
}

#[derive(Filterable)]
#[filterable(target = "collision_target", crate = "libtmux")]
struct InherentTargetCollision {
    label: String,
}

impl InherentTargetCollision {
    const FILTER_TARGET: &'static str = "inherent_target";
}

#[derive(Filterable)]
#[filterable(target = "work_item", fields = "TaskHandles", crate = "libtmux")]
struct Task {
    #[filterable(rename = "name")]
    summary: String,
    done: bool,
    score: Option<i16>,
    #[filterable(enum)]
    state: WorkflowState,
    #[filterable(skip)]
    private_note: String,
    #[filterable(many)]
    children: Vec<Child>,
    #[filterable(one)]
    owner: Option<Child>,
}

#[derive(Filterable)]
#[filterable(target = "duplicate", crate = "libtmux")]
struct FirstDuplicate {
    first: bool,
}

#[derive(Filterable)]
#[filterable(target = "duplicate", crate = "libtmux")]
struct SecondDuplicate {
    second: bool,
}

#[derive(Filterable)]
#[filterable(target = "duplicate_root", crate = "libtmux")]
struct DuplicateRoot {
    #[filterable(many)]
    first: Vec<FirstDuplicate>,
    #[filterable(many)]
    second: Vec<SecondDuplicate>,
}

fn assert_value_traits<T: Clone + Copy + core::fmt::Debug + Eq + Send + Sync>() {}

fn fixture() -> Vec<Task> {
    vec![
        Task {
            summary: String::from("build"),
            done: false,
            score: Some(7),
            state: WorkflowState::Ready,
            private_note: String::from("first-secret"),
            children: vec![Child {
                label: String::from("compile"),
                active: true,
            }],
            owner: None,
        },
        Task {
            summary: String::from("test"),
            done: true,
            score: None,
            state: WorkflowState::Blocked,
            private_note: String::from("second-secret"),
            children: Vec::new(),
            owner: Some(Child {
                label: String::from("review"),
                active: true,
            }),
        },
        Task {
            summary: String::from("deploy"),
            done: false,
            score: None,
            state: WorkflowState::Ready,
            private_note: String::from("third-secret"),
            children: vec![Child {
                label: String::from("ship"),
                active: false,
            }],
            owner: Some(Child {
                label: String::from("release"),
                active: false,
            }),
        },
    ]
}

#[test]
fn root_derive_filters_downstream_vectors_in_order() {
    let values = fixture();
    let fields = Task::filter_fields();
    let child_fields = Child::filter_fields();
    let expression = fields
        .summary
        .contains("u")
        .or(fields.owner.is(child_fields.active.eq(true)));

    let selected = values
        .iter()
        .matching(&expression)
        .map(|task| task.summary.as_str())
        .collect::<Vec<_>>();

    assert_eq!(selected, ["build", "test"]);
}

#[test]
fn generated_dispatch_handles_optional_scalars_enums_and_relations() {
    let values = fixture();
    let fields = Task::filter_fields();
    let child_fields = Child::filter_fields();

    assert!(fields.score.eq(7).matches(&values[0]));
    assert!(!fields.score.eq(7).matches(&values[1]));
    assert!(fields.score.not_in([]).matches(&values[0]));
    assert!(!fields.score.not_in([]).matches(&values[1]));
    assert!(fields.score.not_in([]).not().matches(&values[1]));

    assert!(fields.state.eq(WorkflowState::Ready).matches(&values[0]));
    assert!(!fields.state.eq(WorkflowState::Ready).matches(&values[1]));

    assert!(
        !fields
            .children
            .any(child_fields.active.eq(true))
            .matches(&values[1])
    );
    assert!(
        fields
            .children
            .all(child_fields.active.eq(true))
            .matches(&values[1])
    );
    assert!(
        fields
            .children
            .none(child_fields.active.eq(true))
            .matches(&values[1])
    );
    assert!(
        !fields
            .owner
            .is(child_fields.active.eq(true))
            .matches(&values[0])
    );
    assert!(
        fields
            .owner
            .is(child_fields.active.eq(true))
            .matches(&values[1])
    );
}

#[test]
fn generated_companion_has_stable_names_and_value_traits() {
    assert_value_traits::<TaskHandles>();
    assert_eq!(
        <Task as libtmux::query::Filterable>::FILTER_TARGET,
        "work_item"
    );

    let fields = Task::filter_fields();
    let copied = fields;
    assert_eq!(fields, copied);

    let debug = format!("{fields:?}");
    assert!(debug.contains("TaskHandles"));
    assert!(debug.contains("work_item"));
    assert!(debug.contains("name"));
    assert!(!debug.contains("private_note"));
    assert!(!debug.contains("first-secret"));
}

#[test]
fn generated_handles_use_trait_target_when_inherent_constant_collides() {
    let fields = InherentTargetCollision::filter_fields();
    let expected = libtmux::query::__private::text_field::<InherentTargetCollision>(
        <InherentTargetCollision as libtmux::query::Filterable>::FILTER_TARGET,
        "label",
    );

    assert_eq!(fields.label, expected);
}

/// An empty to-many relation satisfies `all` and satisfies `any` for nothing.
///
/// Vacuous truth is the rule every quantifier language settles on, and it is
/// the one a caller is most likely to be surprised by, so it is pinned here
/// rather than left to the reader.
#[test]
fn an_empty_relation_is_vacuously_true_for_all_and_false_for_any() {
    let childless = Task {
        summary: "alone".into(),
        done: false,
        score: None,
        state: WorkflowState::Blocked,
        private_note: String::new(),
        children: Vec::new(),
        owner: None,
    };

    let fields = Task::filter_fields();
    let impossible = Child::filter_fields().label.eq("nothing matches this");

    assert!(
        fields.children.all(impossible.clone()).matches(&childless),
        "all holds over no children",
    );
    assert!(
        !fields.children.any(impossible.clone()).matches(&childless),
        "any holds over none of them",
    );
    assert!(
        fields.children.none(impossible).matches(&childless),
        "and none holds too",
    );
}

#[cfg(feature = "schema")]
#[test]
fn generated_schema_uses_the_same_fields_types_and_relations() {
    use libtmux::query::FilterExpr;
    use serde_json::json;

    let schema =
        serde_json::to_value(schemars::schema_for!(FilterExpr<Task>)).expect("schema serializes");
    jsonschema::draft202012::meta::validate(&schema).expect("schema is valid Draft 2020-12");
    let validator = jsonschema::draft202012::new(&schema).expect("schema compiles");

    assert!(validator.is_valid(&json!({
        "version": 1,
        "target": "work_item",
        "expr": {
            "op": "relation",
            "field": "children",
            "quantifier": "any",
            "expr": {"op": "eq", "field": "active", "value": true},
        },
    })));
    for signed in ["0", "-5", "5"] {
        assert!(
            validator.is_valid(&json!({"version": 1, "target": "work_item", "expr":
                {"op": "eq", "field": "score", "value": signed}})),
            "schema rejected {signed}"
        );
    }
    for malformed in [
        json!({"version": 1, "target": "work_item", "expr":
            {"op": "contains", "field": "done", "value": "yes"}}),
        json!({"version": 1, "target": "work_item", "expr":
            {"op": "eq", "field": "private_note", "value": "secret"}}),
        json!({"version": 1, "target": "work_item", "expr": {
            "op": "relation", "field": "owner", "quantifier": "any",
            "expr": {"op": "eq", "field": "active", "value": true},
        }}),
        json!({"version": 1, "target": "work_item", "expr":
            {"op": "eq", "field": "score", "value": "-0"}}),
    ] {
        assert!(
            !validator.is_valid(&malformed),
            "schema accepted {malformed}"
        );
    }
}

#[cfg(feature = "schema")]
#[test]
#[should_panic(expected = "filter target `duplicate` has conflicting schemas")]
fn generated_schema_rejects_one_target_name_with_two_grammars() {
    use libtmux::query::FilterExpr;

    let _ = schemars::schema_for!(FilterExpr<DuplicateRoot>);
}
