# Changelog

Notable changes to `tmux-mcp`. The format follows [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Releases are numbered `0.1.N`, and each release increments `N`. The API is not
settled: any release may change or remove exported identifiers, and the
changelog marks each break. Pin an exact version to avoid one.

The crates are released together, so every release has a section here.
`tmux-mcp` moves at its own pace, so the version in a heading is not always its
own: from `0.1.0-alpha.4` to `0.1.0-alpha.17` the heading names the other
crates, and `tmux-mcp` was one alpha ahead, from `0.1.0-alpha.5` to
`0.1.0-alpha.18`. The release summaries in the main changelog give both. Changes
that span crates, such as documentation, CI and release tooling, are in the
[main
changelog](https://github.com/libtmux/libtmux-rs/blob/master/CHANGELOG.md),
which a published crate cannot link by a relative path.

## Unreleased

## 0.1.2 - 2026-11-01

No entries for `tmux-mcp`.

## 0.1.1 - 2026-10-31

### Changed

- **Behaviour change.** `tmux-mcp` no longer drives a server of its own on the
  `libtmux-mcp` socket. With no `-S` or `-L` it takes `LIBTMUX_SOCKET_PATH`,
  then `LIBTMUX_SOCKET`, else tmux picks the server. It starts and stops no
  daemon and loads no shipped configuration, `teardown` is not in the default
  toolsets, and both variables set together no longer stop startup:
  `LIBTMUX_SOCKET_PATH` wins. To keep the old setup, start a server on the
  `libtmux-mcp` socket yourself, run `tmux-mcp -L libtmux-mcp`, and add
  `teardown` to `LIBTMUX_TOOLSETS`. (#119)

## 0.1.0 - 2026-10-25

No entries for `tmux-mcp`.

## 0.1.0-alpha.17 - 2026-10-24

### Added

- `tmux-mcp` `wait_for_text` now runs on tmux 3.3a through 3.6 by reading the
  screen with `capture-pane` every 120 ms, where it used to refuse with
  `unsupported_version`. A pattern that appears and scrolls off between two
  reads is missed. `capture_since` and `run_shell_command` still need 3.7. (#72)

### Fixed

- `tmux-mcp` `capture_last_command` chooses its `capture-pane` flags from the
  daemon's version rather than the `tmux` executable's, so a newer client
  against a 3.3a daemon no longer sends flags the daemon rejects. (#72)
- `tmux-mcp` `wait_for_text` joins rows the pane wrapped, so a match split
  across a wrap is found, and a line this server submitted that wrapped is
  discounted as its own echo. (#72)
- `tmux-mcp` `wait_for_text` reports `stream_lost` when its control stream
  ends while the pane is still alive, where it reported `pane_closed`. A pane
  or server that ends still reports `pane_closed`, with the output seen so
  far. (#72)

## 0.1.0-alpha.16 - 2026-10-18

### Added

- `tmux://capabilities` reports `minimumTmuxVersion`. (#70)

### Changed

- `tmux-mcp` reports a refused tmux server as `unsupported_version` with the
  remedy, and its instructions state the 3.3a minimum. (#70)

### Fixed

- `tmux-mcp` completion helpers accept a marker only from the tmux server it
  was issued for, so a server replaced before the opening frame refuses the
  job, even when the replacement runs the same tmux version. (#70)

## 0.1.0-alpha.15 - 2026-09-27

No entries for `tmux-mcp`.

## 0.1.0-alpha.14 - 2026-09-27

### Added

- `tmux_mcp::ToolError` implements `Display` and `std::error::Error`, so a
  program that calls a tool directly can propagate its failure with `?`.
  (#33)
- `tmux-mcp` ships runnable examples: `run_and_wait` runs a command for its
  exit status and waits for a line a background job prints, `follow` reads
  everything a pane wrote between two `capture_since` calls, scrollback
  included, and `refused` shows the error a client gets for a tool the server
  left out. (#33)

### Changed

- **Breaking.** `tmux://capabilities` reports each tool's name, title,
  annotations and capability row, and no longer its description or input and
  output schemas, which `tools/list` already carries; `schemaVersion` is 2.
  The report for the default surface falls from 164 KB to 26 KB. Read schemas
  from `tools/list`. (#33)
- `tmux-mcp`'s server instructions tell an agent what the server is for and
  what it is not (tmux panes, not editor splits or desktop windows), that
  listings cannot see terminal text, which tool to wait with instead of
  polling, how output loss is reported, and that a pane in a tmux mode belongs
  to the person in it. They stay under 2 KB. (#33)

## 0.1.0-alpha.13 - 2026-09-26

### Fixed

- MCP `select_layout` rejects malformed input before window lookup and uses the
  core's daemon-aware layout guard. Successful replies return tmux's applied
  saved layout, including when the request uses a name abbreviation. (#27)

## 0.1.0-alpha.12 - 2026-09-19

### Fixed

- `tmux-mcp`'s `wait_for_text` ignores echoed input from `send_keys`,
  `paste_text` and `run_shell_command` when matching command output. (#28)
- `tmux-mcp`'s streamed captures and waits keep reading output after an
  unterminated terminal escape string. (#28)
- `tmux-mcp` reports tool refusals as tool errors, with structured reasons
  for clients deciding whether to retry. (#28)
- `tmux-mcp` accepts session IDs returned by its own listings. (#28)
- `tmux-mcp` treats empty `TMUX` and `TMUX_PANE` as detached operation. (#28)
- `tmux-mcp` accepts interrupt keys while `run_shell_command` owns a pane,
  so callers can stop a running command. (#28)
- `tmux-mcp` keeps its shared daemon alive while another instance uses it.
  (#28)

### Changed

- **Breaking.** `tmux-mcp` tool methods return `ToolError` for refusals.
  (#28)
- **Breaking.** `tmux-mcp`'s `wait_for_text` distinguishes output present
  at entry, pending input and output received during the wait. (#28)
- **Breaking.** MCP tool metadata omits repeated descriptions and schemas;
  read them from the tool or `tmux://capabilities`. (#28)
- `tmux-mcp` requires the `teardown` toolset for `set_history_limit`. (#28)

### Security

- **Breaking.** MCP environment tools withhold values unless their names
  appear in `LIBTMUX_ENVIRONMENT_VALUES`. (#28)

## 0.1.0-alpha.11 - 2026-09-12

### Fixed

- The MCP reports text that is not an id as `bad_input` rather than as an
  object that went away, so an agent is no longer told to look again for
  something that can never resolve. It resolves a pane or window through
  tmux's own `-f` predicate instead of listing every object and comparing
  rendered strings, so a padded id such as `%01` now addresses `%1`; a
  well-formed id for an object that is genuinely gone still reports
  `object_gone`. (#26)

### Changed

- The MCP names the words it accepts when it rejects a split direction, as it
  already did when rejecting a resize direction. Both lists are generated from
  the libtmux enums they stand for through an exhaustive match, so a variant
  added there fails the build rather than going unadvertised. The words a
  client may send are unchanged. (#26)

## 0.1.0-alpha.10 - 2026-09-07

### Added

- `LIBTMUX_TOOLSETS`, `LIBTMUX_TOOLS`, and `LIBTMUX_EXCLUDE_TOOLS` freeze an
  exact tool surface at startup. An unknown name or an empty comma-list token
  stops startup rather than being ignored, and an exclusion wins over
  toolsets, named inclusions, and aggregate nested authority.
- `tmux://capabilities` reports the surface a client actually received: the
  effective tools, the selected socket and how it was chosen, direct process
  reach, tmux effects, output classes, schema-keyed input literalization,
  future-input amplification, nested authority, and whole-call MCP
  annotations.

### Changed

- **Breaking.** The MCP surface is 45 tools in the unordered `inspect`,
  `manage`, `execute`, and `teardown` toolsets. Registration, descriptions,
  schemas, selection, and capability reporting come from one typed native
  manifest, so a tool cannot be advertised with a description that no longer
  describes it.
- **Breaking.** The binary defaults to the dedicated `libtmux-mcp` socket and
  a minimal tmux configuration. A newly created default daemon enables all
  four toolsets; an existing or explicitly configured daemon requires an
  explicit selection before `teardown` is enabled.
- **Breaking.** `LIBTMUX_TMUX_CONFIG` accepts only a nonempty absolute path,
  and rejects anything else before tmux is opened. A relative path changed
  meaning with the launch directory.
- **Breaking.** Spawn tools start only their configured pane process and
  accept no command or environment payload. A caller that sent one now has it
  refused rather than run.
- The authenticated creator stops its dedicated daemon when stdio closes, so a
  client that exits leaves no server behind.
- `run_shell_command` stays request-owned through completion, timeout,
  cancellation, or a dropped request, and exposes no background-job handle.
- `set_synchronize_panes` advertises that its input reaches every pane in the
  group, so a client can ask before amplifying a keystroke.
- Pane input fails closed when an effective configured recipient is dead, in a
  human-owned mode, or may be the inherited MCP caller.
- `paste_text` remains target-only, and `run_shell_command` requires exactly
  one recipient at both pre-dispatch checks.
- `run_shell_command` rejects ASCII terminal-control bytes in its exact
  executable or socket route before any watcher is set up.
- `call_read_tools_batch` exposes an exclusion-pruned 16-name schema and caps
  its complete JSON-RPC response line, request ID and newline included, at
  1,000,000 bytes without dropping an executed row. Truncation stays explicit,
  and a serialized request ID over 512 KiB fails before dispatch.
- `capture_since`, and a read batch holding only that operation, report
  observe-only effects. Retaining a cursor does not change tmux state.
- `search_panes` uses the linear-time regex engine with a 1 MiB matching
  budget, and pane searches and waits reject oversized pattern sets.

### Removed

- **Breaking.** `enter_copy_mode` and `exit_copy_mode` are no longer MCP
  tools. Capture, snapshot, search, and cursor tools observe pane output
  without taking ownership of a human-controlled mode; `Pane::copy_mode` and
  `Pane::exit_mode` remain for an application that owns the whole interaction.
- **Breaking.** `--safety`, `LIBTMUX_SAFETY`, and `TMUX_MCP_SAFETY` no longer
  select ordered tiers. Either retired environment setting stops startup with
  a migration error rather than being ignored.
- **Breaking.** `--confirm`, `--no-confirm`, and `TMUX_MCP_CONFIRM` no longer
  configure server-side approval. Remove them; a client decides whether to ask
  from each tool's MCP annotations.
- Prompts, generic plan execution, background-job tools, server-wide tools,
  generic mutating tools, and dynamic resource templates are no longer part of
  the public MCP surface.

### Security

- Tool descriptions and the capability report separate socket-scoped object
  selection, pane-process authority, output sensitivity, direct effects, and
  conservative whole-call annotations. Neither tool filtering nor an MCP
  annotation is described as authorization or operating-system confinement,
  because neither is one.

### Fixed

- Caller and pane-input checks read the configured socket path instead of
  asking tmux for `#{socket_path}`. tmux escapes a non-printable byte in the
  path when it stores it, and releases disagree about reporting it, so the
  answer named a file that does not exist: a pane read as busy told an agent
  to wait, `retryable: true`, for a `run_shell_command` that was never
  running, and a path carrying an ASCII terminal-control byte arrived as four
  printable characters and passed a check written to refuse it. A path that is
  not valid UTF-8 also came back with replacement characters on every release.
- Pane input and teardown treat inherited `TMUX` and `TMUX_PANE` as one
  canonical identity, resolve a same-daemon caller in its claimed session, and
  fail closed on partial, stale, or inconsistent selected-daemon context. A
  complete identity on another physical socket stays foreign.
- `run_shell_command` completes normally on tmux 3.3 through 3.4, returning
  output and exit status when the command ends instead of waiting until the
  request deadline.
- `run_shell_command` completes on macOS and the BSDs. A pane accepts about
  1024 bytes of input in one burst there, against 4096 on Linux, and discards
  the rest, so the completion frame -- 2.6 KB for `sh` and 4.7 KB for `bash`
  -- never arrived whole and every run waited out its deadline for a marker
  that could not come. Sending one line at a time fails the same way, because
  the bound is the burst rather than any one line, so the frame is staged
  where the pane reads it back instead of being typed. The typed line no
  longer carries the command either, and so no longer grows with it: one over
  4 KB was truncated on Linux too.

## 0.1.0-alpha.9 - 2026-08-31

### Changed

- **Breaking.** The MCP `cancel_job` tool is replaced by `forget_job`.
  Forgetting stops collection and discards retained output but does not
  interrupt the command running in its pane. (#16)
- `capture_since` returns the first retained screen when the requested cursor
  predates retained history, making truncation observable without losing the
  available snapshot. (#16)
- MCP schemas now close fixed vocabularies and reject unknown fields instead
  of silently accepting unsupported input. (#16)

### Fixed

- Retained MCP jobs, pane tails, and streamed output honor their byte limits,
  cancellation state, and truncation boundaries. (#16)
- MCP rename results report the name tmux actually stored. (#16)

## 0.1.0-alpha.8 - 2026-08-22

No entries for `tmux-mcp`.

## 0.1.0-alpha.7 - 2026-08-22

No entries for `tmux-mcp`.

## 0.1.0-alpha.6 - 2026-08-16

### Added

- `tmux-mcp` runs commands in the background: `start_command` returns a job id
  at once, `job_status` reports the exit status and only what is new since the
  cursor it gave you, `list_jobs` and `cancel_job` manage them. A ten-minute
  build no longer costs an agent its turn, and several can run at once.
- `tmux-mcp` gains `wait_for_idle`, for when a caller cannot name what success
  looks like -- a TUI settling, a prompt glyph no regex predicts.
- `tmux-mcp` gains ten tools that reach what nothing else could:
  `list_servers` (these tools bind one socket for life, so nothing else could
  learn another exists), `expand_format` (any tmux format, so a field with no
  tool of its own is one call away), `show_environment`, `set_environment`,
  `show_hooks`, `pipe_pane`, `select_layout`, `clear_pane`, `respawn_pane`
  and `paste_text`.
- `cargo run --example budget`, measuring what a client downloads at
  `tools/list`. That budget is why adding a tool is a decision.
- `what_changed`, reporting which windows have written since the timestamp it
  handed back, so re-orienting costs one call rather than a capture per pane.
- `run_command`, `wait_for_text` and `wait_for_idle` report progress every
  five seconds to a client that asked for it. Measured before it was built:
  Codex sends a `progressToken` on every `tools/call`, so this is consumed
  rather than merely published. A client that sends no token pays nothing.
- `--confirm` and `TMUX_MCP_CONFIRM`, which ask a person before anything
  destructive and refuse when the client cannot ask. Measured first: Codex
  declares `elicitation`, so this is a question something can answer.
- `capture_pane` takes `last_command`, returning only what the last command
  printed instead of the screen or the whole history. It reports `marks` --
  `present`, `absent`, or `unsupported` -- because an answer that fell back
  looks exactly like a command that printed a great deal.

### Changed

- `run_command`, `wait_for_text` and `wait_for_idle` each answer with their
  own outcome vocabulary instead of a shared one. Every wait used to advertise
  `no_shell` and every run `matched`, which are answers they cannot give;
  `tools/list` also lost 2.8 KB.

## 0.1.0-alpha.5 - 2026-08-16

### Added

- `tmux-mcp` caps its tmux-side fan-out.

## 0.1.0-alpha.4 - 2026-08-15

No entries for `tmux-mcp`.

## tmux-mcp 0.1.0-alpha.4 - 2026-08-14

That crate alone. Version numbers diverge from here, because `tmux-mcp`
moves at its own pace.

## 0.1.0-alpha.3 - 2026-08-14

No entries for `tmux-mcp`.

## 0.1.0-alpha.2 - 2026-08-14

No entries for `tmux-mcp`.

## 0.1.0-alpha.1 - 2026-08-13

No entries for `tmux-mcp`.
