# check-api-examples

Checks the complete API programs in `crates/libtmux-tokio/examples/` and
`crates/libtmux/examples/` against each target's defining crate inventory.
Both roots own a version 1 `api/manifest.json`; older revisions keep their
single `libtmux` manifest. A target such as
`libtmux_core::CreatedSession::pane` is checked in `libtmux-core`, even when
its program uses the `libtmux` facade.

The check fails on a missing, duplicate or unknown native target, a program
that hides a source helper, a truncated source file, or a manifest that
disagrees with the programs on disk. The mixed program may own its runtime
from a synchronous `main`; an asynchronous `main` needs `#[tokio::main]`.

Legacy and Tokio examples keep `cargo run --quiet`. The split facade's
external-fixture recipe must match its README shell block, with only the
checkout's Cargo invocation replaced by the standalone invocation.

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
$ uv run --project scripts pytest scripts/check-api-examples
```
