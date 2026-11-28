# Changelog

Notable changes to the workspace that span more than one crate: documentation,
CI, tests, release tooling, private tools, and the notes that cover a whole
release, including which version each crate is at. The format follows [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/), and the crates follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). A change to one
crate is in that crate's changelog:

- [`libtmux`](crates/libtmux/CHANGELOG.md)
- [`libtmux-core`](crates/libtmux-core/CHANGELOG.md)
- [`libtmux-blocking`](crates/libtmux-blocking/CHANGELOG.md)
- [`libtmux-tokio`](crates/libtmux-tokio/CHANGELOG.md)
- [`libtmux-macros`](crates/libtmux-macros/CHANGELOG.md)
- [`tmux-workspace`](crates/tmux-workspace/CHANGELOG.md)
- [`tmux-mcp`](crates/tmux-mcp/CHANGELOG.md)

Crates are versioned together except `tmux-mcp`, which moves at its own pace
because its dependencies need a newer compiler than the libraries do.

Releases are numbered `0.1.N`, and each release increments `N`. The API is not
settled: any release may change or remove exported identifiers, and the
changelog marks each break. Pin an exact version to avoid one.

## Unreleased

### Changed

- `libtmux_blocking::Server::capture` includes a complete program that reads
  an existing pane and writes its exact bytes without Tokio. The blocking
  README links the example and shows its socket and pane arguments.
- The root README puts ordinary Tokio use before fixture examples and names
  the blocking client's current typed operations. The Tokio README uses
  sibling checkout dependencies for its unpublished API and distinguishes
  the published `libtmux` 0.1.2 API.
- `check-api-examples` validates the split facade's standalone run recipe
  against the facade README, changing only the Cargo invocation. Legacy
  and Tokio profiles retain their exact default command.
- `format_injection` checks literal start directories against the child
  process's byte report. tmux 3.3a may omit its current path while the child
  starts; unescaped directory formats still fail the assertions.
- The copied mixed-reader proof uses the source-owned private socket recipe
  in HTML and Markdown. Its archive join checks the caller-owned fixture,
  exact recipe digest and both public client feature graphs, rejecting
  `test-support` in the copied or staged reader.
- The docs preview copies complete multiline subshell recipes. Its Rust
  gate checks the production code-block renderer before repeating the site
  build and running copied programs.
- The docs preview resolves primary home objects and native ID checks for
  both Rust source layouts. Public API extraction follows parsed module
  re-exports, so hidden helper names cannot acquire another declaration's
  identity. The LLM export and fixture guidance use the selected API.
- The docs preview resolves showcase API links to the defining Rust crate
  while retaining the released `0.1.2` identifiers. Its curated links omit
  the retired `Window::select` owner. CI and the installed-reader proof use
  the same docs revision for those links and the native recorder.
- The root, facade and blocking crate READMEs start with a native blocking
  program that creates, observes and kills its own session on an explicit
  private socket. `installed_clients.py` now compiles and runs the displayed
  facade README program from its archive with default features, records its
  digest and checks stdout and cleanup. The no-default facade docs retain a
  runnable shared-type example.
- `installed_clients.py` records a stdio witness for a saved MCP window link
  after withdrawal. The archive-installed tools must refuse it while the
  surviving link and session focus stay unchanged; only `select_window` is
  enabled alongside `inspect`.
- `cargo deny check` now accepts RUSTSEC-2026-0543, which marks `chrono`
  unmaintained, so `just check` passes again. Only `tmux-mcp` reaches `chrono`,
  through `rmcp`, and no `rmcp` release drops it yet. `deny.toml` names that one
  advisory and says when to delete the entry; every other advisory still fails
  the check, as do yanked crates, licences, bans and sources. (#171)
- CI installs the toolchain `rust-toolchain.toml` pins and caches builds with
  `actions-rust-lang/setup-rust-toolchain`, where jobs installed `stable` and
  let the pin override it inside the checkout. Clippy and rustfmt failures now
  show on the pull request diff. The 1.85.0 and 1.88.0 floors and `nightly`
  install alongside the pin. (#140)
- **Behaviour change.** The Cargo runner in `.cargo/config.toml` is now
  `scripts/sandbox/sandbox_runner.py`, a typed Python script that `uv` runs, so
  `cargo test`, `cargo run` and `cargo bench` in a checkout need `uv` on `PATH`.
  It sets the same environment, exits with the same statuses and prints the same
  messages as the shell runner it replaces, and removes the `PATH` entry and
  variables `uv` adds. Measured on one loaded Linux host, it starts a binary in
  106 ms at the median, where the shell runner took 59 ms. (#160)
- The support scripts are typed Python under `scripts/<name>/`, each with a
  README and pytest tests, and `just scripts` formats, lints, type-checks and
  tests them with ruff, ty and pytest, which `just check` runs. They replace the
  shell scripts `check-doctest-sandbox.sh`, `check-package-contents.sh`,
  `compat-lanes.sh`, `require-tests.sh`, `run-examples.sh`, `sandbox-outside.sh`,
  `sandbox-runner.sh` and `test-tmux-format-compat.sh`. The `ci-test` and
  `compat-supervisor-test` recipes are gone, and the planner, the supervisor and
  the other Python scripts moved into their own directories. (#160)
- CI installs tmux and its build dependencies with the shared `apt-get` action
  of `libtmux/.github`, pinned by commit, and the repository no longer carries
  `scripts/apt-get.sh`. The compatibility lanes keep their own tmux build,
  because the shared `build-tmux` action pins a release by the checksum of its
  archive where `.github/ci-matrix.txt` pins the commit each tag must name.
  (#160)

### Removed

- The tmuxp parity run and the CLI benchmark against tmuxp (`just parity`,
  `just bench-cli` and the weekly `parity` CI job), the structure tmuxp 1.74.0
  builds for each of its examples under
  `crates/tmux-workspace/tests/fixtures/tmuxp/expected/`, and the Python parity
  ledger `crates/libtmux/docs/parity.md` with its checker, `just parity-claims`.
  The tmuxp example workspaces stay as test inputs, and the checked format
  catalog table the ledger held is now `crates/libtmux/docs/format-catalog.md`.
  The Speed against tmuxp section of the workspace CLI guide went with the
  benchmark. (#160)

## 0.1.2 - 2026-11-01

`libtmux`, `libtmux-macros`, `tmux-workspace` and `tmux-mcp` are all 0.1.2.
Each crate now ships its own changelog (#128). No crate's code changed.

### Changed

- **Behaviour change.** `cargo test`, `cargo test --doc`, `cargo bench` and
  `cargo run` in a checkout of this repository no longer see `TMUX`,
  `TMUX_PANE`, `LIBTMUX_SOCKET` or `LIBTMUX_SOCKET_PATH`, and get a private
  `TMUX_TMPDIR` that a runner searches for servers a test left behind, failing
  the run when it finds one. Since 0.1.1 a `Server::new()` that names no socket
  reaches the server its caller is inside or tmux's default socket, so a test
  without one could drive the developer's own session. To reach a real server
  on purpose, run the built binary, or name it to `cargo run` with
  `LIBTMUX_SANDBOX_TMUX`.
  A run exits 78 before the binary starts when `TMPDIR` is relative or would put
  a socket path past 100 bytes, so shorten `TMPDIR`; and exits 70 when a binary
  that passed left a tmux server, which the runner stops, or started one on the
  default socket outside the sandbox, which it leaves alone. Published crates
  are unaffected. (#126)

## 0.1.1 - 2026-10-31

`libtmux`, `libtmux-macros`, `tmux-workspace` and `tmux-mcp` are all 0.1.1.
`libtmux`, `tmux-workspace` and `tmux-mcp` now pass tmux neither `-L` nor `-S`
when no socket is named in code or on the command line and neither
`LIBTMUX_SOCKET_PATH` nor `LIBTMUX_SOCKET` is set, so tmux picks the server.
To pin one, name its socket or set either variable. JSON layouts need tmux 3.8.

### Changed

- CI builds tmux 3.3a and 3.8 on each push and pull request, and every lane
  in `.github/ci-matrix.txt` on the weekly schedule and on a dispatch with
  `scope=full`. A dispatch also takes `tmux-ref`, `rust-toolchain`, `os` and
  `tmate`, so any branch can build any tmux version, tag, branch or commit.
  `LIBTMUX_COMPAT_LANES="3.3a 3.8" just compat` builds chosen lanes locally.
  (#118)

## 0.1.0 - 2026-10-25

The first final release: `libtmux`, `libtmux-macros`, `tmux-workspace` and
`tmux-mcp` are all 0.1.0, and `cargo add libtmux` resolves it without naming
an alpha.

### Changed

- Releases are numbered `0.1.N` from `0.1.0` on, with no `-alpha` tag unless
  the maintainer asks for one. Every `0.1.N` release may change the API, and
  the changelog marks each break with **Breaking.** The READMEs say so in one
  shared sentence in place of the alpha banner.

### Fixed

- The `wait_for_text` tool text, its README section and the `libtmux`
  migration guide no longer promise that text scrolling past between checks
  is seen below tmux 3.7, and the crate READMEs no longer claim macOS test
  runs in CI. (#97)
- The `libtmux` README, design and migration guides, and the capability
  summaries now say which refused commands the guard does not read when they
  are nested in another command, on which tmux releases, and whether the
  result is a server exit, the wrong buffer or a tmux error. The README also
  says `PaneWait::Dead` carries no text. (#99)
- `cargo test --test optional_features` now names the missing `test-support`
  feature instead of reporting `ok. 0 passed`. (#99)

## 0.1.0-alpha.17 - 2026-10-24

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.17;
`tmux-mcp` is 0.1.0-alpha.18.

tmux 3.7 is now the floor for control acquisition and the tools built on it;
everything else keeps the 3.3a minimum, with each operation naming the release
it needs and refusing below it. `wait_for_text` now works on tmux 3.3a
through 3.6, and `capabilities.json` lists every optional operation.

### Added

- A capability registry, `capabilities.json` at the workspace root, lists each
  optional tmux operation with the release it needs, how a caller is refused
  below it, the public APIs it affects and the releases with a known defect.
  It is emitted from the tables the refusals read, and a test fails when the
  committed file differs from them. (#72)

### Changed

- CI no longer runs the test suite on macOS. macOS stays supported on a
  best-effort basis, and the release workflow still builds and attaches the
  macOS binaries. (#87)
- Control acquisition, pane streams, subscriptions and the MCP tools that
  require them need tmux 3.7 or newer and a compatible native client. A daemon
  ranking below 3.7 final (`next-3.7`, `3.7-rc*`) refuses acquisition;
  `3.8-rc2` and `next-3.8` are eligible. Ordinary subprocess operations and
  polling keep the tmux 3.3a minimum. (#72)

### Security

- The release binaries of `tmux-mcp` and `tmux-workspace` build against tokio
  1.54.1, which fixes RUSTSEC-2026-0386 in tokio's `AsyncFd` constructors.
  (#92)

## 0.1.0-alpha.16 - 2026-10-18

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.16;
`tmux-mcp` is 0.1.0-alpha.17.

tmux 3.3a is now the minimum. Every crate reads the running server's version
before it sends anything and refuses an older server, or a report that names
no release, with the remedy. Callers that upgrade must handle
`Error::UnrecognizedTmuxVersion` and drop the removed `since` constants.

### Changed

- **Breaking.** `libtmux`, `tmux-mcp`, and `tmux-workspace` require tmux 3.3a
  or newer, checked against the running tmux server rather than the `tmux`
  executable. Raw commands, chains and scoped operations refuse an older
  server, including 3.3, with `Error::UnsupportedTmuxVersion` before any
  effect; an absent endpoint checks the executable that would start it.
  Upgrade tmux and restart the server, or pin the previous release. (#70)

## 0.1.0-alpha.15 - 2026-09-27

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.15;
`tmux-mcp` is 0.1.0-alpha.16.

Nothing in the API changes. Each crate's page on crates.io and docs.rs now
opens with its logo.

### Changed

- Each crate's README opens with its logo, and each crate packages the SVG and
  PNG under `assets/`. The images load from absolute URLs, so the logo also
  shows where `libtmux` embeds its README in the API reference. (#36)

## 0.1.0-alpha.14 - 2026-09-27

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.14;
`tmux-mcp` is 0.1.0-alpha.15.

Take this one if you run `tmux-workspace` as a command: every command's help
and manual page describe each argument and end with examples, `load --dry-run`
shows what a load would build, `cargo install tmux-workspace` builds it by
default, and releases carry prebuilt binaries for it and `tmux-mcp`. `load`
passes a variable's value to the pane's environment instead of pasting it into
the command, where a value holding shell syntax ran as code. The library reads
a tmuxp document the way tmuxp and the command do, which changes `PaneConfig`
and refuses a document without `windows`, and `tmux://capabilities` leaves
tool schemas to `tools/list`. Each breaking change is marked below.

### Added

- Releases of `tmux-workspace` and `tmux-mcp` carry prebuilt binaries for
  Linux (musl, x86_64 and aarch64) and macOS (arm64 and x86_64), with build
  provenance, and `cargo binstall` installs them. (#33)

### Changed

- The `tmux-workspace` README covers the command: install, load, one command
  per task and the differences from tmuxp. `docs/cli.md` is its reference, and
  the library guide is its docs.rs page. The `libtmux` migration guide covers
  each release, newest first. (#33)

## 0.1.0-alpha.13 - 2026-09-26

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.13;
`tmux-mcp` is 0.1.0-alpha.14, because it was already at alpha.13.

Take this one if you load tmuxp workspaces: `tmux-workspace` ships a binary
behind its `cli` feature that loads, freezes and converts them, imports
tmuxinator and teamocil files, and reports to scripts in JSON or NDJSON. A
plan's panes now build in the order the workspace gives them -- a four-pane
window had come out A, D, C, B -- and a window of five or more panes builds at a
default terminal size. Layout strings are checked before they reach tmux, which
on 3.3 and 3.3a exits its whole daemon on one it cannot parse. Two changes are
breaking: `ServerBuilder::colors` refuses `88`, and `Workspace::from_yaml`
refuses a `session_name` containing `:` or `.`.

## 0.1.0-alpha.12 - 2026-09-19

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.12;
`tmux-mcp` is 0.1.0-alpha.13, because it was already at alpha.12.

Take this one if you pass names, titles or start directories you did not write
yourself: they reach tmux as literal text now, so a value holding `#{...}` is a
value rather than a format, and text beginning with `-` is not read as a flag.
Failures that used to vanish now arrive -- a control stream reports its errors,
a scoped operation keeps its own failure alongside its cleanup's, and a setup
failure names the frame limit it hit instead of an ordinary close. Options have
one reader and checked writes, and the lenient `*_or_empty` listing twins are
gone.

### Changed

- `Window::select_layout` and layout operations in plans, workspaces and
  MCP refuse invalid layouts before dispatch. Unique preset prefixes remain
  accepted. (#28)

## 0.1.0-alpha.11 - 2026-09-12

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.11;
`tmux-mcp` is 0.1.0-alpha.12, because it was already at alpha.11.

Take this one if you build tmux plans, and especially if a plan of yours sends
text: `plan::SendKeys::text` documented literal text and did not send it
literally, so text naming a tmux key was pressed rather than typed. A plan can
now reach all four sides of a split rather than two, asking a client what it is
attached to costs one tmux command instead of two, and the MCP tells an agent
that a malformed target is bad input rather than an object that went away --
which had been advice to retry something that could never resolve.

## 0.1.0-alpha.10 - 2026-09-07

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.10;
`tmux-mcp` is 0.1.0-alpha.11, because it was already at alpha.10.

Take this one if you run the MCP server, and especially if you run it on
macOS, where `run_shell_command` could not deliver its completion frame at
all and every run waited out its deadline. The tool surface is now frozen
behind a typed manifest and reported by `tmux://capabilities`, so a caller
can read what it was given instead of inferring it by trying. Pane input
fails closed when it cannot prove which pane will receive it, and the
configuration swapper is a transactional Rust binary in place of a Python
script that rewrote files where they sat.

Every breaking change here is in the MCP surface. The libraries only gain:
three pane accessors, the resolved tmux executable, and operand delimiting
so a value beginning with a dash is not read as a flag.

### Changed

- The configuration swapper is `tools/mcp-swap`, a Rust binary, in place of
  `scripts/mcp_swap.py`. It authenticates every selected config and recovery
  artifact before building or writing, rejects path and inode aliases, and
  commits `use` or `revert` as one rollback-safe transaction; versioned state
  binds config and backup bytes, modes, identities, and topology, so `revert`
  refuses a file a human edited or replaced. A dry run performs the same
  read-only plan without building, launching a server, or writing.

## 0.1.0-alpha.9 - 2026-08-31

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.9;
`tmux-mcp` is 0.1.0-alpha.10, because it was already at alpha.9.

Take this one if you drive tmux from an async caller, or if you pass it names
that came from somewhere you do not control. Blocking tmux operations no
longer deadlock the runtime they are called from, and option, hook,
environment, layout, and workspace inputs are validated and separated from
tmux flags before execution; `tmux-workspace` now rejects names and start
directories that could be read as tmux formats or options.

Three exported items changed shape. `OptionSchema::scope` is replaced by
`scopes` and `accepts`, `Error::LinkGone` carries `kind` and `target`, and the
MCP `cancel_job` tool is now `forget_job`.

### Security

- Option, hook, environment, layout, and workspace inputs are validated and
  separated from tmux flags before execution. (#16)

## 0.1.0-alpha.8 - 2026-08-22

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.8;
`tmux-mcp` is 0.1.0-alpha.9, because it was already at alpha.8.

Take this one if you run tmux below 3.7: `ControlSender::mute_pane` could kill
the tmux server outright on every supported release but the newest.

### Fixed

- `Error::kind` reports a replaced tmux server as `ObjectGone` rather than
  `Refused`. A daemon that restarted on the same socket reissues ids from the
  start, so every handle captured from the previous one names something that
  is not there, and looking it up again is exactly the fix `Refused` said
  would not help. `Error::is_object_gone` now answers `true` for it, and
  `tmux-mcp` reports it as `"stale": true`.
- `Error::kind` no longer reports a missing tmux server as `Refused`, whose
  documented meaning is that the arguments were wrong. tmux exits 1 for a
  command it refused and for one that found no server, and separates them
  only in stderr. A caller branching on the kind was told to fix a request
  that was never the trouble; `tmux-mcp` reported the same thing on the wire
  as `"kind": "refused"`.

### Added

- `ErrorKind::ServerGone`, `Error::ServerGone` and `ServerGoneKind`, naming
  which way a server was not there: never started, unreachable on its socket,
  lost with the command in flight, or shut down with it in flight. The wording
  is pinned against every supported tmux release. `tmux-mcp` reports it as
  `"kind": "server_gone"`, and as an internal error rather than
  `invalid_params`, because the request was not what failed.

## 0.1.0-alpha.7 - 2026-08-22

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.7;
`tmux-mcp` is 0.1.0-alpha.8, because it was already at alpha.7.

No published code changed. Each crate's install instructions name the current
version, and a test that typed into a pane before its shell could read was
fixed.

## 0.1.0-alpha.6 - 2026-08-16

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.6;
`tmux-mcp` is 0.1.0-alpha.7, because it was already at alpha.6.

### Fixed

- `Error::control_mode_unrepresentable` wore a doc comment left behind by
  `control_mode_closed`.
- `Command::arg` and `Command::target` had swapped doc comments, and
  `control_mode_line`'s was spliced between them.
- `just doc-blocks` now also fails when a doc comment sits below a non-doc
  attribute, which is the shape a split leaves when it lands on a sentence
  boundary. It found the two above on its first run.
- Three doctests exercised tmux 3.3 capabilities without guarding on the
  version, so the whole doc suite failed on tmux 3.2a -- a lane CI runs.

## 0.1.0-alpha.5 - 2026-08-16

`libtmux`, `libtmux-macros`, and `tmux-workspace` are 0.1.0-alpha.5;
`tmux-mcp` is 0.1.0-alpha.6, because it was already at alpha.5.

### Added

- `Debug` on every public type, enforced by `missing_debug_implementations`.
- A runnable example on every crate-root type, enforced by
  `just example-coverage-check`.
- A recorded public API surface: `just api`, `just api-check`.
- `just doc-blocks`, which catches a doc comment split across two items.
- Fuzz targets for the control-mode, filter-expression, and workspace-YAML
  parsers. `just fuzz <target>`; weekly in CI.
- A test that handle equality and hashing separate two servers, and that the
  four handles are `Clone + Debug + Eq + Hash + Send + Sync`.
- `crates/libtmux/docs/format-coverage.txt`, measuring the format catalog
  against tmux's own source: 178 catalogued, 80 excluded, 15 missing.
  `just format-coverage-check` fails on drift.

### Fixed

- Twelve parity rows were `planned` after shipping.
- `just api-check` used a fixed `/tmp` path, shared between checkouts.

### Removed

- The `semver` recipe. `cargo-semver-checks` skips every lint on a
  prerelease-to-prerelease step and then reports success.

## 0.1.0-alpha.4 - 2026-08-15

`libtmux` and `libtmux-macros` are 0.1.0-alpha.4; `tmux-mcp` is 0.1.0-alpha.5,
because it already occupied alpha.4. Its own behaviour is unchanged since then:
it is republished to pick up the licence and repository metadata.

### Changed

- Licensed under `MIT OR Apache-2.0` rather than MIT alone, matching Rust
  convention. Releases up to and including `0.1.0-alpha.3` were MIT only.
- `repository` points at this workspace rather than at the Python libtmux
  repository, which does not contain this code.
- Published from CI by a `v*` tag, with no stored API token: the workflow
  exchanges a GitHub OIDC identity for a short-lived crates.io token.

## tmux-mcp 0.1.0-alpha.4 - 2026-08-14

No entries that span crates.

## 0.1.0-alpha.3 - 2026-08-14

No entries that span crates.

## 0.1.0-alpha.2 - 2026-08-14

No entries that span crates.

## 0.1.0-alpha.1 - 2026-08-13

First published alphas. These predate this repository, which was extracted from
the Python libtmux repository without its history, so their changes are not
itemized here.
