# check-doc-blocks

Fails when a doc comment has been split across two items. rustdoc attaches a
doc comment to whatever follows it and never checks the two match, so a block
inserted one line too high gives an item its neighbour's summary. The script
reports a summary that opens mid-sentence, a `///` below a non-doc attribute,
and a summary of two sentences.

`just doc-blocks` runs it on `crates/`, and so does `just check` in CI. It
reads text only, so it needs no toolchain.

```console
$ uv run scripts/check-doc-blocks/check_doc_blocks.py crates
```

Run its tests:

```console
$ uv run pytest scripts/check-doc-blocks
```
