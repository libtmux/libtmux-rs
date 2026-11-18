# tmux-format-compat

Builds each pinned tmux release from source and runs the workspace against it.
tmux's output and error wording change between releases, so a lane proves
either that the whole workspace passes on a release, or that the crate refuses
one it does not support before any command reaches it.

| File | What it does |
| --- | --- |
| `tmux_format_compat.py` | The run: preflight, then one supervised worker that builds and tests each lane. |
| `compat_lanes.py` | Reads `.github/ci-matrix.txt`, selects lanes, orders the builds a lane needs and resolves a ref the manifest does not hold. |
| `tmux_format_compat_supervisor.py` | Contains every process the worker starts, on Linux with pidfd, and removes the build directory under `/tmp` afterwards. |
| `tests/` | The lane tests, a whole run against a stand-in tmux and `cargo`, and the supervisor cases. |

The manifest pins each lane by the commit its tag must name, and the build
checks the tag is annotated, peels to that commit and that the installed
binary reports the lane's version. A lane the manifest does not hold, named in
`LIBTMUX_COMPAT_LANES`, is resolved to a commit and built as a development
build. The `compat` CI job takes its lanes from `scripts/ci-plan`. The shared
`build-tmux` action pins a release by the SHA256 of its archive and installs a
binary only, so it does not keep these commit pins, and the `format-catalog`
lane needs the source tree.

The 3.8 lane also builds 3.3a and runs core and MCP capability discovery with
the newer client against both daemons. This pins connected-daemon decisions,
control-connection binding and preservation of mutation admission after a
socket replacement.

`just compat` runs it. It is slow, builds every lane it is given and needs
Linux, a `C.UTF-8` locale and `uv`.

```console
$ LIBTMUX_COMPAT_LANES="3.3a 3.8" just compat
```

Run its tests. The supervisor cases kill, stop and fork processes, so each runs
in a process of its own:

```console
$ uv run pytest scripts/tmux-format-compat
```
