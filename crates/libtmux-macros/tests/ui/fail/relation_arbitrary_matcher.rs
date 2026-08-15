use libtmux_macros::Filterable;
use renamed_libtmux::query::{Filterable as _, Matcher};

#[derive(Filterable)]
#[filterable(target = "child")]
struct Child {
    flag: bool,
}

struct Always;

impl Matcher<Child> for Always {
    fn matches(&self, _: &Child) -> bool {
        true
    }
}

#[derive(Filterable)]
#[filterable(target = "parent")]
struct Parent {
    #[filterable(one)]
    favorite: Option<Child>,
}

fn main() {
    let _ = Parent::filter_fields().favorite.is(Always);
}
