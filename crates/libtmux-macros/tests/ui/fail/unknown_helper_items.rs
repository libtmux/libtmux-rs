use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "unknown_items", surprise)]
struct UnknownHelperItems {
    #[filterable(mystery)]
    name: String,
}

fn main() {}
