# public-api

Prints the public API surface of `libtmux` from rustdoc's JSON, one record per
line, sorted so the output diffs. Item signatures and non-blanket trait
implementations appear in it, so a change a caller can see changes the file.
It is not a semver check: it says what moved.

`just api` writes `crates/libtmux/docs/public-api.txt` with it, and `just
api-check`, in the `public API surface` CI job, fails when the committed file
differs. Both need a nightly toolchain for rustdoc's JSON output.

```console
$ cargo +nightly rustdoc \
    --package libtmux \
    --all-features \
    -- \
    -Zunstable-options \
    --output-format json
```

```console
$ uv run scripts/public-api/public_api.py target/doc/libtmux.json \
    > crates/libtmux/docs/public-api.txt
```

Run its tests:

```console
$ uv run pytest scripts/public-api
```
