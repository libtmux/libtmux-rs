use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "empty_helper")]
#[filterable()]
struct EmptyHelperAttribute {
    name: String,
}

fn main() {}
