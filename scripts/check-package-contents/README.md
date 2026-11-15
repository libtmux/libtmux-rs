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

Unlike the shell script it replaces, it reports every problem rather than the
first. `just package` runs it after `cargo package`, and so does `just check`
in CI.

```console
$ uv run scripts/check-package-contents/check_package_contents.py
```

Run its tests, which build small trees and break each rule:

```console
$ uv run pytest scripts/check-package-contents
```
