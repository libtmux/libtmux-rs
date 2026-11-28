# libtmux-blocking

Drive tmux synchronously against an explicit Unix socket. The client uses
native pipes and deadlines. Its normal dependency graph contains no Tokio.

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
