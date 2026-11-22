# fixture-root

Reports whatever the test suite left in the fixture root, `/tmp/libtmux-rs-test`.
tmux never unlinks its socket when the server exits, so whatever named one owns
removing it. `TestServer` does; a test that hand-rolls a socket path does not,
and the file it leaves is invisible until the root fills up.

The script decides by ownership, not by name or age. A directory whose `owner`
file names a living process is in use, and so is a socket that a server still
answers on. Anything else is a leak, and the script exits 1 and names it.
`scripts/run-examples` imports `abandoned` to ask the same question of
`/tmp/libtmux-rs-dev`.

`just fixture-root` runs it, and so does `just check` in CI. It takes another
root as its one argument.

```console
$ uv run scripts/fixture-root/fixture_root.py
```

Run its tests:

```console
$ uv run --project scripts pytest scripts/fixture-root
```
