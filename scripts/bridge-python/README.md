# bridge-python

Names the two Python backends the `tmux-workspace` bridge tests run against.
The `python-bridge` job of the [`CI`](../../.github/workflows/ci.yml) workflow
makes a venv under `$RUNNER_TEMP/bridge-venv`, installs an obsolete tmuxp into
it, and checks out libtmux into `.bridge-python`. This script then:

- writes `$RUNNER_TEMP/bridge-python`, an executable wrapper that runs the
  venv's Python with `.bridge-python/src` on `PYTHONPATH`, so the tests see the
  checked-out libtmux where the obsolete one would otherwise load;
- appends `LIBTMUX_TEST_OBSOLETE_PYTHON` (the venv's Python) and
  `LIBTMUX_TEST_QUALIFIED_PYTHON` (the wrapper) to `$GITHUB_ENV`.

The wrapper is a shell script because an interpreter path cannot carry an
environment variable. It is generated in CI and never committed. It exits 1
when `RUNNER_TEMP` or `GITHUB_ENV` is unset.

```console
$ RUNNER_TEMP="$(mktemp -d)" \
    GITHUB_ENV="$(mktemp)" \
    uv run scripts/bridge-python/bridge_python.py
```

Run its tests:

```console
$ uv run --project scripts pytest scripts/bridge-python
```
