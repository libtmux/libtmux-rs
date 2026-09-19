use libtmux_macros::Filterable;

struct Child;

#[derive(Filterable)]
#[filterable(target = "relation_value")]
struct RelationFlagValue {
    #[filterable(many = Child)]
    children: Vec<Child>,
    #[filterable(one = "owner")]
    owner: Option<Child>,
}

fn main() {}
