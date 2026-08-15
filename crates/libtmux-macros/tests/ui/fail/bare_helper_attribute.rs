use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable]
#[filterable(target = "bare_helper")]
struct BareHelperAttribute {
    name: String,
}

#[derive(Filterable)]
#[filterable(target = "bare_field_helper")]
struct BareFieldHelperAttribute {
    #[filterable]
    name: String,
}

fn main() {}
