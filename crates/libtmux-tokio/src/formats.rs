//! Format plans and row decoders shared by the native drivers.

pub use libtmux_core::TmuxText;
#[cfg(feature = "unstable-fuzzing")]
pub use libtmux_core::formats::__fuzz_format_rows;
pub(crate) use libtmux_core::formats::*;

#[cfg(test)]
mod tests;
