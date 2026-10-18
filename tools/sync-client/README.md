# Blocking client generation

`libtmux-tokio` owns the canonical operation bodies. This tool translates
those bodies into checked-in blocking Rust in `libtmux`; consumers do not run
it and the published base crate has no generator dependency.

After changing a canonical operation, run:

```console
$ cargo run -p libtmux-sync-client
$ cargo run -p libtmux-sync-client -- --check
```

The generator removes async syntax through parsed source spans. It preserves
argument bytes, comments, and ordinary string literals. Scoped creation stays
lazy, and an async block's return and `?` boundary becomes a closure boundary.
Error derives become explicit `Display` and `Error` implementations. Rustdoc
examples and package names are translated with the code.

Execution, process ownership, cancellation, control actors, and thread races
need separate implementations. They are recorded in `source-ports.tsv`:

| Port | Check |
| --- | --- |
| `generated` | Output must equal a fresh translation. |
| `equal` | Both runtime-independent files must match byte for byte. |
| `manual` | A changed canonical digest requires review of the blocking counterpart. |
| `native-only` | The native runtime facade has an explicit blocking API mapping. |

Every native Rust source, integration test, example, and benchmark must have a
record. `blocking-sources.tsv` accounts for shared primitives and
implementation files that exist only in the base package. Manual integration
ports retain the native test names or an explicit
entry in `test-renames.tsv`. Inline tests split out of generated operations
use `test-ports.tsv`.
`doc-ports.tsv` records cancellation and execution contracts whose prose must
change with the runtime model.

Update a manual digest only after reviewing the changed behavior and its
blocking tests. The inventory catches missing ports and canonical drift; it
does not prove that a manually translated concurrency test exercises the same
race. Run the relevant live tests and retain deliberate-break evidence.

`--only=server.rs` limits regeneration to an operation module. `--seed=tests/commands.rs`
writes an initial translation of a registered manual surface and may replace
an existing port. Review its concurrency and cancellation before using it.
