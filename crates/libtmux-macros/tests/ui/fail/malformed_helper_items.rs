use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "malformed_items", fields = 7, crate)]
struct MalformedHelperItems {
    #[filterable(rename = false)]
    name: String,
    #[filterable(skip = true)]
    cached: usize,
    #[filterable(enum("state"))]
    state: State,
}

struct State;

fn main() {}
