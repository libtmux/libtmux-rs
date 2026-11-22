# Writing

Repository-specific rules for `README.md`, `CHANGELOG.md`, release notes,
commit messages, CLI and help text, error messages, `rustdoc`, and source
comments in `libtmux-rs`.

Shared organization voice, CommonMark wrapping, code-fence formatting,
source-comment gates, slop prevention, and commit conventions live in
[`libtmux/.github` `WRITING.md`][org-writing]. Building, testing, gates, and
pull requests live in [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Terminology and spelling

- Headings are sentence case: `## Platform support`, not `## Platform Support`.
- `tmux` is lowercase everywhere, including at the start of a sentence —
  rewrite the sentence instead.
- Crate names are as published and in backticks: `libtmux`, `libtmux-macros`,
  `tmux-mcp`, `tmux-workspace`. The repository is `libtmux-rs`; the crate is
  `libtmux`. They are not interchangeable.
- Write `prerelease`, not `pre-release`. Write `changelog`, not `change log`.
- Rust is capitalised; `rustdoc`, `rustfmt`, `clippy`, and `cargo` are not,
  except at the start of a sentence.
- Prose here uses British spelling — `behaviour`, `catalogued`, `artefact`.
  Identifiers and `rustdoc` follow Rust's own conventions, which are American:
  `color`, `initialize`. Do not change one to match the other.
- Use the same verb for the same operation everywhere. `kill` is what tmux
  calls it, so it is what we call it; not "delete", "remove", or "close".
- Keep `Note:`, `Important:`, and `Warning:` out of prose; reserve an
  admonition for something genuinely destructive.
- Give measurements with their conditions and reproduction command rather than
  adjectives, and never claim speed from the implementation language.

## README shape

Both the root `README.md` and each crate's `README.md` follow this shape:

- **Header.** The logo, a centred title, and a badge row: CI, published crates,
  the `libtmux.org` docs, MSRV, and license. Link only pages returning 200.
- **First sentence and boundaries.** Say what the crate does without backstory
  (project history belongs in `crates/libtmux/docs/design.md`), put concepts
  before switches, and say who it is not for while naming the alternative.
- **Tasks and linked APIs.** A `##` heading names what the reader came to do; a
  `###` heading links an API name to its `libtmux.org` reference page, followed
  by a colon and its job in plain words, then linked sibling APIs, the example,
  what it prints, at most one short note, and a `More:` link to the guide.
- **Short paragraphs, no inventories.** Keep paragraphs to three lines or
  fewer, turn lists of facts into bullets, and omit feature inventories,
  toolchain patch versions, CI matrix logs, and gate boilerplate.
- **Executable examples.** Rust examples on the five wired README pages (listed
  in [`CONTRIBUTING.md`](CONTRIBUTING.md)) are compiled by `cargo test --doc`,
  and longer programs live in `examples/` where `just examples` runs them.
  Examples start with their imports and own a private server through
  `TestServer`, never shell `tmux`, `tmux kill-server`, or `rm`.
- **Headline features.** Typed queries, the planner and `Planner::explain`,
  chaining, and control mode each get a section with a linked heading and an
  example. Describe none the crate lacks.
- **Packages and closing sections.** Keep the package table and per-crate
  headings linking trunk directories, API reference, registry, and changelog,
  and close with short bold-lead bullets under `What it leaves out` and
  `Support and status` (`tmux 3.3a or newer`, `Rust 1.85 or newer`).

The version warning is worded the same in every libtmux port. Do not reword it:

> Releases are numbered `0.1.N`, and each release increments `N`. The API is not
> settled: any release may change or remove exported identifiers, and the
> changelog marks each break. Pin an exact version to avoid one.

## API documentation

`rustdoc` is the reference. The [Rust API Guidelines][api-guidelines] are the
baseline; what follows is where this repository is more specific.

**The first sentence stands alone.** `rustdoc` renders it as the summary in
search results and in module listings, away from everything after it. Write a
noun and its semantic distinction, not a restatement of the name.

```rust
/// A tmux target that has been resolved to an ID and cannot go stale.
```

Not `/// This struct is used for holding information about a target.`

**Document the contract, not the implementation.** What a caller may rely on:
what is returned, what is refused, ordering, whether it blocks, what happens
when the object is gone, which tmux release it needs. A comment describing the
channel inside a type is not documentation.

**Sections, when they say something.**

- `# Examples` on every crate-root type. This is enforced, though the check is
  cruder than the rule: it accepts any fenced block in the type's own
  documentation, so the heading is the convention and the block is the gate.
  See [`CONTRIBUTING.md`](CONTRIBUTING.md).
- `# Errors` on anything returning `Result`, naming the conditions rather than
  restating that it can fail.
- `# Panics` where a caller could trip one. Where nothing can, "Never panics"
  is worth writing: it is a promise, and inference is not.
- `# Cancel safety` on an `async fn` whose future, dropped partway, leaves
  something a caller has to know about. Say which of three it is: nothing
  happened; the effect may or may not have happened, so a retry can repeat it;
  or something is left held -- a lock, a queue position, a half-sent input.
  The crate-wide model is in the README's "Cancellation and shutdown": dropping
  a dispatch signals its process group and reaps it. A method that adds nothing
  to that model needs no section; one that does needs one, because a caller
  racing it against a timeout or a `select!` cannot read the answer off the
  signature.
- No `# Safety`. `unsafe_code` is `forbid` at the workspace level, so there is
  no unsafe code here to document. If that ever changes, the section states
  the proof obligation the caller must uphold, and the reason it holds.

**Examples use `?`, not `unwrap()`.** `clippy` denies `unwrap_used`,
`expect_used`, and `panic` outside tests, and an example teaches whatever it
shows. Hide the setup with `#` so the example stays short and still compiles:

```rust
/// ```no_run
/// # async fn example(server: &libtmux::Server) -> Result<(), libtmux::Error> {
/// use libtmux::AccessMode;
///
/// server.grant_access("observer", AccessMode::ReadOnly).await?;
/// # Ok(())
/// # }
/// ```
```

The hidden `async fn example` wrapper is the shape used throughout this crate.
Use `no_run` when the example needs a server it does not start, and a real
`TestServer` when the behavior is the point.

**Link with intra-doc links.** `[`Server::hierarchy`]` over "see the hierarchy
method below" — the link survives a rename and a reader can follow it.

**Document invariants on the type that holds them.** "Offsets always fall on
UTF-8 code point boundaries" is worth more than ten paragraphs about methods.

**A doc comment belongs to the item below it.** `rustdoc` treats it as an
attribute and never checks that the prose describes that item, so a block
inserted one line too high silently hands a type its neighbour's summary.
Never insert a doc block without landing above the whole of the previous
item's comment. `just doc-blocks` catches the shape; it cannot catch the
meaning.

## Source comments

Source comments answer to the shared Loss, Ceiling, and Upkeep gates in
[`libtmux/.github` `WRITING.md`][org-writing]: one or two lines stating an
invariant, upstream tmux quirk, protocol constraint, lifetime or concurrency
rule, or why code that looks wrong is intentional, with deliberation kept in
the commit message.

Upkeep reaches values tracking our own code (`// There are 321 tests...`), not
frozen external facts:

```rust
// tmux < 3.2 reports the pane ID only after the command completes,
// so this query must stay separate.
```

`rustdoc` `///` comments, `# Examples` doctests, and `# Errors` and `# Panics`
sections are exempt from the loss gate because they serve the caller, while
keeping the same brevity and upkeep bar.

## Changelog

Each published crate keeps its own `CHANGELOG.md` beside its `Cargo.toml`
(`crates/libtmux/CHANGELOG.md`, `crates/libtmux-macros/CHANGELOG.md`,
`crates/tmux-mcp/CHANGELOG.md`, `crates/tmux-workspace/CHANGELOG.md`), and an
entry lives in the single file of the crate it changes. The root
`CHANGELOG.md` holds cross-crate changes, documentation, CI, tests, release
tooling, and each release's opening version summary.

- Use [Keep a Changelog][keep-a-changelog] sections (`### Added`,
  `### Changed`, `### Fixed`, `### Removed`) under `## <version> - <date>`,
  with unreleased work under `## Unreleased`.
- Prefix crate-only releases as `## tmux-mcp 0.1.0-alpha.4 - 2026-08-14` (kept
  in `tmux-mcp`'s file and the root's only). Every crate file carries each
  workspace release heading and writes ``No entries for `libtmux-macros`.``
  when unchanged.
- Prefix breaking `0.1.N` bullets with `**Breaking.**` and give the migration
  in the same bullet.
- Lead with the literal identifier (`Pane::capture_lines`, `LIBTMUX_TOOLSETS`,
  `tmux://capabilities`) and a concrete verb from the caller's side; record an
  MSRV raise with both versions.
- Use absolute links inside crate changelogs because `just package` refuses
  links to files the crate tarball does not ship.
- GitHub release notes brief upgraders on breaking changes, new capability, and
  measured effects without emoji, linking the changelog rather than pasting it.

## Commit messages

Follow the `Scope(type[detail]): concise description` format (50-character
subject excluding `(#NN)`, 72-character body lines, blank line between `why:`
and `what:`) in [`libtmux/.github` `WRITING.md`][org-writing].

- Scopes name a subsystem (`Pane`, `ControlMode`, `tmux-mcp`), not a filename.
- Types in use include `feat`, `fix`, `refactor`, `docs`, `chore`, `test`,
  `style`, `rs(deps)`, `rs(deps[dev])`, `rs(deps[toolchain])`, `ci(deps)`, and
  `ai(rules[AGENTS])`.
- Release commits use `Tag v<version>` rather than `Scope(type[detail]):`. A
  tag publishes to crates.io and cannot be replaced; push one tag at a time per
  [`CONTRIBUTING.md`](CONTRIBUTING.md).

## CLI and help text

`tmux-mcp` and the example binaries are interfaces, and an interface a script
or an agent drives has a contract. Where this repository documents a binary,
it documents:

- **Exit status**, as a table, one row per status with what it means. A caller
  that treats every nonzero status alike is a caller the documentation failed.
- **Streams.** What goes to stdout, what goes to stderr, and what changes when
  a machine-readable mode is selected.
- **Configuration precedence**, written out in order rather than left to be
  reverse-engineered — built-in defaults, then configuration, then environment,
  then arguments.
- **What is stable.** Human-readable output may change; a machine-readable
  format is a contract. Say which is which, so terminal presentation stays
  free to improve.
- **Destructive behavior**, stated as an invariant rather than a warning.
  "`kill_session` is unavailable outside `teardown` and refuses a target that
  may contain the MCP caller" beats "be careful."

An error message answers three questions in order: what failed, why, and what
the reader can do. `Error::kind` exists so a caller can branch on the answer;
the message exists so a person can act on it.

Defaults belong in the help text, next to the flag, not in prose a reader has
to hunt through.

## Markdown and directives

Follow the CommonMark, 80-column repository wrapping, reference-link, and
paste-and-run `console` fence rules in
[`libtmux/.github` `WRITING.md`][org-writing] (no `> [!NOTE]` or `> [!WARNING]`
alerts in tracked `.md` files, and no hard-wrapping in issue or pull request
bodies).

- Doctests and executed examples are exempt from single-command fence
  splitting because the test suite runs them.
- Never reformat directive comments: `#![cfg_attr]`, `#[allow]`, the `region:`
  markers the documentation site slices examples between, and the
  `<!-- example-output: -->` markers `just example-tables` runs.
- Preserve every comment documenting an invariant, protocol constraint,
  platform quirk, or upstream workaround.

[org-writing]: https://github.com/libtmux/.github/blob/main/WRITING.md
[api-guidelines]: https://rust-lang.github.io/api-guidelines/documentation.html
[keep-a-changelog]: https://keepachangelog.com/en/1.1.0/
