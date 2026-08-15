use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(
    target = "first",
    fields = "FirstFields",
    crate = "renamed_libtmux"
)]
#[filterable(
    target = "second",
    fields = "SecondFields",
    crate = "renamed_libtmux"
)]
struct DuplicateContainerKeys {
    name: String,
}

fn main() {}
