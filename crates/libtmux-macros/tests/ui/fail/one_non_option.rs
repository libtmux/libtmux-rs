#![allow(dead_code)]

use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "invalid_one")]
struct InvalidOne {
    #[filterable(one)]
    child: Child,
    #[filterable(one)]
    vector: Vec<Child>,
    #[filterable(one)]
    loader: fn() -> Option<Child>,
}

struct Child;

fn main() {}
