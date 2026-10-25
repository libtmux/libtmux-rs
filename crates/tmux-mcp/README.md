<!-- libtmux-logo -->
<div align="center">
  <picture>
    <source srcset="https://raw.githubusercontent.com/libtmux/libtmux-rs/HEAD/crates/tmux-mcp/assets/logo.svg" type="image/svg+xml">
    <img src="https://raw.githubusercontent.com/libtmux/libtmux-rs/HEAD/crates/tmux-mcp/assets/logo.png" width="128" height="128" alt="libtmux for Rust MCP">
  </picture>
</div>
<!-- /libtmux-logo -->

<div align="center">

# tmux-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server for
[tmux](https://github.com/tmux/tmux), built on
[libtmux](https://docs.rs/libtmux).

</div>

Give an agent typed operations inside the terminal: [inspect][toolset-inspect]
tmux, [arrange][toolset-manage] its objects, [drive][toolset-execute] pane
programs, and [wait][wait_for_text] for observable outcomes.

[Run a command][run_shell_command] and read its exit status, [wait][wait_for_text]
for output you did not start, [follow][capture_since] a pane across turns, and
[find][search_panes] the pane that printed something. For a single
`tmux send-keys`, shelling out is shorter.

> Releases are numbered `0.1.N`, and each release increments `N`. The API is
> not settled: any release may change or remove exported identifiers, and the
> changelog marks each break. Pin an exact version to avoid one. This includes
> the tool surface.

**Contents** — [Quick start](#quick-start) ·
[Connect another client](#connect-another-client) ·
[Choose which tmux it drives](#choose-which-tmux-it-drives) ·
[Run work and wait for it](#run-work-and-wait-for-it) ·
[The tools](#the-tools) ·
[Limiting what a client can do](#limiting-what-a-client-can-do) ·
[What comes back](#what-comes-back) · [Settings](#settings) ·
[What it can reach](#what-it-can-reach) ·
[Using it from Rust](#using-it-from-rust) · [Packages](#packages) ·
[What it leaves out](#what-it-leaves-out) ·
[Support and status](#support-and-status) ·
[Documentation](#documentation)

## Quick start

You need tmux 3.3a or newer on `PATH`, and Rust 1.88 to build.

Basic listing, capture, direct input, layout and channel tools work on 3.3a.
`paste_text` needs 3.4 for safe named-buffer cleanup; an empty paste without
Enter needs no buffer. `wait_for_text` streams pane output on tmux 3.7 or
newer and polls the visible screen with `capture-pane` below that, chosen from
the daemon's version before any control client is attempted; a match that
appears and scrolls off between two reads is missed on the polling path.
`capture_since` and `run_shell_command` need tmux 3.7 or newer because they
acquire a control client; unavailable operations return an
`unsupported_version` refusal before control acquisition or paste staging.

```console
$ cargo install tmux-mcp
```

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) fetches a
prebuilt binary for Linux or macOS instead, when the release carries one:

```console
$ cargo binstall tmux-mcp
```

Either puts a `tmux-mcp` binary on your path. A client starts it as a
subprocess and speaks MCP on its stdin and stdout. Check that the shell finds
it:

```console
$ tmux-mcp --version
```

### `claude mcp add`: tell Claude Code about it

```console
$ claude mcp add tmux -- tmux-mcp
```

Then ask the client for something only tmux can do, such as "create a tmux
session called demo and tell me what panes it has".

## Connect another client

Every client starts the same command, `tmux-mcp`. [Connect an MCP
client][connect] on libtmux.org covers startup and shutdown.

### `codex mcp add`: Codex CLI

```console
$ codex mcp add tmux -- tmux-mcp
```

### `gemini mcp add`: Gemini CLI

Note the missing `--`: this command takes the server command as a positional
argument, so a `--` before it would be parsed as the end of the arguments and
nothing would be registered.

```console
$ gemini mcp add tmux tmux-mcp
```

### `grok mcp add`: Grok CLI

```console
$ grok mcp add tmux tmux-mcp
```

### `claude_desktop_config.json`: Claude Desktop

Add to `claude_desktop_config.json` — under `~/Library/Application
Support/Claude/` on macOS, `%APPDATA%\Claude\` on Windows:

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp"
    }
  }
}
```

### `.cursor/mcp.json`: Cursor

Add to `.cursor/mcp.json` in a project, or `~/.cursor/mcp.json` for every
project:

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp"
    }
  }
}
```

### `.vscode/mcp.json`: VS Code

```json
{
  "servers": {
    "tmux": {
      "type": "stdio",
      "command": "tmux-mcp"
    }
  }
}
```

### `~/.codeium/windsurf/mcp_config.json`: Windsurf

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp"
    }
  }
}
```

### `~/.config/zed/settings.json`: Zed

Open it with `zed: open settings`:

```json
{
  "context_servers": {
    "tmux": {
      "command": {
        "path": "tmux-mcp",
        "args": []
      }
    }
  }
}
```

### `cline_mcp_settings.json`: Cline and Roo Code

Open the MCP Servers panel and choose Configure MCP Servers, which opens
`cline_mcp_settings.json`. Add:

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp"
    }
  }
}
```

### `~/.config/goose/config.yaml`: Goose

```yaml
extensions:
  tmux:
    enabled: true
    type: stdio
    cmd: tmux-mcp
    args: []
```

### `~/.config/opencode/opencode.json`: opencode

Note that `command` is an array here, and that the table is `mcp` rather than
`mcpServers`:

```json
{
  "mcp": {
    "tmux": {
      "type": "local",
      "command": ["tmux-mcp"]
    }
  }
}
```

### `~/.gemini/config/mcp_config.json`: Antigravity

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp"
    }
  }
}
```

### JetBrains AI Assistant: a stdio server

Settings → Tools → AI Assistant → Model Context Protocol → Add, then choose a
stdio server with command `tmux-mcp`.

### Passing options: a socket or a toolset

Every client above starts the same command, so a socket or a toolset goes
where the client keeps a command's arguments and environment. Claude Code
takes both on the command line. This offers the read tools only, on the tmux
socket named `work`:

```console
$ claude mcp add tmux \
    --env LIBTMUX_TOOLSETS=inspect \
    -- tmux-mcp --socket-name work
```

A JSON client takes the same two settings as `args` and `env`:

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp",
      "args": ["--socket-name", "work"],
      "env": { "LIBTMUX_TOOLSETS": "inspect" }
    }
  }
}
```

### Anything else: the command and no arguments

Any client that runs a stdio MCP server takes the same two pieces: the command
`tmux-mcp`, and no arguments. That default selects a dedicated product socket
and a minimal tmux configuration.

## Choose which tmux it drives

The choice is made once, when the server starts, and no tool call can change
it. [Settings](#settings) lists every flag and variable, and
[the contract][contract-server] has the precedence rules.

### [`--socket-name`][guide-existing-server]: pick a socket by name

With no flag, the server drives a tmux server of its own, on a socket named
`libtmux-mcp`, started with a minimal configuration the package ships. None of
its tools can name the sessions you already have.

To work with a tmux server you already run, name its socket:

```console
$ tmux-mcp --socket-name work
```

`-L` is the short form. Several processes started without arguments share the
`libtmux-mcp` daemon, and `tmux -L libtmux-mcp kill-server` stops it. [The
contract][contract-server] says which process stops it on its own.

### [`--socket`][guide-existing-server]: pick a socket by path

```console
$ tmux-mcp --socket /tmp/tmux-1000/work
```

`-S` is the short form. A socket name and a path are mutually exclusive.
[`LIBTMUX_SOCKET_PATH`][contract-server] and
[`LIBTMUX_SOCKET`][contract-server] make the same choices from the
environment, which is how a client's configuration sets them, and
[`LIBTMUX_TMUX_CONFIG`][contract-server] names the tmux configuration for a
daemon the server starts.

## Run work and wait for it

The server earns its keep once an agent has to wait, look, or avoid breaking
the terminal it works in. Reading a pane goes through tmux's control mode
rather than screen captures, so output that scrolled past between calls is
still seen.

### [`run_shell_command`][run_shell_command]: run a command and read its exit status

[`run_shell_command`][run_shell_command] sends the command, waits for it to
finish, and answers with its exit status and output:

> **You:** Run the API tests in the `api` session and tell me what broke.
>
> **Agent:** [`run_shell_command`][run_shell_command] in pane `%3` finished
> with exit status 1. Two failures, both in `test_auth.py` —
> `test_token_refresh` and `test_expired_session`. Want me to open them?

The agent waited for the command, read its real exit status, and got the
output the command wrote: no prompt, no echo, and nothing lost to scrollback.

[`examples/run_and_wait`][example-run-and-wait] makes the same call through
the tool a client calls, on a tmux of its own under `/tmp/libtmux-rs-test/`:

```console
$ cargo run --example run_and_wait
```

It runs `ls /no/such/path` and prints the nonzero exit status `ls` returned.
Reaching the deadline ends the waiting and leaves the pane command running, so
inspect the pane before sending more input.

### [`wait_for_text`][wait_for_text]: wait for what a pane prints

[`wait_for_text`][wait_for_text] watches the pane's output stream for a
pattern, with stop patterns for the failures you already know. On tmux 3.7 or
newer a line that scrolls past between looks is still seen, because it reads
the stream. Below 3.7 it polls the visible screen, and a line that scrolls off
between two reads is missed.

The same example starts a background job that prints `server is ready` a second
later, waits for that line, and prints `waited: Matched`.

[`run_shell_command`][run_shell_command], [`wait_for_text`][wait_for_text], and
[`capture_since`][capture_since] return the output stream with escape
sequences removed, so a line redrawn in place repeats once per redraw.
[`capture_pane`][capture_pane] returns what the screen displays.

### [`capture_since`][capture_since]: follow a pane across turns

[`capture_since`][capture_since] returns only what is new since the cursor it
gave you last time, and says `missed: true` if anything was dropped:

```console
$ cargo run --example follow
```

[`examples/follow`][example-follow] pushes 200 lines through a 24-row pane
between two calls. The second call returns all of them, and the example prints
`saw 200 of 200 lines, missed: false`.

### [`call_read_tools_batch`][call_read_tools_batch]: several reads in one call

One batch lists topology, snapshots a pane, and reads an option, keeping each
child's complete MCP envelope:

```json
{
  "operations": [
    {"tool": "list_sessions", "arguments": {}},
    {"tool": "snapshot_pane", "arguments": {"pane": "%3"}},
    {"tool": "show_option", "arguments": {"name": "status"}}
  ],
  "on_error": "continue"
}
```

Each result row keeps its own `content`, `structuredContent`, `_meta`, and
`isError`. A reply over the size cap is trimmed, and every trimmed row says so.
[The contract][contract-batches] gives the limits.

### Keep reads small

- **Finding where something is.** [`search_panes`][search_panes] matches
  across every pane at once and reports the pane and line. The listing tools
  read names and commands, and cannot see what a terminal is showing.
- **Asking about layout.** [`find_pane_by_position`][find_pane_by_position]
  finds the pane touching a named window corner.
  [`snapshot_pane`][snapshot_pane] adds geometry, cursor, and mode state to the
  visible content.
- **Reading tmux variables.**
  [`get_tmux_variables`][get_tmux_variables] takes validated variable names
  and builds the `#{variable}` references itself.
- **Arranging a window.** [`select_layout`][select_layout] takes saved and
  named tmux layouts, and returns the layout tmux applied.

## The tools

The tools belong to unordered toolsets, and an operator grants or withholds a
toolset as a whole. Each tool carries one machine-readable capability record.
Registration, descriptions, annotations, selection, and
[`tmux://capabilities`] all read it. The
[generated tool reference](TOOLS.md) records every route and capability
directly from that registry, and the [tool pages][tools] publish it.

| Toolset | Intent | Tools |
|---|---|---|
| [`inspect`][toolset-inspect] (18) | Read bounded tmux state and terminal output | [`call_read_tools_batch`][call_read_tools_batch], [`capture_pane`][capture_pane], [`capture_since`][capture_since], [`find_pane_by_position`][find_pane_by_position], [`get_pane_info`][get_pane_info], [`get_server_info`][get_server_info], [`get_session_info`][get_session_info], [`get_tmux_variables`][get_tmux_variables], [`get_window_info`][get_window_info], [`list_panes`][list_panes], [`list_sessions`][list_sessions], [`list_windows`][list_windows], [`search_panes`][search_panes], [`show_environment`][show_environment], [`show_hooks`][show_hooks], [`show_option`][show_option], [`snapshot_pane`][snapshot_pane], [`wait_for_text`][wait_for_text] |
| [`manage`][toolset-manage] (13) | Change tmux objects without starting a process | [`move_window`][move_window], [`rename_session`][rename_session], [`rename_window`][rename_window], [`resize_pane`][resize_pane], [`resize_window`][resize_window], [`select_layout`][select_layout], [`select_pane`][select_pane], [`select_window`][select_window], [`set_mouse_enabled`][set_mouse_enabled], [`set_pane_title`][set_pane_title], [`signal_channel`][signal_channel], [`swap_pane`][swap_pane], [`wait_for_channel`][wait_for_channel] |
| [`execute`][toolset-execute] (9) | Start configured processes or drive pane programs | [`create_session`][create_session], [`create_window`][create_window], [`paste_text`][paste_text], [`respawn_pane`][respawn_pane], [`run_shell_command`][run_shell_command], [`send_keys`][send_keys], [`send_keys_batch`][send_keys_batch], [`set_synchronize_panes`][set_synchronize_panes], [`split_window`][split_window] |
| [`teardown`][toolset-teardown] (5) | Delete tmux state | [`clear_pane_scrollback`][clear_pane_scrollback], [`kill_pane`][kill_pane], [`kill_session`][kill_session], [`kill_window`][kill_window], [`set_history_limit`][set_history_limit] |

### [`send_keys`][send_keys]: typing into panes

[`send_keys`][send_keys], [`send_keys_batch`][send_keys_batch],
[`paste_text`][paste_text] and [`run_shell_command`][run_shell_command] type
into panes, and what they type runs with your user's permissions. Before one
acts, the server looks at the panes the input would reach. A failed check
refuses the input and sends nothing.

- **Synchronized panes.** [`send_keys`][send_keys] and
  [`send_keys_batch`][send_keys_batch] also reach every other synchronized pane
  in the window. [`paste_text`][paste_text] reaches only the pane you name, and
  [`run_shell_command`][run_shell_command] refuses when input would reach more
  than one.
- **Panes it refuses.** A dead pane, one with input turned off or in a mode
  such as copy mode, one a person has attached on screen, one reserved by a
  running [`run_shell_command`][run_shell_command], and the pane the server
  runs in.
- **The shell.** [`run_shell_command`][run_shell_command] needs a pane running
  a trusted POSIX-compatible pane shell that the server recognises.
- **No lock.** The checks look; they do not lock. A person can attach, or a
  pane can change, between the check and the keystrokes.
- **No delivery receipt.** A reply lists the panes the input was meant for. It
  does not prove the input arrived.

[The contract][contract-pane-input] has the full checks, the reservation rules
of [`run_shell_command`][run_shell_command], and the batch limits.

## Limiting what a client can do

### [`LIBTMUX_TOOLSETS`]: choose the toolsets

It takes toolset names separated by commas, in any order, and replaces the
default. An empty string offers no toolsets. Empty tokens and unknown names stop
startup.

The default dedicated socket gets all four toolsets only when `tmux-mcp` can
establish minimal-configuration provenance. An existing dedicated socket, an
explicit socket, or an explicit tmux configuration defaults to `inspect`,
`manage`, and `execute`, so select `teardown` there by name.

```console
$ LIBTMUX_TOOLSETS=inspect,execute tmux-mcp
```

### [`LIBTMUX_TOOLS`] and [`LIBTMUX_EXCLUDE_TOOLS`]: name exact tools

[`LIBTMUX_TOOLS`] adds named tools after toolset expansion.
[`LIBTMUX_EXCLUDE_TOOLS`] removes named tools last, including aggregate nested
authority. Exclusions always win, and hidden tools are neither listed nor
callable.

```console
$ LIBTMUX_TOOLSETS=inspect \
    LIBTMUX_TOOLS=send_keys \
    LIBTMUX_EXCLUDE_TOOLS=capture_pane \
    tmux-mcp
```

An unknown tool, an unknown toolset, or an empty item in any nonempty list
stops startup before tmux is opened, so a typo never widens the surface.
Naming [`call_read_tools_batch`][call_read_tools_batch] with an empty toolset
list offers only the batch, and an excluded operation leaves its nested
authority:

```console
$ LIBTMUX_TOOLSETS='' \
    LIBTMUX_TOOLS=call_read_tools_batch \
    LIBTMUX_EXCLUDE_TOOLS=show_environment \
    tmux-mcp
```

### [`examples/surface`][example-surface]: preview what a client would be offered

It prints a selection's tools, their descriptions, answer fields, and
capability records, without starting a tmux server. With no arguments it
prints the surface the default selection offers:

```console
$ cargo run --example surface
```

It takes the toolsets, the included tools, and the excluded tools as arguments.
This previews the aggregate-only selection above:

```console
$ cargo run --example surface -- '' call_read_tools_batch show_environment
```

### [`examples/refused`][example-refused]: what a client hears for a tool you left out

The example serves only `inspect`, calls [`send_keys`][send_keys] anyway, and
prints the refusal a client gets:

```console
$ cargo run --example refused
```

The refusal is a JSON-RPC invalid-params error. It names why the tool is
missing and the setting an operator would change to offer it.

### [`tmux://capabilities`]: what each tool says about itself

Each tool describes what it reaches and changes, under
`_meta["com.git-pull.libtmux-mcp/capability"]`. The server's one resource,
[`tmux://capabilities`], reports the same records beside the selected socket and
where that choice came from. [The contract][contract-resource] lists the
fields.

| URI | Holds |
|---|---|
| [`tmux://capabilities`] | The frozen effective tool surface, boundary, and connection provenance |

### Asking first: annotations

The server has no confirmation policy. A client decides whether to ask a
person before a call from each tool's four MCP annotations, which derive from
the tool's capability record. Selecting tools shapes what a client is offered
and leaves the tmux user's authority as it was.
[The contract][contract-asking] maps tools to annotations.

## What comes back

Every tool publishes an output schema and answers with structured content, so
an agent reads fields rather than parsing text:

```json
{"pane": "%3", "outcome": "completed", "exit_status": 0,
 "output": "ok\n", "bytes": 812, "truncated": false}
```

Every error carries the same three fields, so an agent decides what to do next
without reading prose:

```json
{"kind": "object_gone", "retryable": false, "stale": true}
```

- **`stale`** means the target is gone, so look again before you retry.
- **`retryable`** means the same call is safe to repeat unchanged.
- **`partial_effect`** (a `kind`) means tmux accepted part of a multi-step call
  before a later step failed. Inspect the current state first.

## Settings

Flags choose the tmux server once, at startup. `tmux-mcp --help` prints them.

| Flag | Meaning |
|---|---|
| `-S`, `--socket <PATH>` | The tmux server on this socket path |
| `-L`, `--socket-name <NAME>` | The tmux server with this socket name |
| `-V`, `--version` | Print the version and exit |
| `-h`, `--help` | Print the flags and variables and exit |

The environment sets the rest. A flag wins over its variable.

| Variable | Does |
|---|---|
| [`LIBTMUX_TOOLSETS`] | Selects any unordered subset of the toolsets |
| [`LIBTMUX_TOOLS`] | Adds exact tool names after toolset expansion |
| [`LIBTMUX_EXCLUDE_TOOLS`] | Removes exact names after every inclusion |
| [`LIBTMUX_ENVIRONMENT_VALUES`] | Names the tmux environment variables whose values tools may return; none by default |
| [`LIBTMUX_SOCKET_PATH`][contract-server] | Socket path, as `-S`; ignored when `-S` or `-L` is given |
| [`LIBTMUX_SOCKET`][contract-server] | Socket name, as `-L`; not together with [`LIBTMUX_SOCKET_PATH`][contract-server] |
| [`LIBTMUX_TMUX_CONFIG`][contract-server] | Absolute path of a tmux configuration for a daemon the server starts |

A malformed setting stops startup. The protocol runs on stdout, so anything
the binary prints for a person goes to stderr, where an MCP client collects it
as the server's log.

## What it can reach

One process is bound to one tmux socket, and no tool takes a socket argument.
That limits which tmux objects the tools can name. It does not confine
filesystem, network, credential, or process access, and filtering the tools
does not authorize anything.

- **Input runs with your authority.** Execute tools type into panes, and the
  pane program runs what arrives.
- **Environment values stay home.** [`show_environment`][show_environment]
  lists names and whether each is set, and withholds the value.
  [`LIBTMUX_ENVIRONMENT_VALUES`] names, at startup, the variables whose values
  tools may return.
- **Pane output may be sensitive.** Output and tmux metadata can hold secrets
  or untrusted text, and hooks can hold executable configuration.

This lets the tools return the values of `TERM` and `LANG`, and no others:

```console
$ LIBTMUX_ENVIRONMENT_VALUES=TERM,LANG tmux-mcp
```

[The contract][contract-trust] covers the shell requirements of
[`run_shell_command`][run_shell_command] and what the server trusts.

## Using it from Rust

### [`TmuxTools`]: the tool surface as a type

The tool surface is a type, so a program can freeze a [`Selection`] in code,
serve it over a transport other than stdio, or call a tool directly. The
[API reference][api-reference] and [docs.rs][docs-rs] cover it, and
[`examples/readonly`][example-readonly] serves the `inspect` toolset:

```console
$ cargo run --example readonly
```

It speaks MCP on stdin and stdout, so talk to it from a client, or end it with
EOF.

### [`examples/budget`][example-budget]: measure what a client downloads

A client fetches every tool's schema at `tools/list`, which is the constraint
that decides whether a tool earns its place. The example measures it for three
selections, and names the tools that cost the most:

```console
$ cargo run --example budget
```

## Packages

### [`tmux-mcp`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp): the server

This crate: the `tmux-mcp` command a client launches, and the library behind
it. [`TmuxTools`] is the tool surface, and [`Selection`] is the toolset
choice.

### [`libtmux`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux): the client under it

The async tmux client and object model that every tool calls. [Its
README][libtmux-guide] covers filtering, waiting, and control mode.

### [`tmux-workspace`](https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-workspace): sessions to inspect

Loads a tmuxp-style YAML workspace on a named socket, which an agent can then
read through `tmux-mcp -L` with the same socket name. [Its
README][workspace-agent] has the two commands.

## What it leaves out

- **Per-call socket selection.** One process serves one socket. Run one MCP
  process per socket, and ask [`get_server_info`][get_server_info] for the
  server a process is pinned to.
- **Host commands.** No public tool runs a caller-authored command on the host.
  Spawn tools start only the configured process.
- **A consent policy.** A client decides approval from each tool's
  [annotations](#asking-first-annotations).
- **Pane modes.** Copy mode and the other modes belong to the person attached.
  Read with [`capture_pane`][capture_pane], [`snapshot_pane`][snapshot_pane],
  [`search_panes`][search_panes], or [`capture_since`][capture_since].
- **Live resources.** [`tmux://capabilities`] is the only resource. Read tmux
  state through the inspect tools.
- **Background jobs, prompts, and idle heuristics.** The
  [migration table][migration] says what replaced each.

## Support and status

- **Versions.** Releases are numbered `0.1.N`, and any release may change the
  tool surface. Pin an exact version.
- **tmux 3.3a or newer**, on `PATH`.
- **Rust 1.88 to build.** The libraries under it build on 1.85.
- **Linux, including WSL.** CI runs the workspace's tests on Linux. macOS
  is supported on a best-effort basis; the release workflow builds binaries
  there, and CI does not run the tests.
- **Windows.** Native Windows is unsupported because tmux is unavailable
  there.

## Documentation

| | |
|---|---|
| [Tool pages][tools] | Every tool's arguments and results, one page each |
| [Connect a client][connect] | Install, choose a socket, check the connection |
| [Tool selection][topic-tool-selection] | Toolsets, inclusions, exclusions |
| [API reference][api-reference] | The Rust API on libtmux.org |
| [docs.rs][docs-rs] | The Rust API as rustdoc |
| [`TOOLS.md`](TOOLS.md) | The generated tool reference |
| [`docs/contracts.md`][contracts] | What the server promises |
| [`docs/migration.md`][migration] | Moving from an earlier alpha |
| [`examples/`][examples] | Runnable programs against a tmux of their own |

## Development

```console
$ cargo test -p tmux-mcp
```

The tests drive a real tmux on an isolated socket, so tmux must be on `$PATH`.
They pass every command and client configuration on this page through the
binary's own argument and setting validation, and `just examples` runs each
example.

## Related

- [libtmux](https://docs.rs/libtmux) — the typed tmux client underneath
- [libtmux-mcp](https://github.com/tmux-python/libtmux-mcp) — the Python server
  this one learned its discoverability habits from

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.

[tools]: https://libtmux.org/en/rs/latest/mcp/tools/
[connect]: https://libtmux.org/en/rs/latest/mcp/guides/connect-client/
[api-reference]: https://libtmux.org/en/rs/latest/mcp/reference/
[docs-rs]: https://docs.rs/tmux-mcp/latest/tmux_mcp/
[topic-tool-selection]: https://libtmux.org/en/rs/latest/mcp/topics/tool-selection/
[guide-existing-server]: https://libtmux.org/en/rs/latest/mcp/guides/connect-client/#connect-to-an-existing-server
[contracts]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/contracts.md
[contract-pane-input]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/contracts.md#pane-input
[contract-batches]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/contracts.md#batches
[contract-resource]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/contracts.md#the-capabilities-resource
[contract-asking]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/contracts.md#asking-first
[contract-server]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/contracts.md#choosing-a-server
[contract-trust]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/contracts.md#trust-boundary
[migration]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/docs/migration.md
[examples]: https://github.com/libtmux/libtmux-rs/tree/master/crates/tmux-mcp/examples
[example-run-and-wait]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/examples/run_and_wait.rs
[example-follow]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/examples/follow.rs
[example-surface]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/examples/surface.rs
[example-refused]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/examples/refused.rs
[example-readonly]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/examples/readonly.rs
[example-budget]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-mcp/examples/budget.rs
[libtmux-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md
[workspace-agent]: https://github.com/libtmux/libtmux-rs/blob/master/crates/tmux-workspace/README.md#let-an-agent-watch-it
[toolset-inspect]: https://libtmux.org/en/rs/latest/mcp/topics/tool-selection/#toolsets-inspect
[toolset-manage]: https://libtmux.org/en/rs/latest/mcp/topics/tool-selection/#toolsets-manage
[toolset-execute]: https://libtmux.org/en/rs/latest/mcp/topics/tool-selection/#toolsets-execute
[toolset-teardown]: https://libtmux.org/en/rs/latest/mcp/topics/tool-selection/#toolsets-teardown
[call_read_tools_batch]: https://libtmux.org/en/rs/latest/mcp/tools/call_read_tools_batch/
[capture_pane]: https://libtmux.org/en/rs/latest/mcp/tools/capture_pane/
[capture_since]: https://libtmux.org/en/rs/latest/mcp/tools/capture_since/
[clear_pane_scrollback]: https://libtmux.org/en/rs/latest/mcp/tools/clear_pane_scrollback/
[create_session]: https://libtmux.org/en/rs/latest/mcp/tools/create_session/
[create_window]: https://libtmux.org/en/rs/latest/mcp/tools/create_window/
[find_pane_by_position]: https://libtmux.org/en/rs/latest/mcp/tools/find_pane_by_position/
[get_pane_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_pane_info/
[get_server_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_server_info/
[get_session_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_session_info/
[get_tmux_variables]: https://libtmux.org/en/rs/latest/mcp/tools/get_tmux_variables/
[get_window_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_window_info/
[kill_pane]: https://libtmux.org/en/rs/latest/mcp/tools/kill_pane/
[kill_session]: https://libtmux.org/en/rs/latest/mcp/tools/kill_session/
[kill_window]: https://libtmux.org/en/rs/latest/mcp/tools/kill_window/
[list_panes]: https://libtmux.org/en/rs/latest/mcp/tools/list_panes/
[list_sessions]: https://libtmux.org/en/rs/latest/mcp/tools/list_sessions/
[list_windows]: https://libtmux.org/en/rs/latest/mcp/tools/list_windows/
[move_window]: https://libtmux.org/en/rs/latest/mcp/tools/move_window/
[paste_text]: https://libtmux.org/en/rs/latest/mcp/tools/paste_text/
[rename_session]: https://libtmux.org/en/rs/latest/mcp/tools/rename_session/
[rename_window]: https://libtmux.org/en/rs/latest/mcp/tools/rename_window/
[resize_pane]: https://libtmux.org/en/rs/latest/mcp/tools/resize_pane/
[resize_window]: https://libtmux.org/en/rs/latest/mcp/tools/resize_window/
[respawn_pane]: https://libtmux.org/en/rs/latest/mcp/tools/respawn_pane/
[run_shell_command]: https://libtmux.org/en/rs/latest/mcp/tools/run_shell_command/
[search_panes]: https://libtmux.org/en/rs/latest/mcp/tools/search_panes/
[select_layout]: https://libtmux.org/en/rs/latest/mcp/tools/select_layout/
[select_pane]: https://libtmux.org/en/rs/latest/mcp/tools/select_pane/
[select_window]: https://libtmux.org/en/rs/latest/mcp/tools/select_window/
[send_keys]: https://libtmux.org/en/rs/latest/mcp/tools/send_keys/
[send_keys_batch]: https://libtmux.org/en/rs/latest/mcp/tools/send_keys_batch/
[set_history_limit]: https://libtmux.org/en/rs/latest/mcp/tools/set_history_limit/
[set_mouse_enabled]: https://libtmux.org/en/rs/latest/mcp/tools/set_mouse_enabled/
[set_pane_title]: https://libtmux.org/en/rs/latest/mcp/tools/set_pane_title/
[set_synchronize_panes]: https://libtmux.org/en/rs/latest/mcp/tools/set_synchronize_panes/
[show_environment]: https://libtmux.org/en/rs/latest/mcp/tools/show_environment/
[show_hooks]: https://libtmux.org/en/rs/latest/mcp/tools/show_hooks/
[show_option]: https://libtmux.org/en/rs/latest/mcp/tools/show_option/
[signal_channel]: https://libtmux.org/en/rs/latest/mcp/tools/signal_channel/
[snapshot_pane]: https://libtmux.org/en/rs/latest/mcp/tools/snapshot_pane/
[split_window]: https://libtmux.org/en/rs/latest/mcp/tools/split_window/
[swap_pane]: https://libtmux.org/en/rs/latest/mcp/tools/swap_pane/
[wait_for_channel]: https://libtmux.org/en/rs/latest/mcp/tools/wait_for_channel/
[wait_for_text]: https://libtmux.org/en/rs/latest/mcp/tools/wait_for_text/

[`tmux://capabilities`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-resources-capabilities/
[`LIBTMUX_TOOLSETS`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-toolsets_env/
[`LIBTMUX_TOOLS`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-tools_env/
[`LIBTMUX_EXCLUDE_TOOLS`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-exclude_tools_env/
[`LIBTMUX_ENVIRONMENT_VALUES`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-environment_values_env/
[`Selection`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-selection/
[`TmuxTools`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-src-tmuxtools/
