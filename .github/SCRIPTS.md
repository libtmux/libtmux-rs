# Scripts

This policy covers the support scripts in this repository: files under
`scripts/`, CI helpers, generators and checkers. It applies the libtmux
decision in [libtmux/docs#323](https://github.com/libtmux/docs/pull/323).

## Keep a script only for a current use

A script stays when this project runs it today: a CI job, a release step, a
code generator, a documentation check, or a maintainer task named in
[CONTRIBUTING.md](CONTRIBUTING.md).

- Parity with Python libtmux or tmuxp is not a use. Delete comparison
  harnesses, oracles, ledgers and generated fixtures, and the CI steps that
  install them.
- A shell step that creates, prepares or stops tmux for an example or test
  means the library lacks an operation. File an issue for the operation and
  remove the step when it lands.
- Delete scripts nothing calls.
- Tooling several ports share lives once, as a shared workflow or action, not
  as a vendored copy.

## Language

Scripts are Python: the library has no Python dependency, and a small typed
script costs less than a second build system for tooling. Declare dependencies
inline (PEP 723) and run scripts with `uv run`.

## Layout and checks

- A script with tests lives in `scripts/<name>/`, with a `README.md` (what it
  does, who runs it, how) and `tests/`. A short single-purpose script may be
  one file with a usage header.
- Scripts pass `ruff format`, `ruff check`, `ty check` and `pytest`, with
  `pytest-asyncio` for async code and doctests collected, in CI.
- Use current language and tool idioms; no shims below the project's floor.
- Before a new or rewritten script merges, a reviewer who did not write it
  checks it against [WRITING.md](WRITING.md) and
  [no-ai-slop](https://github.com/petergyang/no-ai-slop).

## What stays

- GitHub Actions `run:` lines that call one command.
- Test data that is not a comparison, such as workspace files a loader reads.
- `tools/mcp-swap`, which is Rust. It is a workspace member, so `fmt-check`,
  `clippy` with `-D warnings` and `test` cover it, and `swap-test` and `msrv`
  run its suite, which adds no second build system. It keeps a transaction and
  recovery contract, with a lock and an exact revert from the first backup,
  that its own suite covers. A Python copy would be a second implementation of
  that contract. A tool with its own JSONC and TOML editors is a crate, not a
  script. Its README documents the maintainer task that runs it, and
  CONTRIBUTING names it.
