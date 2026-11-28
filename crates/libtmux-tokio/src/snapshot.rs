//! Observed tmux values shared by both clients.
pub(crate) use libtmux_core::snapshot::*;
#[cfg(feature = "query")]
pub use libtmux_core::snapshot::{
    ClientFields, PaneFields, ReadField, SessionFields, WindowFields,
};
#[cfg(all(test, feature = "test-support"))]
mod tests;
