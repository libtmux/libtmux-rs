use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "rename_skip")]
struct RenamePlusSkip {
    #[filterable(rename = "cached", skip)]
    cached_value: usize,
}

fn main() {}
