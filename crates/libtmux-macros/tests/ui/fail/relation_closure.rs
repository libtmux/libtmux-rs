use libtmux_macros::Filterable;
use renamed_libtmux::query::Filterable as _;

#[derive(Filterable)]
#[filterable(target = "child")]
struct Child {
    flag: bool,
}

#[derive(Filterable)]
#[filterable(target = "parent")]
struct Parent {
    #[filterable(many)]
    children: Vec<Child>,
}

fn main() {
    let _ = Parent::filter_fields().children.any(|_: &Child| true);
}
