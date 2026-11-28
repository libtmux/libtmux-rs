# libtmux-blocking

Drive tmux synchronously against an explicit Unix socket. The client uses
native pipes and deadlines. Its normal dependency graph contains no Tokio.

A request defaults to a five-second deadline and one MiB each for stdout and
stderr. Set the deadline and byte limit on [`RequestOptions`] before passing
it to a method such as [`Server::capture_with`].

## Create and clean a session

From a binary project next to this checkout, add the client and shared types:

```console
$ cargo add libtmux-blocking --path ../libtmux-rs/crates/libtmux-blocking
```

```console
$ cargo add libtmux-core --path ../libtmux-rs/crates/libtmux-core
```

Save this as `src/main.rs`. [`Server::at`] selects the endpoint;
[`Server::new_session`] returns the session, window and pane identities from
one creation. Observation checks that the pane still belongs to that daemon.

```rust,no_run
use libtmux_blocking::Server;
use libtmux_core::NewSession;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::args_os().nth(1).ok_or("pass an absolute socket path")?;
    let server = Server::at(socket)?;
    let created = server.new_session(NewSession::new("readme").command("sleep 30"))?;
    let observed = server.observe_pane(created.pane().id());
    server.kill_created_session(&created)?;
    server.close_ordinary()?;
    assert_eq!(&observed?, created.pane());
    println!("created and cleaned one session");
    Ok(())
}
```

Pass an absolute socket path in a private directory you own. These commands
choose a separate endpoint for this program:

```console
$ mkdir -p /tmp/libtmux-rs-dev
```

```console
$ directory=$(mktemp -d /tmp/libtmux-rs-dev/readme.XXXXXX)
```

```console
$ cargo run -- "$directory/socket"
```

It prints `created and cleaned one session`. The program kills the session it
created even if observing its pane fails.

## Confirm ordinary subprocess cleanup

[`Server::close_ordinary`] stops ordinary subprocess admission across every
clone and waits up to one second per call. Admitted requests keep their
existing deadlines and cancellation tokens. A successful close confirms that
their children have been reaped; it does not kill the tmux daemon.

After [`libtmux_core::Error::CleanupPending`], call it again to check completion.
The first cleanup failure is retained in a bounded
[`libtmux_core::Error::Shutdown`] summary, including after the original request
has returned. New ordinary requests return [`libtmux_core::Error::Closed`].

Persistent control clients keep independent lifetimes and require their own
shutdown. New attachments use ordinary subprocesses for qualification, which
cannot complete after close. Dropping the server does not wait for cleanup;
admitted workers keep ownership until reaping.

## Read pane bytes or share its key

[`Server::capture`] takes the observed pane key and returns the viewport as
`Vec<u8>`. Write those bytes with `std::io::Write::write_all` to preserve them.
A key from another socket or a replaced daemon is refused.

The complete example on [`Server::capture`] takes an existing socket and
pane ID, observes the key and writes exact bytes without Tokio or mutation.
Save it as `src/main.rs` and pass the socket and pane to read:

```console
$ cargo run -- /absolute/path/to/socket '%0'
```

The [mixed-client program](https://libtmux.org/en/rs/latest/reference/libtmux_blocking-server-new_session/)
creates a session with this client and passes its pane key directly to a Tokio
capture on the same socket. Use the `libtmux` facade for both namespaces.
The [runtime architecture decision](https://github.com/libtmux/libtmux-rs/pull/186)
describes the shared operations and separate driver lifetimes.

[Changelog](CHANGELOG.md)
