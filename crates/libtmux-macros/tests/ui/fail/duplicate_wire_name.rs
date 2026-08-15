use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "duplicate_wire")]
struct DuplicateWireName {
    #[filterable(rename = "name")]
    display_name: String,
    name: String,
}

fn main() {}
