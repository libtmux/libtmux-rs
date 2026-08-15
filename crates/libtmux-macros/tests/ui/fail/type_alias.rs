use libtmux_macros::Filterable;

type Label = String;

#[derive(Filterable)]
#[filterable(target = "alias_row")]
struct AliasRow {
    label: Label,
}

fn main() {}
