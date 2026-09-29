use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(
    target = "first",
    fields = "FirstFields",
    crate = "libtmux"
)]
#[filterable(
    target = "second",
    fields = "SecondFields",
    crate = "libtmux"
)]
struct DuplicateContainerKeys {
    name: String,
}

fn main() {}
