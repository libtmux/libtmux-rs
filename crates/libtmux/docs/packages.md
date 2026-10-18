# Packages and runtimes

`libtmux` owns the tmux operations. `libtmux-tokio` supplies native Tokio
processes, timers, and tasks. `libtmux-core` contains values and protocol
parsing without any crate dependencies.

| Package | Use it for | Runtime |
| --- | --- | --- |
| `libtmux-core` | IDs, targets, byte text, versions, and control protocol parsing | None; it performs no I/O |
| `libtmux` | The complete async client, including control mode and scoped operations | `async-io` by default, or an explicit driver |
| `libtmux-tokio` | The same operations in a Tokio application | Tokio |

`libtmux` is usable without Tokio. It still depends on an async executor,
I/O support, synchronisation, and the core package. Choose it when removing
Tokio is the requirement; it does not meet a zero-dependency client requirement.

## Synchronous callers

Import `libtmux::WaitExt` and call `.wait()` on an operation's future. The
calling thread waits while the configured driver performs I/O. The
`blocking` feature also provides `blocking::Runtime` for a block containing
several operations. Neither entry point belongs on an async executor thread.

The handles, options, results, query language, plans, and control-mode
operations are the same ones used by asynchronous callers. A synchronous
caller changes how it waits for completion.

## Tokio callers

Construct `libtmux_tokio::Server` or `libtmux_tokio::test::TestServer` inside a
Tokio application. Their driver uses Tokio's process, timer, and task APIs.
Command execution does not use `spawn_blocking`.

Returned `Session`, `Window`, `Pane`, and `Client` handles are shared
`libtmux` types. Each carries the driver chosen by its server. Moving one
between modules does not change its runtime. The Tokio `Server` has explicit
methods for both ordinary calls and associated-function calls, and passes its
own type to `with_channel_lock` callbacks.

The Tokio runtime must remain alive until shutdown finishes. Dropping a
handle does not replace explicit shutdown when cleanup errors matter.

## Control mode

Both clients use the same actor and protocol parser. A request is followed by
a private reply fence, so nested commands and command aliases may emit
multiple response blocks without giving the next request the wrong reply.
Notifications remain separate from replies. Reply limits cover the whole
expanded request, excluding its private fence.

A command that waits indefinitely can also hold up its fence. Use the
subprocess channel APIs for `wait-for` waits and locks. A command that closes
the control connection before the fence returns reports a closed connection.

Parked channel and lock clients have a separate limit of 64 per server.
`DispatchLimits::max_wait_clients` changes that limit. A full budget returns
`Error::Overloaded` before starting another client. Timed-out channel waits
may retain their client and permit until a later signal or server shutdown.

## Features and dependencies

Optional features on `libtmux-tokio` enable the matching features on
`libtmux`. `libtmux-core` has no feature-dependent dependencies. Query,
serialisation, schema, derive, tracing, plans, and test fixtures remain
explicit choices in the client packages.

Cargo can unify features when both clients appear in one graph. The runtime
is chosen by a server constructor, so enabling Tokio elsewhere does not
change how an existing server executes a command.

## Platform and publishing

The client packages support Linux, including WSL, with Rust 1.97.1 or later.
They require an installed tmux executable. The value package uses only the
standard library and does not launch tmux.

Publish `libtmux-core` and `libtmux-macros` first, then `libtmux`, then
`libtmux-tokio`, and finally the applications. Exact prerelease dependencies
keep the packages aligned. `just package` checks their packaged contents;
`just runtime-boundaries` checks the dependency split.
