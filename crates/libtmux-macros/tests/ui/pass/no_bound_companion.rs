#![allow(dead_code)]

use std::rc::Rc;

use libtmux_macros::Filterable;
use renamed_libtmux::query::{FilterEnum, Filterable as _};

struct OpaqueState(Rc<()>);

impl FilterEnum for OpaqueState {
    const FILTER_VARIANTS: &'static [&'static str] = &["only"];

    fn filter_name(&self) -> &'static str {
        "only"
    }
}

#[derive(Filterable)]
#[filterable(target = "related")]
struct Related {
    flag: bool,
    #[filterable(skip)]
    nonsend: Rc<()>,
}

#[derive(Filterable)]
#[filterable(target = "envelope")]
struct Envelope<E, R>
where
    E: FilterEnum,
    R: renamed_libtmux::query::Filterable,
{
    #[filterable(enum)]
    state: E,
    #[filterable(many)]
    related: Vec<R>,
    #[filterable(one)]
    primary: Option<R>,
}

fn assert_value_traits<T: Clone + Copy + core::fmt::Debug + Eq + Send + Sync>() {}

fn main() {
    assert_value_traits::<EnvelopeFields<OpaqueState, Related>>();

    let fields: EnvelopeFields<OpaqueState, Related> =
        Envelope::<OpaqueState, Related>::filter_fields();
    let related = Related::filter_fields().flag.eq(true);
    let _ = fields.state.eq(OpaqueState(Rc::new(())));
    let _ = fields.related.any(related.clone());
    let _ = fields.primary.is(related);
}
