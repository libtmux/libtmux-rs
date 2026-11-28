# check-example-tables

Fails when a block of program output pasted into a README no longer matches
the example that prints it. A `<!-- example-output: NAME ARGS -->` comment
above a `text` fence names the example. The script runs it with `cargo run
--example` and compares every column except the wall clock, whose shape it
checks instead.

`just example-tables` checks the Tokio README. CI includes it in `just check`.

```console
$ uv run scripts/check-example-tables/check_example_tables.py crates/libtmux-tokio/README.md
```

Run its tests, which need no cargo:

```console
$ uv run --project scripts pytest scripts/check-example-tables
```
