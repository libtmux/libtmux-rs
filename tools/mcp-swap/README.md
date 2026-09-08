# mcp-swap

`mcp-swap` points every selected agent CLI at a particular build of
`tmux-mcp`: the checkout you are editing, a compiled profile, an explicit
binary, or a published release. `use` updates the selected configuration
files; `revert` restores the exact pre-swap bytes, mode, and symlink route from
the first backup for each layer.

This is a private workspace tool (`publish = false`), not a supported
`libtmux` crate or an end-user configuration manager.

## Sources

`--source` selects where the server comes from:

- `debug` and `release` build the crate and register the binary in
  `target/<profile>/`. An agent starts it directly, with no build step in
  front of the handshake. `debug` is the default.
- `run` registers `cargo run --locked`, which checks for a rebuild on every
  start. It follows current source without another swap, but the first launch
  after a change can outlast a client's handshake timeout.
- `published` installs one crates.io version under its own private root. Two
  selected releases do not overwrite each other's executables.
- `path` registers the executable passed with `--bin`, wherever it came from.

Defaults come from the MCP crate's `Cargo.toml`:

- The server name is the package name with a trailing `-mcp` removed
  (`tmux-mcp` becomes `tmux`).
- The binary name is the first `[[bin]]` name, a `src/bin` stem, or the package
  name.

## Examples

List the known clients and report whether each binary and configuration file
is present:

```console
$ cargo run --locked --package mcp-swap -- detect
```

Show the current `tmux` entry in each existing configuration:

```console
$ cargo run --locked --package mcp-swap -- status
```

Validate an all-detected-client swap without building, launching, or writing:

```console
$ cargo run --locked --package mcp-swap -- use --dry-run
```

Build and select the release profile:

```console
$ cargo run --locked --package mcp-swap -- use --source release
```

Select the `cargo run` launcher:

```console
$ cargo run --locked --package mcp-swap -- use --source run
```

Install and select a version-isolated published release:

```console
$ cargo run --locked --package mcp-swap -- use --source published --version 0.1.0-alpha.10
```

Restore every recorded client layer:

```console
$ cargo run --locked --package mcp-swap -- revert
```

Pass `--cli` more than once or use comma-separated selectors. Selection is
deduplicated into the fixed transaction order; `antigravity` is an alias for
`agy`.

```console
$ cargo run --locked --package mcp-swap -- use --cli cursor,pi --cli antigravity --env LIBTMUX_TOOLSETS=standard
```

The swap preserves each entry's existing environment, with explicit `--env`
values winning. `LIBTMUX_SAFETY` is retired: if an existing entry has it, the
swap removes it only when `LIBTMUX_TOOLSETS` supplies the replacement authority.

`doctor` is read-only and reports config readability, outstanding recovery
entries, and authentication environment variables that override stored CLI
logins.

```console
$ cargo run --locked --package mcp-swap -- doctor
```

## Scope

The tool is deliberately narrow:

- **Known global configs only.** It writes `~/.cursor/mcp.json`,
  `~/.claude.json`, `~/.codex/config.toml`,
  `~/.gemini/settings.json`, `~/.grok/config.toml` (TOML `mcp_servers`, the
  same shape as Codex), `~/.gemini/config/mcp_config.json` (agy/Antigravity's
  JSON `mcpServers` file), `$XDG_CONFIG_HOME/opencode/opencode.jsonc`, and
  `~/.pi/agent/mcp.json`. Existing JSON, JSONC, and TOML documents keep
  unrelated keys, comments, and surrounding structure; only the selected
  server entry changes.

  Project-local files such as `$PWD/.cursor/mcp.json`,
  `$PWD/.gemini/settings.json`, and `$PWD/opencode.json` are not walked.
  Claude is the exception: its per-project
  `projects.<absolute-repo>.mcpServers` entry lives inside `~/.claude.json` and
  is supported. When project precedence matters for another client, use that
  client's native command. For example, use `cursor mcp add` or
  `gemini mcp add`; opencode has no non-interactive project-scope add, so edit
  `$PWD/opencode.json` directly.

- **opencode merges three global files.** `config.json`, `opencode.json`, and
  `opencode.jsonc` in the same directory are all loaded, with `.jsonc`
  winning. `mcp-swap` owns `.jsonc`, which opencode itself writes. A stale
  `mcp.<name>` in `opencode.json` still merges underneath; remove it by hand
  if that matters.

- **pi needs an adapter.** pi ships no built-in MCP client.
  `~/.pi/agent/mcp.json` is consumed by the third-party `pi-mcp-adapter`
  extension. `detect` reports when that package directory is absent, because
  writing a swap without the adapter has no effect.

- **Claude has two layers.** `use --scope project` is the default and changes
  only the selected repository's entry. `use --scope user` changes Claude's
  top-level fallback for projects without an override. Other clients silently
  normalize either value to `user`. Both Claude layers can coexist with
  independent first backups. An unscoped `revert` restores both in reverse
  swap order as one transaction; a scoped revert cannot skip a newer layer.

- **Detection is simple.** A client is detected when its executable is on
  `PATH` and its known configuration file exists. Custom Homebrew/npm prefixes
  and client-local install directories count only when they are already on
  `PATH`.

- **One config shape per client.** There are no fallback paths and no merge of
  multiple sources. If a setup differs from the paths and formats above, use
  the client's native MCP command.

## Recovery and safety

Mutating commands serialize through the shared cross-port lock at
`$XDG_STATE_HOME/libtmux-mcp-dev/swap/state.lock`. Rust recovery state lives
under the sibling `rust/` namespace, and Rust backup filenames carry a
`mcp-swap-rust` marker so another language port cannot claim them.

Before any config is published, the tool stages and synchronizes every output,
retains the first 0600 backup for each client layer, and publishes a bounded,
versioned, checksummed 0600 recovery ledger. It authenticates paths, symlink
routes, parent directories, device/inode identities, modes, sizes, and SHA-256
digests at mutation boundaries. Publication never replaces a path that
appeared late.

Before `use` writes anything, it launches the selected server with the proposed
environment and requires a valid MCP `initialize` result. A healthy stdio
server may remain running after replying; the probe accepts the reply, then
terminates its process group and reaps the direct child. On Linux, an
exact-marker sweep also terminates descendants that leave the group. Other
Unix targets provide process-group cleanup only, so a detached descendant can
outlive the probe there.

If a boundary changes and exact reverse rollback can be proven, every earlier
step is unwound in reverse order. If it cannot be proven, the command fails
closed and retains the recovery artifacts instead of guessing which file is
safe to overwrite or delete. Inspect such artifacts before removing them; an
untracked backup can be the only surviving pre-swap copy.

`--dry-run` parses all selected configurations and authenticates existing
recovery state, but does not create the lock/state directory, build or install
a server, run preflight, or write a file.

## Development

Run the focused native suite:

```console
$ just swap-test
```

All mutation tests use temporary configuration roots. CLI mutation tests also
replace `HOME`, `XDG_CONFIG_HOME`, and `XDG_STATE_HOME`. Tests must never
inspect or write a developer's live client configuration.
