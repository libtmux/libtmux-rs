# public-api

Prints a crate's public API surface from rustdoc's JSON, one record per
line, sorted so the output diffs. Item signatures and non-blanket trait
implementations appear in it, so a change a caller can see changes the file.
It is not a semver check: it says what moved.

`just api` records `docs/public-api.txt` in `libtmux-core`, `libtmux-blocking`,
`libtmux-tokio` and `libtmux`. `just api-check`, in the `public API surface` CI
job, checks all four and fails with a diff when a committed file differs.
Both need a nightly toolchain for rustdoc's JSON output.

External re-exports record their source crate and path, including facade
globs and module aliases. The defining crate's inventory records its members
and signatures.

JSON includes hidden modules so visible root re-exports retain their native
declarations. The ledger follows visible exports and types used by their public
signatures. Hidden helpers and explicitly hidden items are excluded.

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
$ uv run scripts/public-api/public_api.py target/doc/libtmux.json \
    > crates/libtmux/docs/public-api.txt
```

Run its tests:

```console
$ uv run --project scripts pytest scripts/public-api
```
