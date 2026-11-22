# check-changelogs

Fails when a published crate has no changelog that it ships and links. Each
crate keeps a `CHANGELOG.md` beside its `Cargo.toml` that opens with the
crate's name, has an Unreleased section, appears in the crate's `include` list
and is linked from the crate's README.

`just changelogs` runs it, and so does `just check` in CI. The packaged
tarball is checked separately by `scripts/check-package-contents`.

```console
$ uv run scripts/check-changelogs/check_changelogs.py
```

Run its tests, which break a copy of the crates one way at a time:

```console
$ uv run --project scripts pytest scripts/check-changelogs
```
