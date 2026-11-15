# check-example-tables

Fails when a block of program output pasted into a README no longer matches
the example that prints it. A `<!-- example-output: NAME ARGS -->` comment
above a `text` fence names the example. The script runs it with `cargo run
--example` and compares every column except the wall clock, whose shape it
checks instead.

`just example-tables` runs it on `README.md`, and so does `just check` in CI.

```console
$ uv run scripts/check-example-tables/check_example_tables.py README.md
```

Run its tests, which need no cargo:

```console
$ uv run pytest scripts/check-example-tables
```
