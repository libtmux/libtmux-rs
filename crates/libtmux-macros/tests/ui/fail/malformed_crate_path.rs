use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "malformed_crate", crate = "not a Rust path")]
struct MalformedCratePath {
    name: String,
}

fn main() {}
