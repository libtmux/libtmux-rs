#![allow(dead_code)]

use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "duplicate_fields")]
struct DuplicateFieldKeys {
    #[filterable(rename = "first")]
    #[filterable(rename = "second")]
    renamed: String,
    #[filterable(skip)]
    #[filterable(skip)]
    skipped: usize,
    #[filterable(enum)]
    #[filterable(enum)]
    state: State,
    #[filterable(many)]
    #[filterable(many)]
    children: Vec<Child>,
    #[filterable(one)]
    #[filterable(one)]
    favorite: Option<Child>,
}

struct State;
struct Child;

fn main() {}
