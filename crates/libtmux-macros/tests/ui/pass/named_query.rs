#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::{Filterable as _, QueryIteratorExt};

#[derive(Filterable)]
#[filterable(target = "row")]
struct Row {
    name: String,
    done: bool,
}

fn main() {
    let values = [
        Row {
            name: String::from("build"),
            done: false,
        },
        Row {
            name: String::from("test"),
            done: true,
        },
    ];
    let fields = Row::filter_fields();
    let expression = fields.name.contains("ui").and(fields.done.eq(false));
    let selected = values.iter().matching(&expression).collect::<Vec<_>>();
    assert_eq!(selected.len(), 1);
}
