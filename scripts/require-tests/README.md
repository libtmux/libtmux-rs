# require-tests

Runs a test command and fails when it ran no test. A `cargo test` name filter
that matches nothing exits 0 with `running 0 tests`, so a renamed test leaves
its lane green without running anything. The command's output passes through
as it is written. The wrapper returns the command's own exit status when that
is not zero, and 1 when the command passed without a test.

The `python-bridge` and `compat` CI jobs wrap their name-filtered `cargo test`
runs in it, and `scripts/sandbox` and `scripts/tmux-format-compat` import it.

```console
$ uv run scripts/require-tests/require_tests.py \
    cargo test \
    --package libtmux \
    --test daemon_version_check \
    real_tmux_compat_
```

Run its tests:

```console
$ uv run pytest scripts/require-tests
```
