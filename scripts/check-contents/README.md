# check-contents

Fails when a document's `## Contents` list and its sections disagree. A list
is a copy of the headings, so a renamed or added section leaves it linking to
an anchor that no longer exists. The script compares the two in order, checks
each link against the anchor GitHub gives the heading, and checks an indented
entry against the `###` subsections of its section.

`just doc-contents` runs it on `design.md` and `findings.md`, and so does
`just check` in CI.

```console
$ uv run scripts/check-contents/check_contents.py \
    crates/libtmux/docs/design.md \
    crates/libtmux/docs/findings.md
```

Run its tests, which break a document one way at a time:

```console
$ uv run --project scripts pytest scripts/check-contents
```
