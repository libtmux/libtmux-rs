#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub use libtmux_core::*;

#[cfg(feature = "blocking")]
#[doc(inline)]
pub use libtmux_blocking as blocking;

#[cfg(feature = "tokio")]
#[doc(inline)]
pub use libtmux_tokio as tokio;

#[cfg(doctest)]
/// ```compile_fail,E0432
/// use libtmux::Server;
/// ```
///
/// ```compile_fail,E0432
/// use libtmux::blocking::Runtime;
/// ```
///
/// ```compile_fail,E0432
/// use libtmux::NewSessionOptions;
/// ```
mod retired_imports {}

#[cfg(all(doctest, feature = "test-support", feature = "query"))]
#[doc = include_str!("../docs/workspace-readme.md")]
struct WorkspaceReadme;
