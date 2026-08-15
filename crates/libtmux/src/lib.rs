#![doc = include_str!("../README.md")]
//!
//! ## Query iterators
//!
//! Listings hand back an ordered `Vec<T>` that the caller owns. Borrow it with
//! `.iter()`, use [`Iterator::filter`] for an inline closure, and
//! [`query::QueryIteratorExt::matching`] for a portable expression or a named
//! [`query::Matcher`]. Exact cardinality inspects at most two items.
//!
//! If another iterator extension trait, such as `itertools::Itertools`, adds
//! the same method name, use universal function call syntax to select this
//! crate's method:
//!
//! ```
//! use libtmux::query::QueryIteratorExt;
//!
//! let values = vec![1];
//! let item = QueryIteratorExt::exactly_one(values.iter());
//! assert_eq!(item, Ok(&1));
//! ```
//!
//! ## Finding what is already there
//!
//! Naming an object is cheaper than listing and scanning for it:
//!
//! ```no_run
//! # async fn walk() -> Result<(), libtmux::Error> {
//! let server = libtmux::Server::new()?;
//!
//! // Find one object rather than listing and scanning.
//! if let Some(session) = server.session("work").await? {
//!     if let Some(window) = session.window("editor").await? {
//!         if let Some(pane) = window.active_pane().await? {
//!             pane.send_keys("cargo test").await?;
//!             pane.send_key_names(["Enter"]).await?;
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Listings come in pairs. The plain form returns an empty `Vec` when the
//! underlying tmux command fails, which suits a status line; the `try_` form
//! keeps the reason, which suits anything that must not guess:
//!
//! ```no_run
//! # async fn both(server: &libtmux::Server) -> Result<(), libtmux::Error> {
//! let quiet = server.sessions().await;          // empty on failure
//! let loud = server.try_sessions().await?;      // Err on failure
//! # let _ = (quiet, loud);
//! # Ok(())
//! # }
//! ```
//!
//! ## Building something and cleaning up
//!
//! Scoped operations kill what they create, whether the body succeeded or
//! failed. `Drop` is deliberately not destructive, so nothing disappears
//! because a handle went out of scope.
//!
//! ```no_run
//! # async fn scoped(server: &libtmux::Server) -> Result<(), libtmux::Error> {
//! let id = server
//!     .with_session("throwaway", async |session| {
//!         session.new_window("build").await?;
//!         Ok::<_, libtmux::Error>(session.id().to_string())
//!     })
//!     .await?;
//! # let _ = id;
//! # Ok(())
//! # }
//! ```
//!
//! Setup and teardown failures convert into the operation's own error type,
//! so there is one `?` rather than two. If both the operation and the cleanup
//! fail, the operation's error is returned: that is the work you were doing.
//!
//! ## Options carry types
//!
//! tmux reports no type over the command line, so the crate generates the
//! schema from tmux's own table. That matters more than it sounds: `status`
//! holds `"on"` but is a choice, because tmux also accepts `2` through `5`.
//!
//! ```no_run
//! # async fn options(server: &libtmux::Server) -> Result<(), libtmux::Error> {
//! use libtmux::{OptionValue, option_names};
//!
//! // Names are constants, so a typo does not compile.
//! let mouse = server.typed_global_option(option_names::MOUSE).await?;
//! assert!(matches!(mouse, Some(OptionValue::Flag(_))));
//! # Ok(())
//! # }
//! ```
//!
//! ## Examples
//!
//! Runnable programs live in `examples/`: `inspect` reports what a server is
//! running, `find` selects panes with a typed expression, and `scratch`
//! builds a throwaway session on its own socket and cleans it up.
//!
//! ## Filtering the hierarchy
//!
//! [`Session`], [`Window`], [`Pane`], and [`Client`] carry generated field
//! handles, so an expression names the same type a listing returns:
//!
//! ```
//! use libtmux::query::Filterable as _;
//!
//! let fields = libtmux::Session::filter_fields();
//! let expression = fields.session_name.starts_with("build");
//! let sessions: Vec<libtmux::Session> = Vec::new();
//! assert_eq!(sessions.iter().count(), 0);
//! # let _ = expression;
//! ```
//!
//! Field types decide which operations exist, so a mismatched comparison is a
//! compile error rather than a predicate that is always false:
//!
//! ```compile_fail
//! use libtmux::query::Filterable as _;
//!
//! let fields = libtmux::Session::filter_fields();
//! // `session_name` is text, so it has no integer comparison.
//! let _ = fields.session_name.eq(3_u32);
//! ```
//!
//! ```compile_fail
//! use libtmux::query::Filterable as _;
//!
//! let fields = libtmux::Session::filter_fields();
//! // `session_windows` is an integer, so it has no substring operation.
//! let _ = fields.session_windows.contains("3");
//! ```
//!
//! A question about what a session *contains* needs a value that holds its
//! windows. [`Server::hierarchy`] returns one, and [`SessionTree`] and
//! [`WindowTree`] carry relations for it:
//!
//! ```no_run
//! # async fn contained(server: &libtmux::Server) -> Result<(), libtmux::Error> {
//! use libtmux::query::{Filterable as _, QueryIteratorExt as _};
//! use libtmux::{SessionTree, WindowTree};
//!
//! let sessions = SessionTree::filter_fields();
//! let windows = WindowTree::filter_fields();
//!
//! // The session's own fields sit beside the relation, not behind it.
//! let building = sessions
//!     .session
//!     .session_name
//!     .starts_with("build")
//!     .and(sessions.windows.any(windows.window.window_name.eq("editor")));
//!
//! for branch in server.hierarchy().await?.iter().matching(&building) {
//!     println!("{}", branch.session);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Query extensions intentionally apply only to borrowed iterators:
//!
//! ```compile_fail
//! use libtmux::query::QueryIteratorExt;
//!
//! let values = vec![1, 2, 3];
//! let _ = values.into_iter().matching(|candidate: &i32| *candidate > 1);
//! ```
#![cfg_attr(
    feature = "control-mode",
    doc = r#"
## Being told instead of asking

Everything above runs a tmux command and reads the answer. The `control-mode`
feature opens one connection and keeps it, so tmux reports what happens as it
happens -- no polling interval, and nothing missed between two polls.

[`Pane::stream_output`] is the narrow version: what one pane writes, as it
writes it, where [`Pane::capture`] gives only what is on screen now.

```no_run
# async fn watch(pane: &libtmux::Pane) -> Result<(), libtmux::Error> {
let mut output = pane.stream_output().await?;

while let Some(chunk) = output.next_chunk().await {
    println!("{} bytes", chunk.len());
}

output.shutdown().await
# }
```

[`control::ControlMode`] is the whole connection: every notification the server
sends, plus commands that travel down the connection rather than spawning a
process. Sending and watching are separate handles, so a task can act on what
it sees. See the [`control`] module.
"#
)]
#![cfg_attr(
    feature = "blocking",
    doc = r#"
## Calling from code that is not async

The `blocking` feature adds a [`blocking::Runtime`] that drives this crate's
futures to completion. It is deliberately a runtime rather than a mirrored
blocking API: one type to learn, and no second surface to keep in step.

```no_run
# fn run() -> Result<(), libtmux::Error> {
let runtime = libtmux::blocking::Runtime::new()?;
let server = libtmux::Server::new()?;

let sessions = runtime.run(server.try_sessions())?;
println!("{} sessions", sessions.len());
# Ok(())
# }
```
"#
)]
// docs.rs builds with this cfg set, so every gated item there carries the
// feature that unlocks it. Nightly-only, and a no-op everywhere else.
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]

#[cfg(not(unix))]
compile_error!("libtmux requires a Unix target with tmux available");

#[cfg(feature = "blocking")]
pub mod blocking;
mod capabilities;
mod client;
mod command;
#[cfg(feature = "control-mode")]
pub mod control;
mod error;
mod formats;
pub mod hooks;
mod internal;
mod options;
mod pane;
#[cfg(feature = "plan")]
pub mod plan;
#[cfg(feature = "query")]
pub mod query;
mod server;
mod session;
mod snapshot;
mod target;
mod version;
mod window;

#[cfg(feature = "test-support")]
pub mod test;

pub use capabilities::EngineCapabilities;
pub use client::Client;
pub use command::{Command, CommandChain, CommandResult, CommandSummary};
#[cfg(feature = "control-mode")]
pub use error::ControlModeErrorKind;
pub use error::{
    Error, ErrorKind, IdParseError, ListingDecodeError, ObjectKind, OptionErrorKind,
    ServerConfigurationErrorKind,
};
pub use formats::TmuxText;
pub use hooks::{IndexedHooks, ReplaceMode};
pub use options::{
    OptionKind, OptionSchema, OptionScope, OptionValue, names as option_names, option_schema,
};
pub use pane::{CaptureOptions, Pane};
pub use server::{
    AccessMode, AccessRule, Chooser, NewSessionOptions, PromptKind, Server, ServerBuilder,
    SessionTree, WindowTree,
};
#[cfg(feature = "query")]
pub use server::{SessionTreeFields, WindowTreeFields};
pub use session::{EnvironmentEntry, NewWindowOptions, Session, WindowPlacement};
pub use snapshot::PaneProgressState;
#[cfg(feature = "query")]
pub use snapshot::{ClientFields, PaneFields, SessionFields, WindowFields};
pub use target::{
    PaneId, PaneTarget, ServerIdentity, SessionId, SessionName, SessionNameError, SessionTarget,
    WindowId, WindowTarget,
};
pub use version::{ReleaseSuffix, ReleaseVersion, TmuxVersion};
pub use window::{
    PaneDirection, PaneSize, ResizeDirection, Rotation, SplitDirection, SplitOptions, Window,
};

/// Derive a stable typed filter schema for a named struct.
///
/// The generated companion exposes typed field handles through
/// [`query::Filterable::filter_fields`].
///
/// # Examples
///
/// ```
/// use libtmux::query::{Filterable as _, QueryIteratorExt as _};
///
/// #[derive(libtmux::Filterable)]
/// #[filterable(target = "task")]
/// # #[filterable(crate = "libtmux")]
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
#[cfg(feature = "derive")]
#[doc(inline)]
pub use libtmux_macros::Filterable;
