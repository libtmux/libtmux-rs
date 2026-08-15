use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "number")]
union Number {
    signed: i32,
    unsigned: u32,
}

fn main() {}
