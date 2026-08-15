use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "tuple_row")]
struct TupleRow(String);

fn main() {}
