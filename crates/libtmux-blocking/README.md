# libtmux-blocking

Run tmux commands synchronously against an explicit Unix socket. The client
uses native pipes and deadlines. A blocking-only package has no Tokio runtime
dependency.

Create a detached session with `Server::new_session`. Its receipt contains
the session ID, initial window ID and a pane key tied to the observed daemon.
Pass the pane key to `Server::capture` to read the viewport, or to the Tokio
client connected to the same socket.

The [runtime architecture decision](https://github.com/libtmux/libtmux-rs/pull/186)
describes how the two clients share operation semantics.

[Changelog](CHANGELOG.md)
