#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::Filterable as _;

#[derive(Filterable)]
#[filterable(target = "child")]
struct Child {
    done: bool,
}

#[derive(Filterable)]
#[filterable(target = "parent")]
struct Parent {
    #[filterable(rename = "members", many)]
    children: Vec<Child>,
    #[filterable(rename = "primary")]
    #[filterable(one)]
    favorite: Option<Child>,
}

fn main() {
    let empty = Parent {
        children: Vec::new(),
        favorite: None,
    };
    let parent = Parent {
        children: vec![Child { done: true }, Child { done: false }],
        favorite: Some(Child { done: true }),
    };
    let all_hit = Parent {
        children: vec![Child { done: true }, Child { done: true }],
        favorite: None,
    };
    let no_hit = Parent {
        children: vec![Child { done: false }, Child { done: false }],
        favorite: None,
    };
    let parent_fields = Parent::filter_fields();
    let done = Child::filter_fields().done;

    assert!(!parent_fields.children.any(done.eq(true)).matches(&empty));
    assert!(parent_fields.children.all(done.eq(true)).matches(&empty));
    assert!(parent_fields.children.none(done.eq(true)).matches(&empty));
    assert!(!parent_fields.favorite.is(done.eq(true)).matches(&empty));

    assert!(parent_fields.children.any(done.eq(true)).matches(&parent));
    assert!(!parent_fields.children.all(done.eq(true)).matches(&parent));
    assert!(!parent_fields.children.none(done.eq(true)).matches(&parent));
    assert!(parent_fields.favorite.is(done.eq(true)).matches(&parent));

    assert!(parent_fields.children.any(done.eq(true)).matches(&all_hit));
    assert!(parent_fields.children.all(done.eq(true)).matches(&all_hit));
    assert!(!parent_fields.children.none(done.eq(true)).matches(&all_hit));

    assert!(!parent_fields.children.any(done.eq(true)).matches(&no_hit));
    assert!(!parent_fields.children.all(done.eq(true)).matches(&no_hit));
    assert!(parent_fields.children.none(done.eq(true)).matches(&no_hit));

    assert!(format!("{:?}", parent_fields.children).contains("members"));
    assert!(format!("{:?}", parent_fields.favorite).contains("primary"));
}
