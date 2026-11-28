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

#[cfg(feature = "derive")]
pub use libtmux_core::Filterable;
#[cfg(feature = "plan")]
pub use libtmux_core::plan;
pub use libtmux_core::{
    Availability, Capture, ClientSnapshot, Command, CommandChain, CommandKind, CommandSummary,
    CreatedSession, Effect, Error, FormatCodecError, IdParseError, NewSession, ObjectKey,
    ObservePane, Operand, OperandKind, OptionScope, PaneId, PaneKey, PaneProgressState,
    PaneSnapshot, PaneTarget, ReleaseSuffix, ReleaseVersion, Reply, ServerGeneration,
    ServerIdentity, SessionId, SessionKey, SessionName, SessionNameError, SessionSnapshot,
    SessionTarget, Snapshot, SnapshotList, SplitDirection, TmuxArg, TmuxText, TmuxVersion,
    VersionError, WindowId, WindowKey, WindowLinkSnapshot, WindowSnapshot, WindowTarget,
    escape_format, options, since,
};
#[cfg(feature = "query")]
pub use libtmux_core::{ClientFields, PaneFields, SessionFields, WindowFields, query};

/// Command encoding and reply decoding for transport implementers.
///
/// Encode a literal operand before passing it to the tmux executable:
///
/// ```
/// use libtmux::protocol::argv_operand;
/// use std::ffi::OsStr;
///
/// assert_eq!(argv_operand(OsStr::new("done;")), OsStr::new(r"done\;"));
/// ```
pub mod protocol {
    pub use libtmux_core::{
        CreationNonce, OrdinaryGenerationProbe, OrdinaryPaneObservation, OrdinaryPaneProbe,
        OrdinaryVersionProbe, argv_operand, control,
    };
}

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
///
/// ```compile_fail,E0432
/// use libtmux::CreationNonce;
/// ```
mod retired_imports {}

#[cfg(all(doctest, feature = "derive"))]
#[doc = include_str!("../libtmux-macros-README.md")]
mod macros_readme {}
