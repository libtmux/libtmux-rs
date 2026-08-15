use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "pointer_width")]
struct PointerWidthIntegers {
    signed: isize,
    unsigned: usize,
}

fn main() {}
