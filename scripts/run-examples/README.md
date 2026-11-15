# run-examples

Runs every shipped example to completion against a tmux server the script
owns, and fails when one exits nonzero, does not print what it exists to show,
or leaves a socket in `/tmp/libtmux-rs-dev`. `cargo test --all-targets`
compiles an example and never runs it, so an example that fails at runtime
passes every other gate.

`TABLE` in the script has one row for each example: its crate, features,
arguments, a driver for its standard input, the line it must print, and what
it needs. The script checks the table against `cargo metadata` in both
directions before it runs anything, so an added example with no row fails and
so does a row naming an example that is gone. A `pane` example reads `$TMUX`
as a process inside a pane does, and is handed the fixture server by name.
Before the table, the script runs `inspect` with every variable that selects a
server aimed at the fixture and requires that it finds none.

`just examples` runs it, and so does `just check` in CI.

```console
$ uv run scripts/run-examples/run_examples.py
```

Run its tests, which need no tmux server:

```console
$ uv run pytest scripts/run-examples
```
