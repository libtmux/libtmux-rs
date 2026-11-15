# example-coverage

Counts the crate-root types of `libtmux` that carry a runnable example in
their own documentation. A type with well-documented methods and no example
of its own leaves a reader who arrived from a search with nothing to copy.
`--require-all` fails when any type lacks one.

`just example-coverage` prints the count and `just example-coverage-check`
fails on a gap. The `public API surface` CI job runs the check. Both read
rustdoc's JSON, which needs a nightly toolchain.

```console
$ cargo +nightly rustdoc \
    --package libtmux \
    --all-features \
    -- \
    -Zunstable-options \
    --output-format json
```

```console
$ uv run scripts/example-coverage/example_coverage.py \
    --require-all \
    target/doc/libtmux.json
```

Run its tests:

```console
$ uv run pytest scripts/example-coverage
```
