#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::Filterable as _;

mod support {
    pub use renamed_libtmux as core;
}

#[derive(Filterable)]
#[filterable(target = "override_row", crate = "crate::support::core")]
struct OverrideRow {
    name: String,
}

fn main() {
    let _ = OverrideRow::filter_fields().name.eq("override");
}
