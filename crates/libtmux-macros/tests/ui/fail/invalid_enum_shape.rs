#![allow(dead_code)]

use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "invalid_enum_shape")]
struct InvalidEnumShape {
    #[filterable(enum)]
    collection: Vec<State>,
    #[filterable(enum)]
    nested_optional: Option<Option<State>>,
}

struct State;

fn main() {}
