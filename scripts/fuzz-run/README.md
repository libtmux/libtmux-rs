# fuzz-run

Fuzzes one cargo-fuzz target for a bounded time. The `fuzz` job of the
[`CI`](../../.github/workflows/ci.yml) workflow runs it for each target of its
matrix, on the weekly schedule and on a dispatch.

cargo-fuzz builds for the triple it was itself compiled for, and the installer
ships a musl binary. musl links libc statically, which a sanitizer cannot
instrument, so the script reads the nightly host triple from `rustc +nightly
-vV` and passes it as `--target`. It creates `fuzz/corpus/<target>` first,
because libFuzzer writes new inputs to the first corpus directory only, then
runs `cargo +nightly fuzz run` over that directory and `fuzz/seeds/<target>`
with a limit of 300 seconds and 4096 MiB of memory. Its exit status is
cargo-fuzz's.

```console
$ uv run scripts/fuzz-run/fuzz_run.py layout
```

Run its tests:

```console
$ uv run --project scripts pytest scripts/fuzz-run
```
