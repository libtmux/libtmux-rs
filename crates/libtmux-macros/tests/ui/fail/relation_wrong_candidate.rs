use libtmux_macros::Filterable;
use renamed_libtmux::query::Filterable as _;

#[derive(Filterable)]
#[filterable(target = "child")]
struct Child {
    flag: bool,
}

#[derive(Filterable)]
#[filterable(target = "other")]
struct Other {
    flag: bool,
}

#[derive(Filterable)]
#[filterable(target = "parent")]
struct Parent {
    #[filterable(many)]
    children: Vec<Child>,
    #[filterable(one)]
    favorite: Option<Child>,
}

fn main() {
    let expression = Other::filter_fields().flag.eq(true);
    let fields = Parent::filter_fields();
    let _ = fields.children.any(expression.clone());
    let _ = fields.favorite.is(expression);
}
