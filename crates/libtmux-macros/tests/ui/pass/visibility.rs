//! Public generated items must carry documentation.

#![deny(missing_docs)]
#![allow(dead_code)]

/// Public downstream model used to lint generated documentation.
pub mod model {
    use libtmux_macros::Filterable;

    /// A public downstream candidate.
    #[derive(Filterable)]
    #[filterable(target = "public_row")]
    pub struct PublicRow {
        name: String,
    }
}

use renamed_libtmux::query::Filterable as _;

fn main() {
    let fields: model::PublicRowFields = model::PublicRow::filter_fields();
    let _: renamed_libtmux::query::TextField<model::PublicRow> = fields.name;
}
