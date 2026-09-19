# Migrating from 0.1.0-alpha.11

## Timestamps are `SystemTime`

`Session::created`, `Session::last_attached`, `Window::last_activity`,
`Client::created` and `ServerGeneration::start_time` return
`std::time::SystemTime` in place of an `i64` of Unix seconds:

```no_run
# fn age(session: &libtmux::Session) -> Result<(), std::time::SystemTimeError> {
use std::time::{SystemTime, UNIX_EPOCH};

// was: let created: i64 = session.created();
let created: SystemTime = session.created();
let age = SystemTime::now().duration_since(created)?;

// The seconds are one conversion away.
let seconds = created.duration_since(UNIX_EPOCH)?.as_secs();
# let _ = (age, seconds);
# Ok(())
# }
```

The field handles are unchanged: `session.get(fields.session_created)` and a
filter on it still see the `i64` tmux reports.

## `respawn` and `display_menu` take types, not literals

```no_run
# async fn respawn(pane: &mut libtmux::Pane) -> Result<(), libtmux::Error> {
// was: pane.respawn(Some("sh"), true)
pane.respawn(Some("sh"), libtmux::Respawn::Replacing).await?;
# Ok(())
# }
```

`Respawn::OnlyIfDead` is the `false` case, and it is the one worth checking
for: tmux refuses the respawn while the old command is alive.

`Server::display_menu` takes `MenuItem::new(label, key, command)` in place of
a `(String, String, String)` triple.

## ID filter handles carry their ID type

`PaneFields::pane_id`, `WindowFields::window_id` and
`SessionFields::session_id` are `TextField<Target, PaneId>`,
`TextField<Target, WindowId>` and `TextField<Target, SessionId>`. Filtering
through them is unchanged. Only code that spells the handle's type changes:

```
use libtmux::query::{Filterable as _, TextField};
use libtmux::{Pane, PaneId};

// was: let handle: TextField<Pane> = Pane::filter_fields().pane_id;
let handle: TextField<Pane, PaneId> = Pane::filter_fields().pane_id;
let _ = handle.eq("%1");
```

## One option reader: `typed_option`

`get_option` on `Server`, `Session`, `Window` and `Pane`, and
`Server::{get_global_option, get_global_window_option}`, are gone. Read with
`typed_option`, `Server::typed_global_option` or
`Server::typed_global_window_option`. A flag now arrives as a `bool` and a
number as an `i64`; where the bytes are wanted, `TmuxText::from` gives back
exactly what the removed reader returned:

```no_run
# async fn read(session: &libtmux::Session) -> Result<(), libtmux::Error> {
use libtmux::TmuxText;

// was: session.get_option("status-left").await?
let left = session.typed_option("status-left").await?.map(TmuxText::from);
# let _ = left;
# Ok(())
# }
```

`set_typed_option` writes the same types back, checked against tmux's option
table; `set_option` is unchanged.

## The `_or_empty` listing twins are gone

Replace `x_or_empty().await` with `x().await.unwrap_or_default()`:

```no_run
# async fn listing(server: &libtmux::Server) -> Result<(), libtmux::Error> {
let sessions = server.sessions().await.unwrap_or_default();
# let _ = sessions;
# Ok(())
# }
```

Worth a moment's thought rather than a blind rewrite: the twins collapsed an
unreachable tmux into an empty list, and anything that reconciles state should
take the `?` instead.

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
