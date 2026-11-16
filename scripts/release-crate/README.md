# release-crate

Reads a crate release out of its tag, and attaches the packaged crate to the
GitHub release. The `publish` job of the
[`Release`](../../.github/workflows/release.yml) workflow runs both, around
`cargo package` and `cargo publish`.

A release tag names one crate, `<crate>@v<version>`.

`target` splits the tag at its last `@v` and exits 1 unless the workspace has a
member of that name at that version, so a tag naming a version the manifest
does not carry cannot publish whatever the manifest says. It appends `package`,
`version` and `crate_file` (`target/package/<package>-<version>.crate`) to
`$GITHUB_OUTPUT`.

`attach` uploads the crate to the release of the tag and creates the release
first when it does not exist, so a retry after a partial publish finds the
release the first attempt made. A version with a `-` suffix is a semver
prerelease and gets a prerelease. It needs `gh`, `GH_TOKEN` and
`GITHUB_REPOSITORY`.

```console
$ uv run scripts/release-crate/release_crate.py \
    target \
    libtmux@v0.1.0
```

Attach a packaged crate to the release of its tag:

```console
$ uv run scripts/release-crate/release_crate.py \
    attach \
    libtmux@v0.1.0 \
    libtmux \
    0.1.0 \
    target/package/libtmux-0.1.0.crate
```

Run its tests:

```console
$ uv run pytest scripts/release-crate
```
