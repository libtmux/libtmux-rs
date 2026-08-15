#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::Filterable as _;

#[derive(Filterable)]
#[filterable(target = "renamed_dependency_2")]
struct RenamedDependency {
    name: String,
}

fn main() {
    let fields: renamed_libtmux::query::TextField<RenamedDependency> =
        RenamedDependency::filter_fields().name;
    assert!(format!("{fields:?}").contains("renamed_dependency_2"));
    let _ = fields.eq("resolved");
}
