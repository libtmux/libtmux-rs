use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "conflicting")]
struct ConflictingFieldAnnotations {
    #[filterable(enum, many)]
    states: Vec<State>,
    #[filterable(many, one)]
    children: Vec<Child>,
    #[filterable(skip, enum)]
    cached_state: State,
}

struct State;
struct Child;

fn main() {}
