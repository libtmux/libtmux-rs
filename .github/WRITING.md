# Writing

How this repository writes, for humans and agents alike. It governs
`README.md`, `CHANGELOG.md`, release notes, commit messages, CLI and help
text, error messages, rustdoc, and source comments — every surface a reader
reaches without opening the code.

For building, testing, the gates, and pull requests, see
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Voice

Calm, technical, economical. The register to aim for is a good man page
written by someone who also cares about API design, not a product page.

The pattern that carries most sentences is **fact, then behavior, then escape
hatch**:

> Symlinks are not followed by default. Use `--follow` to traverse them.

Rules that follow from it:

- State the default explicitly. Never make a reader infer whether something is
  stable, experimental, platform-specific, or destructive.
- Describe behavior before implementation. A reader needs to know what happens
  before they need to know how.
- Document surprising behavior rather than leaving it to be discovered. Name
  sharp edges; do not hide them.
- Give measurements with their conditions, never adjectives. "20 MB versus
  about 100 bytes over two seconds against a flooding neighbour" is a claim; a
  reader can check it. "Much faster" is not.
- A performance number appears identically everywhere it appears, next to the
  command that reproduces it.
- Caveats increase credibility. The sentence that says what did not improve is
  the one that makes the rest believable.
- Prefer concrete nouns and active declarative sentences.
- Put identifiers, paths, environment variables, commands, and flags in
  backticks.
- Never claim speed from the implementation language.

### Diction

| Instead of | Prefer |
| --- | --- |
| "easy", "simple", "just" | omit |
| "obvious" | omit; if it were, you would not be writing it |
| "blazing fast", "high performance" | give the number and the conditions |
| "powerful", "flexible", "seamless" | state the capability |
| "robust" | name the failure that is handled |
| "comprehensive" | name what is covered |
| "production-ready" | state the guarantee |
| "optimized" | give the magnitude |
| "various fixes" | name the components |
| "under the hood" | omit unless a caller can observe it |
| "please note that" | state the fact |
| "leverage", "utilize" | "use" |
| "in order to" | "to" |
| "we added", "new and improved" | "`Foo` now …" |

### One voice per surface

| Surface | Voice |
| --- | --- |
| `README.md` | concise teacher |
| rustdoc | the contract a caller may rely on |
| `--help` | reference manual |
| `CHANGELOG.md` | ledger |
| GitHub release | upgrade briefing |
| error message | diagnosis |
| commit subject | the semantic delta |
| commit body | engineering rationale |
| source comment | what the code cannot say itself |

## README

The README is not the manual. It answers what this is, why you would use it,
what using it feels like, how to install it, and where everything else lives.

- Say what the crate does in the first sentence, without backstory. Project
  history belongs in `crates/libtmux/docs/design.md`.
- Put concepts before switches. The behavioral model teaches the reader; a
  flag dump does not. `--help` and the API documentation own the switches.
- Say who it is not for, and name the alternative. The paragraph that turns a
  reader away is worth more than the one that keeps the wrong reader.
- Compatibility is content, not an afterthought: supported tmux releases, the
  MSRV, and the platforms belong in the README under their own heading.
- A Rust example on a README is compiled by `cargo test --doc`, on the four
  pages wired in for it. Write examples that run; one that cannot compile is
  not an example. [`CONTRIBUTING.md`](CONTRIBUTING.md) lists which pages those
  are, and says that adding a README means wiring it in.
- Badges: CI, the published crates, MSRV, license. That is the set.

The alpha warning is worded the same in every libtmux port. Do not reword it.

## API documentation

rustdoc is the reference. The [Rust API Guidelines][api-guidelines] are the
baseline; what follows is where this repository is more specific.

**The first sentence stands alone.** rustdoc renders it as the summary in
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
- No `# Safety`. `unsafe_code` is `forbid` at the workspace level, so there is
  no unsafe code here to document. If that ever changes, the section states
  the proof obligation the caller must uphold, and the reason it holds.

**Examples use `?`, not `unwrap()`.** Clippy denies `unwrap_used`,
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

**A doc comment belongs to the item below it.** rustdoc treats it as an
attribute and never checks that the prose describes that item, so a block
inserted one line too high silently hands a type its neighbour's summary.
Never insert a doc block without landing above the whole of the previous
item's comment. `just doc-blocks` catches the shape; it cannot catch the
meaning.

## Source comments

A comment ships only if it passes all three gates. Fail any: delete or
rewrite. Borderline: delete — borderline means the information is
reconstructible, which is what makes deletion cheap.

**Loss.** Three years from now, would losing this cost a maintainer real time
rediscovering intent, an invariant, a constraint, or a failure mode the code
and tests do not already make obvious?

**Elite.** Would SQLite, Redis, the Go standard library, or CPython write this
comment, at this length? Those projects state the constraint and stop. They do
not argue with an imagined objector.

**Upkeep.** Will it stay true without maintenance? A comment that hand-syncs a
value the code owns — a count, an offset, a line reference, a duplicated
constant — is false the first time that value moves.

### Ceiling

One or two lines. A comment reaching four is either carrying several facts, in
which case split it, or arguing, in which case cut it to the fact.

Rationale, alternatives weighed, and the story of how the code got here belong
in the commit message: timestamped, attached to the exact diff, and free to
maintain.

A comment often holds both a constraint and the deliberation that found it.
Keep the constraint, cut the deliberation. "Runs at most once per second"
survives; "this is the right trade for now" does not.

### Keep

- Why over how: upstream quirks, protocol and compatibility constraints,
  performance tradeoffs still part of the contract.
- Invariants, preconditions, ordering, lifetime, and concurrency requirements
  that types and tests cannot express.
- Code that looks wrong but is not, so a later cleanup does not reintroduce
  the bug.
- A high-level sketch of an algorithm whose local operations do not reveal the
  whole.

### Delete

- Narration of the next lines; code translated into English.
- Restated names, types, defaults, or control flow.
- Values duplicated from the code and hand-synced.
- Justification, hedging, or apology for a choice.
- Speculation about future requirements.
- History version control already holds, including commented-out code.
- Ticket and issue numbers. They say nothing to a reader without tracker
  access, and they rot when the tracker moves. Unfinished work goes in the
  tracker, not the source.
- Transient observations — "currently", "for now", "the latest release" — that
  go stale with no nearby edit.

### The upkeep gate in practice

It reaches values that track our own code. It does not reach frozen external
facts.

Bad (Delete):

```rust
// There are 321 tests to complete for servers.
```

Good (Keep):

```rust
// tmux < 3.2 reports the pane ID only after the command completes,
// so this query must stay separate.
```

### Documentation exception

Doctests, minimal usage examples, and the `# Errors` and `# Panics` lines on
public API are exempt from the loss gate — they serve the caller, not the
maintainer. They are exempt from nothing else. Ceiling: a good man page entry.

Rustdoc `///` comments and `# Examples` doctests fall under this exception; a
rustdoc example is compiled and run.

## Changelog

`CHANGELOG.md` is a ledger, not a narrative and not `git log`. It is scanned,
not read: a reader asks whether a release affects them and wants the answer
without a paragraph. One change, one bullet.

The shape already in the file, and to keep:

- [Keep a Changelog][keep-a-changelog] headings — `### Added`, `### Changed`,
  `### Fixed`, `### Removed` — under `## <version> - <date>`, with entries
  landing under `## Unreleased` until a release names them.
- A heading prefixed with the crate name when only that crate moved, as in
  `## tmux-mcp 0.1.0-alpha.4 - 2026-08-14`, and an opening line saying which
  crate got which number whenever the versions diverge.
- `**Breaking.**` as an inline lead-in on the bullet. While the version is
  `0.1.0-alpha.*` nothing is stable and breaks are expected, so the marker
  flags the ones a caller has to act on rather than every API change.

Writing an entry:

- Lead with the identifier and a concrete verb: add, fix, remove, refuse,
  report, `now`, `no longer`. Name identifiers literally —
  `Pane::capture_lines`, `--confirm`, `TMUX_MCP_CONFIRM`.
- Write from the caller's side. "Reduce peak memory when traversing large
  trees", not "replace the walker with a channel-based scheduler."
- A second sentence carries the impact, and is usually the sentence worth
  having: what the old behavior was, what a caller should do instead, what is
  unaffected.
- State a changed default explicitly, and an incompatibility more explicitly
  still, with the way forward in the same bullet.
- Do not sell a fix. "No longer returns another command's reply", not
  "improves reliability". Do not describe effort.
- A refactor no caller can observe is not an entry. A dependency bump is not
  an entry unless it changes behavior or addresses a vulnerability.
- An MSRV change is never filed as "update toolchain". It is a compatibility
  change, named with both versions.

## Release notes

The changelog is the durable ledger; the GitHub release is an upgrade
briefing, written for someone deciding whether to take this version now. They
are not the same document, and the release note is allowed to be shorter.

A release note leads with the two or three things that change a decision:
breaking changes first, then new capability, then anything with a measured
effect. Implementation improvements appear only where a caller observes the
result. Link to the changelog for the rest; do not paste it.

The publish workflow writes a short note from the tag and marks the release a
prerelease. Expanding it by hand is worthwhile for a release with a break in
it, and unnecessary otherwise.

No emoji, in release notes or anywhere else.

## Commit messages

```
Scope(type[detail]): concise description

why: Explanation of necessity or impact.

what:
- Specific technical changes made
- Focused on a single topic
```

Keep the subject to 50 characters or fewer, excluding any trailing `(#NN)` PR
reference, and wrap body lines at 72. Separate `why:` and `what:` with a blank
line.

The subject is the semantic delta, and should read as a changelog fragment:
`Avoid following junctions during recursive scans`, not `Update scanner`. The
scope names a subsystem — `Pane`, `ControlMode`, `tmux-mcp` — not a filename.

The body explains why, not the diff. The diff already shows what changed; the
message records the invariant that was wrong, why this approach was chosen,
and what deliberately did not change. A negative result belongs here: the
sentence saying which case did not improve is the one a reader five years from
now needs.

Types in use:

- **feat** — new features or enhancements
- **fix** — bug fixes
- **refactor** — restructuring without functional change
- **docs** — documentation
- **chore** — maintenance: tooling, config
- **test** — test-related
- **style** — formatting
- **rs(deps)** — dependencies
- **rs(deps[dev])** — dev dependencies
- **ai(rules[AGENTS])** — agent rule updates

`dependabot.yml` writes `rs(deps) `, `rs(deps[toolchain]) `, and `ci(deps) `
in the same format.

Example:

```
Pane(feat[send_keys]): Add support for a literal flag

why: Send characters without tmux interpreting them.

what:
- Add a literal field to SendKeysOptions
- Pass -l when it is set
```

For a multi-line message, use a heredoc so the formatting survives:

```console
$ git commit -m "$(cat <<'EOF'
Scope(feat[detail]): Concise description

why: Explanation of the change.

what:
- First change
- Second change
EOF
)"
```

### Release commits

Tagging is allowed here, and a tag publishes to crates.io. A published version
is immutable — it can be yanked, never replaced — so push one tag, watch it
land, and only then push the next. The procedure is in
[`CONTRIBUTING.md`](CONTRIBUTING.md).

A release commit subject is plain and short: `Tag v<version>`. The detail goes
in the body. Do not use the `Scope(type[detail]):` format for a release; it
buries the lede.

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
  "`kill_session` refuses without `--confirm` when the client cannot ask" beats
  "be careful."

An error message answers three questions in order: what failed, why, and what
the reader can do. `Error::kind` exists so a caller can branch on the answer;
the message exists so a person can act on it.

Defaults belong in the help text, next to the flag, not in prose a reader has
to hunt through.

## Terminology and capitalization

- Headings are sentence case: `## Platform support`, not `## Platform Support`.
- `tmux` is lowercase, always, including at the start of a sentence — rewrite
  the sentence instead.
- Crate names are as published and in backticks: `libtmux`, `libtmux-macros`,
  `tmux-mcp`, `tmux-workspace`. The repository is `libtmux-rs`; the crate is
  `libtmux`. They are not interchangeable.
- Write `prerelease`, not `pre-release`. Write `changelog`, not `change log`.
- Rust is capitalized; `rustdoc`, `rustfmt`, `clippy`, and `cargo` are not,
  except at the start of a sentence.
- Prose here uses British spelling — `behaviour`, `catalogued`, `artefact`.
  Identifiers and rustdoc follow Rust's own conventions, which are American:
  `color`, `initialize`. Do not change one to match the other.
- Use the same verb for the same operation everywhere. `kill` is what tmux
  calls it, so it is what we call it; not "delete", "remove", or "close".
- Keep `Note:`, `Important:`, and `Warning:` out of prose. If it matters, say
  it in a sentence. Reserve an admonition for something genuinely destructive.

## Code blocks

Code blocks are paste-and-run units: pasting one block runs exactly one
intended action. Doctests and other executed examples are exempt — the test
suite runs them, nobody pastes them.

- **One command per block.** Multiple steps may share a block only when
  explicitly chained with `&&`, `;`, or `\` continuations — the chain is then
  one logical command.
- **Explanations go in prose above the block**, never as `#` comments inside
  it.
- **Command menus are per-command blocks with prose lead-ins**, not tables.
- **Shell commands use the `console` tag with a `$ ` prefix.** This separates
  interactive commands from scripts and enables prompt-aware copy.
- **Split long commands with `\`** — one flag or flag-and-value pair per
  indented continuation line, positional arguments last.
- **Show output only when the output is the point.** An example carrying both
  the invocation and what it printed is worth more than prose describing it,
  and it has to be deterministic to be worth anything.

Good:

Show the last ten commits as a graph:

```console
$ git log \
    --max-count=10 \
    --graph \
    --oneline
```

Bad:

```console
# Show the last ten commits as a graph
$ git log --max-count=10 --graph --oneline
```

## Markdown

- Plain CommonMark. No renderer-specific extensions, and no GitHub alert
  blocks (`> [!NOTE]`, `> [!WARNING]`) — they are literal text everywhere but
  GitHub. Prose that renders everywhere beats a callout that renders once.
- Markdown files in this repository wrap at 80 columns. A URL that cannot
  break is the exception; prefer a reference-style link so the prose still
  wraps.
- Pull request and issue bodies do not wrap. GitHub renders a single newline
  as a space in a file and as a line break in a comment, so a wrapped comment
  body arrives as ragged stubs.
- Directive comments are not prose and are never reformatted: `#![cfg_attr]`,
  `#[allow]`, and the `docs:` region markers a generator reads.

## Slop prevention

Treat AI slop as review-hostile noise, not as proof the text is wrong. The
goal is information density.

- **AI signatures.** No "Generated by", no conversational filler, no
  unexplained emoji, no tool metadata.
- **Brittle references.** No hard-coded line numbers, fragile file counts,
  dated "as of" claims, bare SHAs, or local absolute paths, unless the
  artefact is strictly evidentiary, such as a benchmark log.
- **Diff narration.** Do not restate what moved, was renamed, or was removed
  in anything the reader holds alongside the diff. The diff and the commit
  message already carry it.
- **Branch-internal narrative.** Do not mention intermediate states, abandoned
  approaches, or "no longer" behavior unless users of a published release
  actually experienced the old state.
- **Low-value scaffolding.** No ownerless TODOs, unused future-proofing, debug
  artefacts, or defensive wrappers around unreachable failure modes.
- **Prose inflation.** Governed by the diction table above.
- **Coded labels.** Plain imperatives only. No `[R1]`, no `Option B`, no index
  a reader has to decode.

Preserve the why. Never delete a comment documenting an invariant, a protocol
constraint, a platform quirk, or an upstream workaround — those are the facts
the source comment gates keep, and every other comment is judged against them.

[api-guidelines]: https://rust-lang.github.io/api-guidelines/documentation.html
[keep-a-changelog]: https://keepachangelog.com/en/1.1.0/
