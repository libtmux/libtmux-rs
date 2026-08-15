#![no_implicit_prelude]
#![allow(dead_code, non_camel_case_types)]

extern crate core;
extern crate libtmux_macros;
extern crate renamed_libtmux;
extern crate std;

use crate::libtmux_macros::Filterable;
use crate::renamed_libtmux::query::Filterable as _;

type i128 = ();
type u128 = ();

#[derive(Filterable)]
#[filterable(target = "qualified_child")]
struct QualifiedChild {
    label: ::std::string::String,
}

#[derive(Filterable)]
#[filterable(target = "qualified_row")]
struct QualifiedRow {
    text: ::std::string::String,
    raw: crate::renamed_libtmux::TmuxText,
    maybe_flag: ::core::option::Option<bool>,
    signed: ::core::primitive::i16,
    maybe_signed: ::core::option::Option<::core::primitive::i32>,
    unsigned: ::core::primitive::u16,
    maybe_unsigned: ::core::option::Option<::core::primitive::u32>,
    #[filterable(many)]
    children: ::std::vec::Vec<QualifiedChild>,
    #[filterable(one)]
    favorite: ::core::option::Option<QualifiedChild>,
}

fn main() {
    let fields = QualifiedRow::filter_fields();
    let child = QualifiedChild::filter_fields().label.eq("child");
    let _ = fields.text.eq("text");
    let _ = fields.raw.eq("raw");
    let _ = fields.maybe_flag.eq(true);
    let _ = fields.signed.eq(-1);
    let _ = fields.maybe_signed.eq(-2);
    let _ = fields.unsigned.eq(1);
    let _ = fields.maybe_unsigned.eq(2);
    let _ = fields
        .children
        .any(::core::clone::Clone::clone(&child));
    let _ = fields.favorite.is(child);
}
