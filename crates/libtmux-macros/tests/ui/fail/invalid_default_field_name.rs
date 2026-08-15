#![allow(non_snake_case)]

use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "invalid_default")]
struct InvalidDefaultFieldName {
    BadName: String,
    badName: String,
    _bad: String,
    café: String,
}

fn main() {}
