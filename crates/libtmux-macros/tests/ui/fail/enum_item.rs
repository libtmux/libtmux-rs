use libtmux_macros::Filterable;

#[derive(Filterable)]
#[filterable(target = "state")]
enum State {
    Ready,
}

fn main() {}
