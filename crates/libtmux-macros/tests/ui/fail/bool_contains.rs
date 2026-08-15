use libtmux_macros::Filterable;
use renamed_libtmux::query::Filterable as _;

#[derive(Filterable)]
#[filterable(target = "bool_row")]
struct BoolRow {
    flag: bool,
}

fn main() {
    let field = BoolRow::filter_fields().flag;
    let _ = (&field).contains("true");
}
