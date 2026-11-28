#![cfg_attr(feature = "blocking", doc = include_str!("../README.md"))]
#![cfg_attr(
    not(feature = "blocking"),
    doc = r#"
Shared tmux types with the default blocking client disabled.

Enable `blocking` for `libtmux::blocking::Server` or `tokio` for
`libtmux::tokio::Server`. Describing a session performs no I/O:

```rust
use libtmux::NewSession;

let request = NewSession::new("issue#185").window_name("editor").size(120, 40);
let operands = request.operands(NewSession::RECEIPT_FORMAT);
assert!(operands.iter().any(|arg| arg.as_os_str() == "issue##185"));
```
"#
)]
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

#[cfg(all(doctest, feature = "derive"))]
#[doc = include_str!("../libtmux-macros-README.md")]
mod macros_readme {}
