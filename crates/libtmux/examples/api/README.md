# Complete API programs

Run one task from its imports through daemon cleanup. Each `api_*.rs` file
contains its own entrypoint and error handling; no Rustdoc setup or example
helper is added before execution.

You need a Unix environment, tmux 3.3a or newer, and the Rust toolchain in the
repository's `rust-toolchain.toml`. Each program starts a private server of
its own:

- Its socket is under `/tmp/libtmux-rs-dev/`, and its configuration is
  `/dev/null`.
- It never connects to the default server or the one `$TMUX` names.

## Run a source example

From the repository root, construct a handle and start its private daemon:

```console
$ cargo run --locked -p libtmux --example api_construction
```

List sessions, windows and panes, then inspect their relations:

```console
$ cargo run --locked -p libtmux --example api_listings
```

Create windows and split from both a window and a pane:

```console
$ cargo run --locked -p libtmux --example api_creation
```

Filter a session snapshot with a typed field expression:

```console
$ cargo run --locked -p libtmux --example api_query
```

Handle missing, unique and ambiguous matches:

```console
$ cargo run --locked -p libtmux --example api_cardinality
```

Send literal text and then a named Enter key:

```console
$ cargo run --locked -p libtmux --example api_input
```

Submit a shell command and capture its complete output lines:

```console
$ cargo run --locked -p libtmux --example api_capture
```

- `send_keys` sends literal text without Enter.
- `send_key_names` interprets tmux key names such as `Enter`.
- `send_line` submits literal text and Enter in one dispatch.

The input and capture programs wait for the shell prompt before sending, and
compare complete output lines, so the echoed command cannot satisfy the check.

## Run as a separate consumer

Place a checkout of this repository at `libtmux-source` in an otherwise
empty working directory. Use the source revision recorded with the example
being read; the API documentation pins that checkout and its source links.

Copy the complete setup files and construction program, then run it:

```console
$ mkdir src &&
  cp libtmux-source/crates/libtmux/examples/api/consumer.toml Cargo.toml &&
  cp libtmux-source/rust-toolchain.toml rust-toolchain.toml &&
  cp libtmux-source/crates/libtmux/examples/api_construction.rs src/main.rs &&
  cargo run --quiet
```

To try another task, copy its `api_*.rs` file to `src/main.rs`. The Cargo and
toolchain files serve every program. The consumer declares Tokio and tempfile,
and does not enable `test-support`.

## Results and cleanup

Success goes to stdout, as the task's result rather than process IDs or socket
paths. Errors go to stderr through Rust's `Result` entrypoint. The output
teaches the examples; it is not a format for other programs to parse.

| Exit status | Meaning |
| --- | --- |
| `0` | The task's result checks and cleanup succeeded. |
| nonzero | Setup, a tmux operation, a result check, the deadline or cleanup failed. |

- **Deadlines.** A task has ten seconds, and each command five.
- **Cleanup.** Cancelling a task does not skip it. The program calls
  `Server::kill` to stop only its own daemon, then `Server::shutdown` to close
  the client executor, and reports task and cleanup errors alike.
- **Leftovers.** If cleanup fails, the diagnostic names the retained
  directory. Otherwise the directory is removed, even when the task failed.

## Source mapping

`manifest.json` names each program, its expected output and the public API it
teaches. Check the mapping against the public API inventory:

```console
$ just api-examples
```

`just examples` also runs every program.
