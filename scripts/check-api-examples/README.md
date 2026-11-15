# check-api-examples

Checks the complete API programs in `crates/libtmux/examples/` against the
recorded public API. It reads `crates/libtmux/examples/api/manifest.json` and
fails on a missing, duplicate or unknown native target, a program that hides a
source helper, a truncated source file, or a manifest that disagrees with the
programs on disk.

`just api-examples` runs it, and so does `just check` in CI. The documentation
build reads the payload that `--output` writes.

```console
$ uv run scripts/check-api-examples/check_api_examples.py
```

Write the exact source payload and hashes:

```console
$ uv run scripts/check-api-examples/check_api_examples.py --output payload.json
```

Run its tests:

```console
$ uv run pytest scripts/check-api-examples
```
