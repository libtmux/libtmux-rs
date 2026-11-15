# sandbox

Keeps tests, examples and benches off the developer's own tmux server. A tmux
command that names no socket follows an inherited `TMUX` to the server its
caller is inside, and otherwise lands on the default socket, so a suite
started from a tmux pane could drive or `kill-server` the session it came from.
[CONTRIBUTING.md](../../.github/CONTRIBUTING.md#nothing-selects-a-server)
gives the reasoning and the exit statuses.

| File | What it does |
| --- | --- |
| `sandbox_runner.py` | The Cargo target runner in `.cargo/config.toml`. It runs one binary with no inherited server and a private `TMUX_TMPDIR`, then stops any server the binary left there and removes the directory. |
| `sandbox_outside.py` | Looks, and only looks, at the default sockets a bare client would use outside the sandbox. Shared by the other two. |
| `check_doctest_sandbox.py` | Runs the doctest canary through the runner and through `[env]` alone, then breaks the sandbox one way at a time. Each broken run must fail, name what is wrong and print no value. `just doctest-sandbox` runs it. |

Cargo runs `sandbox_runner.py` through `uv run --script` for every binary, so
`uv` must be on `PATH`. It removes the `PATH` entry and the variables `uv`
adds, so the binary sees the caller's environment.

Run the check:

```console
$ uv run scripts/sandbox/check_doctest_sandbox.py
```

Run the runner by hand around a command:

```console
$ scripts/sandbox/sandbox_runner.py sh -c 'echo $TMUX_TMPDIR'
```

Run the tests here. `crates/libtmux/tests/sandbox_runner.rs` runs the runner
around real tmux servers, and `sandbox_canary.rs` asserts what it did:

```console
$ uv run pytest scripts/sandbox
```
