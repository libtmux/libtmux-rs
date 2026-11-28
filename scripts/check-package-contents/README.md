# check-package-contents

Verifies what the seven published crates contain, from `cargo package --list`.
It fails when:

- a file a crate's front page or tests depend on does not ship;
- a sandbox test ships, since a package has no `.cargo/config.toml` and so no
  runner for it to assert;
- a shipped document tells a reader to depend on a version other than the one
  in this tree, or a relative link in one points at a file that does not ship;
- `libtmux` carries macro source, `libtmux-macros` depends on `libtmux`, or
  `libtmux` without its default features depends on an optional crate.

It reports every problem, not the first. `just package` runs it with `--build`,
which packages the seven crates into a fresh target directory first, and so does
`just check` in CI. Cargo treats staged registry versions as immutable, so a
fresh target keeps a same-version edit from reusing source an earlier run
extracted.

```console
$ uv run scripts/check-package-contents/check_package_contents.py \
    --build
```

Run its tests, which build small trees and break each rule:

```console
$ uv run --project scripts pytest scripts/check-package-contents
```

## Installed clients

`installed_clients.py` builds external consumers from the seven packaged
archives. It requires exact sibling versions and checks that every native
dependency resolves to its extracted archive. The pure, blocking,
blocking-control, Tokio and mixed configurations each run separately.
The blocking configurations compile and run the complete displayed program
from the staged facade README, with its default features in the blocking
lane. The report retains the program's digest and checks its stdout and
owned-endpoint cleanup.

The ordinary Tokio consumer comes from its archived README and lists the
sessions at a supplied endpoint. Its fixture is built in a separate Cargo
project. Resolved feature trees must keep `test-support` out of every public
reader, including the mixed consumer.

Seven Tokio programs use the guide's consumer manifest and the sources in the
Tokio archive. The proof records each program's digest, stdout bytes and
cleanup result so the docs job can compare them with the examples copied
from its HTML and Markdown.

The mixed program is copied from the facade archive. The pinned bare
recorder from `libtmux/docs` verifies its raw stdout against native captures
of the same pane and generation. The caller creates and retires the fixture;
the reader removes only its own verified session. The proof checks the
borrowed session identities before and after the reader. Separate runs
exercise a name conflict, a failing creation hook, a failed read, and
independent read and cleanup failures. The report retains those outcomes,
the resolved reader features and the source invocation's digest.

Both apps are installed from their archives with an explicit Cargo registry
patch configuration. On a library-owned fixture, the installed
`tmux-workspace` emits a JSON load result and the installed `tmux-mcp` serves
JSON-RPC over stdio, inspects that workspace and captures its keeper pane.
It links that pane's window into the workspace, saves the listed placement,
withdraws it and requires `select_window` to refuse the saved reference. The
surviving link and session focus must stay unchanged. The MCP process enables
`select_window` alongside `inspect`; kill tools stay disabled.
The app protocol transcript and native pane witness are recorded separately.

The `native-clients` CI job checks out the recorder at the revision named by
`RECORDER_REVISION`, then retains `proof.json`, the original archives and
recorder output in the `native-clients-<source SHA>` artefact. The report
includes source, archive, program, recorder and installed-binary hashes.
Start from a clean checkout. The app installs keep the external versions and
checksums from each archived `Cargo.lock`. The MCP check reads its advertised
capabilities, requires conservative whole-call hints on both discovery
surfaces, and checks that their direct capability axes agree. It calls a
disabled tool to verify its retry evidence.

With that exact docs revision checked out beside this repository:

```console
$ uv run scripts/check-package-contents/installed_clients.py \
    --recorder ../docs/scripts/example_tmux_sandbox.py \
    --output-dir artifacts/native-clients
```

The script creates a fresh Cargo target for every run. Its unit tests use
small archives and pipes and do not build Rust packages.
