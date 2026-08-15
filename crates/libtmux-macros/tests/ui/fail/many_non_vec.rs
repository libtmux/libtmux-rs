#![allow(dead_code)]

use std::collections::HashSet;

use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "invalid_many")]
struct InvalidMany {
    #[filterable(many)]
    optional_vector: Option<Vec<Child>>,
    #[filterable(many)]
    set: HashSet<Child>,
    #[filterable(many)]
    array: [Child; 1],
    #[filterable(many)]
    loader: fn() -> Vec<Child>,
}

struct Child;

fn main() {}
