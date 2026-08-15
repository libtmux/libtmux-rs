#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::FilterEnum;

enum State {
    Ready,
}

impl FilterEnum for State {
    const FILTER_VARIANTS: &'static [&'static str] = &["ready"];

    fn filter_name(&self) -> &'static str {
        "ready"
    }
}

#[derive(Filterable)]
#[filterable(target = "explicit_child")]
struct Child {
    done: bool,
}

#[derive(Filterable)]
#[filterable(target = "explicit_annotations")]
struct ExplicitAnnotations {
    state: State,
    children: Vec<Child>,
    favorite: Option<Child>,
}

fn main() {}
