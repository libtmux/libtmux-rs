#![allow(dead_code)]

use libtmux_macros::Filterable;

mod model {
    use super::Filterable;

    #[derive(Filterable)]
    #[filterable(target = "private_row")]
    struct PrivateRow {
        name: String,
    }
}

fn main() {
    let _: model::PrivateRowFields;
}
