# example-coverage

Counts the crate-root types that carry a runnable example in
their own documentation. A type with well-documented methods and no example
of its own leaves a reader who arrived from a search with nothing to copy.
`--require-all` fails when any type lacks one.

`just example-coverage` prints the count and `just example-coverage-check`
fails on a gap in `libtmux-core`, `libtmux-blocking`, `libtmux-tokio` or
`libtmux`. The `public API surface` CI job runs the check. Both read rustdoc's
JSON, which needs a nightly toolchain.

Hidden modules remain in the JSON so their visible root re-exports are counted.
A type with its own `#[doc(hidden)]` attribute is excluded.

```console
$ cargo +nightly rustdoc \
    --package libtmux \
    --all-features \
    -- \
    -Zunstable-options \
    --output-format json \
    --document-hidden-items
```

```console
$ uv run scripts/example-coverage/example_coverage.py \
    --require-all \
    target/doc/libtmux.json
```

Run its tests:

```console
$ uv run --project scripts pytest scripts/example-coverage
```
