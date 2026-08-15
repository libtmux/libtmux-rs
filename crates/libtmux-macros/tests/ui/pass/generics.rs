#![allow(dead_code)]

use std::marker::PhantomData;
use std::rc::Rc;

use libtmux_macros::Filterable;
use renamed_libtmux::query::Filterable as _;

struct OpaqueIterator(Rc<()>);

impl Iterator for OpaqueIterator {
    type Item = ();

    fn next(&mut self) -> Option<Self::Item> {
        None
    }
}

#[derive(Filterable)]
#[filterable(target = "generic_row", fields = "GenericHandles")]
struct GenericRow<'a, T, const N: usize>
where
    T: Iterator<Item = ()>,
{
    name: String,
    #[filterable(skip)]
    iterator: T,
    #[filterable(skip)]
    marker: PhantomData<&'a [u8; N]>,
}

fn assert_value_traits<T: Clone + Copy + core::fmt::Debug + Eq + Send + Sync>() {}

fn main() {
    assert_value_traits::<GenericHandles<'static, OpaqueIterator, 3>>();

    let fields: GenericHandles<'static, OpaqueIterator, 3> =
        GenericRow::<'static, OpaqueIterator, 3>::filter_fields();
    let _ = fields.name.eq("generic");
}
