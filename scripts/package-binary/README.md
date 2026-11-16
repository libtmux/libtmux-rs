# package-binary

Packages a built command for a GitHub release, in the layout
`cargo binstall` fetches through each crate's `[package.metadata.binstall]`:
`<package>-<version>-<target>.tar.gz`, holding
`<package>-<version>-<target>/` with the command, the crate's `README.md`,
`LICENSE-APACHE` and `LICENSE-MIT`.

The `build` job of the [`binaries`](../../.github/workflows/binaries.yml)
workflow runs it for each package and target, after `cargo build --release`.
It appends `name=<package>-<version>-<target>` to `$GITHUB_OUTPUT`, which the
upload step and the artifact name read. An empty version reads the crate's
version from `cargo metadata`, as a pull request build does. A package that
`cargo metadata` does not list, a missing command or a missing licence exits 1
and names it.

```console
$ uv run scripts/package-binary/package_binary.py \
    tmux-workspace \
    x86_64-unknown-linux-musl
```

Run its tests:

```console
$ uv run pytest scripts/package-binary
```
