//! Derive macros for libtmux filter expressions.
//!
//! Start from the [`libtmux` documentation][libtmux]: this crate is the
//! implementation of its `derive` feature, and a caller does not depend on it
//! directly.
//!
//! [libtmux]: https://docs.rs/libtmux

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

mod expand;
mod model;
mod parse;

use expand::expand_filterable;

/// Derive a stable typed filter schema for a named struct.
///
/// The required `target` is the stable name embedded in portable expressions.
/// Every supported field produces one member of the generated `<Type>Fields`
/// companion. Bring `renamed_libtmux::query::Filterable` into scope to call
/// `filter_fields`.
///
/// Enable the core crate's `derive` feature and use its root `Filterable`
/// re-export. A deliberate direct dependency on `libtmux-macros` must exactly
/// match the `libtmux` version because the expansion uses a hidden core ABI.
///
/// # Examples
///
/// ```
/// use renamed_libtmux::Filterable;
/// use renamed_libtmux::query::{Filterable as _, QueryIteratorExt as _};
///
/// #[derive(Filterable)]
/// #[filterable(target = "task")]
/// struct Task {
///     name: String,
///     done: bool,
/// }
///
/// let values = vec![
///     Task { name: "build".into(), done: false },
///     Task { name: "test".into(), done: true },
/// ];
/// let fields = Task::filter_fields();
/// let expression = fields.name.contains("ui").and(fields.done.eq(false));
/// let selected = values.iter().matching(&expression).collect::<Vec<_>>();
/// assert_eq!(selected.len(), 1);
/// ```
#[proc_macro_derive(Filterable, attributes(filterable))]
pub fn derive_filterable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_filterable(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
