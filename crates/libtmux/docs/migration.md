# Migrating from 0.1.0-alpha.11

## Names, titles and start directories are text

Every argument tmux expands as a format now takes `TmuxArg`, and every
conversion into it escapes. `&str`, `String`, `OsString`, `Path` and
`&TmuxText` all convert, so ordinary calls are unchanged:

```no_run
# async fn names(server: &libtmux::Server) -> Result<(), libtmux::Error> {
// Unchanged, and now stored as written rather than expanded.
server.new_session("release#1").await?;
# Ok(())
# }
```

Two changes to make:

- Drop any `escape_format` you applied before calling one of these. The sink
  escapes now, so passing pre-escaped text stores the escape characters.
- Where you meant a format, say so with `TmuxArg::format`. This is the one way
  to reach `#{pane_current_path}` in a start directory, and it belongs only
  around a template the program itself wrote.

```no_run
# async fn split(pane: &libtmux::Pane) -> Result<(), libtmux::Error> {
use libtmux::{SplitDirection, SplitOptions, TmuxArg};

pane.split(
    SplitOptions::new(SplitDirection::Below)
        .start_directory(TmuxArg::format("#{pane_current_path}")),
)
.await?;
# Ok(())
# }
```

The sinks: `NewSessionOptions::{new, window_name, start_directory}`,
`NewWindowOptions::{new, start_directory}`, `SplitOptions::start_directory`,
`Session::rename`, `Window::rename`, `Pane::set_title`. Option names and the
`plan` operations escape internally and need no change at the call site.

## Control events

`ControlEvents` yields `Result<Event, Error>`. `ControlEvents::next_event`
and `ControlMode::next_event` return `Option<Result<Event, Error>>`. Change
`match event` to `match event?` in a fallible consumer:

```no_run
# async fn watch(mut events: libtmux::control::ControlEvents) -> Result<(), libtmux::Error> {
use libtmux::control::Event;

while let Some(event) = events.next_event().await {
    match event? {
        Event::Output { pane, bytes } => println!("{pane}: {} bytes", bytes.len()),
        Event::Exit { .. } => break,
        _ => {}
    }
}
events.shutdown().await?;
# Ok(())
# }
```

A terminal failure arrives once, after buffered notifications. Later polls
return `None`. Normal `%exit` produces `Event::Exit`; EOF without it produces
`ControlModeErrorKind::Closed`. Exhaustion waits for cleanup. Explicit
`shutdown` discards unread notifications, closes the connection and returns
any terminal error not already delivered by iteration. After an error has
been delivered, `shutdown` succeeds. `Server::shutdown` may discard pending
notifications so an unread stream cannot block executor shutdown.

`PaneOutput` keeps its infallible byte-stream contract. Its `None` combines
normal completion and failure; call `shutdown` to observe a connection error.

## Scoped errors

`with_session`, `with_window` and `with_pane` return
`Result<T, ScopeError<T, E>>` instead of `Result<T, E>`. Remove
`E: From<libtmux::Error>` from caller error types when it was needed only for
these helpers. Functions propagating a scope's result can return
`ScopeError<T, E>` or wrap it in their application error. Nested scopes
retain nested error types.

Match `ScopeError::Creation`, `Operation`, `Cleanup` or
`OperationAndCleanup`. The combined variant retains both original values, and
`Cleanup` retains the operation's own successful result, since cleanup
failing is the only way that result would otherwise be lost:

```
use libtmux::{Error, ScopeError};

fn both<T, E>(error: &ScopeError<T, E>) -> Option<(&E, &Error)> {
    match error {
        ScopeError::OperationAndCleanup { operation, cleanup } => Some((operation, cleanup)),
        _ => None,
    }
}

let error: ScopeError<(), _> = ScopeError::Operation("application error");
assert!(both(&error).is_none());
```

Cleanup errors carry `Error::AfterEffect` because resource creation succeeded.
`Debug` and `Display` show the operation and `Cleanup` values when `T` and
`E` implement the matching trait, and `std::error::Error` needs both, since
it requires them as supertraits; a caller whose types implement neither still
gets a working scope, with both values reachable by matching the variant.
The standard error source is the creation or cleanup `Error`, never the
operation value: `E` need not implement `std::error::Error` at all, so match
the operation variant to reach its value or its own source chain. This keeps
`ScopeError<T, E>` usable with boxed errors and values that do not implement
`std::error::Error`.

## Owned queries and names

Borrowed `.iter().matching(...)` calls retain their result and inference
behaviour. Use `.into_iter().matching_owned(...)` to move selected items
without cloning. `exactly_one` and `one_or_none` return the iterator's item
type, including owned values:

```
use libtmux::query::QueryIteratorExt;

let names = vec![String::from("build"), String::from("test")];
let selected = names.into_iter()
    .matching_owned(|name: &String| name.starts_with('b'))
    .exactly_one()?;
assert_eq!(selected, "build");
# Ok::<(), libtmux::query::ExactlyOneError>(())
```

Replace generic bounds such as `I: QueryIteratorExt<'a, T>` with
`I: Iterator<Item = &'a T> + QueryIteratorExt`. The trait no longer has type
or lifetime parameters. Ordinary `Iterator::filter` remains available for
closures with inferred arguments.

`TmuxText` implements `AsRef<[u8]>`. Use `server.session(session.name())`
and `session.window(window.name())` directly. The conversion borrows the
original bytes and never decodes them lossily.
