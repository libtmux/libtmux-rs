#![allow(dead_code)]

use libtmux_macros::Filterable;

trait HasValue {
    type Value;
}

struct Holder;

impl HasValue for Holder {
    type Value = String;
}

#[derive(Filterable)]
#[filterable(target = "unsupported")]
struct UnsupportedTypes {
    float: f64,
    reference: &'static str,
    collection: Vec<String>,
    tuple: (String, bool),
    nested_optional: Option<Option<String>>,
    qualified_self: <Holder as HasValue>::Value,
}

fn main() {}
