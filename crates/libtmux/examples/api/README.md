# Complete API programs

Run one task from its imports through daemon cleanup. Each `api_*.rs` file
contains its own entrypoint and error handling; no Rustdoc setup or example
helper is added before execution.

You need a Unix environment, tmux 3.2a or newer, and the Rust toolchain in
the repository's `rust-toolchain.toml`. Each program creates a private socket
under `/tmp/libtmux-rs-dev/` and reads `/dev/null` as its tmux configuration.
It does not connect to the default server or the server named by `$TMUX`.

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

`send_keys` sends literal text without Enter. `send_key_names` interprets
tmux key names such as `Enter`. `send_line` submits literal text and Enter
in one dispatch. The input and capture programs wait for the shell prompt
before sending anything and compare complete output lines, so the echoed
command cannot satisfy the output check.

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

To try another task, copy its complete `api_*.rs` file to `src/main.rs`.
The same Cargo and toolchain files apply to every program. The consumer
declares its Tokio and tempfile dependencies explicitly and uses the
library's default query feature; it does not enable `test-support`.

## Results and cleanup

Successful output goes to stdout. It reports the task's result rather than
process IDs or socket paths. Errors go to stderr through Rust's `Result`
entrypoint. These lines teach the examples; they are not a versioned output
format for other applications.

| Exit status | Meaning |
| --- | --- |
| `0` | The task's result checks and cleanup succeeded. |
| nonzero | Setup, a tmux operation, a result check, the deadline or cleanup failed. |

Each task has a ten-second deadline, and commands have a five-second
timeout. Cancelling a task does not skip cleanup. The program calls
`Server::kill` to stop only its own daemon, then `Server::shutdown` to close
the client executor. It reports both task and cleanup errors. If either
cleanup operation fails, the diagnostic names the retained directory for
inspection; otherwise it removes the directory even when the task failed.

## Source mapping

`manifest.json` names the complete source files, their expected output and
the public API declarations that they teach. Its targets are checked against
the native `public-api.txt` inventory. No declaration or example is inferred
from a file's name alone.

Validate that mapping and the complete file boundaries:

```console
$ just api-examples
```

The ordinary `just examples` gate also executes every program. This source
gate is separate from the documentation site's exact copied-file, native
consumer and rendered clipboard receipts.
