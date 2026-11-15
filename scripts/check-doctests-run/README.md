# check-doctests-run

Fails when a doctest shows lines it never runs. rustdoc supplies `fn main`
only when the block has none, so a body that is a hidden `async fn example`
compiles and then runs an empty `main`, and `cargo test --doc` still counts a
pass. The script asks whether each visible line is reached, and fails on a
block with unreached lines, a block of definitions only, and a block that
hides its whole body. `no_run`, `ignore` and `compile_fail` blocks are exempt.

It reads Rust doc comments and the Markdown files `include_str!` pulls in.
`just doctests-run` runs it on `crates/`, and so does `just check` in CI.

```console
$ uv run scripts/check-doctests-run/check_doctests_run.py crates
```

Run its tests:

```console
$ uv run pytest scripts/check-doctests-run
```
