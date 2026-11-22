# check-package-contents

Verifies what the four published crates contain, from `cargo package --list`.
It fails when:

- a file a crate's front page or tests depend on does not ship;
- a sandbox test ships, since a package has no `.cargo/config.toml` and so no
  runner for it to assert;
- a shipped document tells a reader to depend on a version other than the one
  in this tree, or a relative link in one points at a file that does not ship;
- `libtmux` carries macro source, `libtmux-macros` depends on `libtmux`, or
  `libtmux` without its default features depends on an optional crate.

It reports every problem, not the first. `just package` runs it with `--build`,
which packages the four crates into a fresh target directory first, and so does
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
