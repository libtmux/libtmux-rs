use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "field_path")]
struct FieldOptionPath {
    #[filterable(serde::skip)]
    name: String,
}

fn main() {}
