# tmux-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server for
[tmux](https://github.com/tmux/tmux), built on
[libtmux](https://docs.rs/libtmux).

> [!WARNING]
> **Alpha.** The tool surface changes between releases, including in ways that
> will not be called out as breaking, because nothing here is stable yet.
> `cargo install` will not pick a prerelease unless asked, so the install
> command below names the version. Feedback welcome.

Give an agent typed operations inside the terminal: inspect tmux, arrange its
objects, drive pane programs, and wait for observable outcomes.

Reading a pane goes through tmux's control mode rather than screen captures,
so output that scrolled past between calls is still seen, and
`run_shell_command` reports a real exit status instead of leaving an agent to
guess from text.

## Requirements

tmux 3.2a or newer, on `$PATH`. Rust 1.88 to build.

## Install

```console
$ cargo install tmux-mcp --version 0.1.0-alpha.10
```

That puts a `tmux-mcp` binary on your path. It speaks MCP on stdin and stdout,
so every client below is really the same thing: run `tmux-mcp`.

### Claude Code

```console
$ claude mcp add tmux -- tmux-mcp
```

### Codex CLI

```console
$ codex mcp add tmux -- tmux-mcp
```

### Gemini CLI

Note the missing `--`: this command takes the server command as a positional
argument, so a `--` before it would be parsed as the end of the arguments and
nothing would be registered.

```console
$ gemini mcp add tmux tmux-mcp
```

### Grok CLI

```console
$ grok mcp add tmux tmux-mcp
```

### Claude Desktop

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

### Cursor

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

### VS Code

Add to `.vscode/mcp.json`:

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

### Windsurf

Add to `~/.codeium/windsurf/mcp_config.json`:

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp"
    }
  }
}
```

### Zed

Add to `~/.config/zed/settings.json`, or open it with `zed: open settings`:

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

### Cline and Roo Code

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

### Goose

Add to `~/.config/goose/config.yaml`:

```yaml
extensions:
  tmux:
    enabled: true
    type: stdio
    cmd: tmux-mcp
    args: []
```

### opencode

Add to `~/.config/opencode/opencode.json`. Note that `command` is an array
here, and that the table is `mcp` rather than `mcpServers`:

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

### Antigravity

Add to `~/.gemini/config/mcp_config.json`:

```json
{
  "mcpServers": {
    "tmux": {
      "command": "tmux-mcp"
    }
  }
}
```

### JetBrains AI Assistant

Settings → Tools → AI Assistant → Model Context Protocol → Add, then choose a
stdio server with command `tmux-mcp`.

### Anything else

Any client that runs a stdio MCP server takes the same two pieces: the command
`tmux-mcp`, and no arguments. That default selects a dedicated product socket
and a minimal tmux configuration.

## What it offers

Forty-five tools belong to the unordered `inspect`, `manage`, `execute`, and
`teardown` toolsets. Each native route carries one machine-readable capability
record; registration, descriptions, annotations, selection, and
`tmux://capabilities` all read that record. The
[generated tool reference](TOOLS.md) records every route and capability
directly from that registry.

| Toolset | Intent | Tools |
|---|---|---|
| `inspect` (18) | Read bounded tmux state and terminal output | `call_read_tools_batch`, `capture_pane`, `capture_since`, `find_pane_by_position`, `get_pane_info`, `get_server_info`, `get_session_info`, `get_tmux_variables`, `get_window_info`, `list_panes`, `list_sessions`, `list_windows`, `search_panes`, `show_environment`, `show_hooks`, `show_option`, `snapshot_pane`, `wait_for_text` |
| `manage` (14) | Change tmux objects without starting a process | `move_window`, `rename_session`, `rename_window`, `resize_pane`, `resize_window`, `select_layout`, `select_pane`, `select_window`, `set_history_limit`, `set_mouse_enabled`, `set_pane_title`, `signal_channel`, `swap_pane`, `wait_for_channel` |
| `execute` (9) | Start configured processes or drive pane programs | `create_session`, `create_window`, `paste_text`, `respawn_pane`, `run_shell_command`, `send_keys`, `send_keys_batch`, `set_synchronize_panes`, `split_window` |
| `teardown` (4) | Delete tmux state | `clear_pane_scrollback`, `kill_pane`, `kill_session`, `kill_window` |

`set_synchronize_panes` changes the window default; individual pane overrides
determine the effective configured recipient cohort. `send_keys` observes that
cohort immediately before input and refuses the whole call if any configured
pane is dead, input-disabled, in a tmux mode, attended by a non-control client,
reserved by an active MCP run, or may be the inherited caller. Malformed state
fails closed. `send_keys_batch` repeats the complete check for each executed
row.
Returned pane IDs describe configured membership and do not prove delivery.
`paste_text` applies the same refusals to its named target before buffer
creation. Text and optional Enter share one private buffer; an empty paste
without Enter stays buffer-free. The tool repeats the target check immediately
before target-only paste and deletes the buffer after refusal or delivery.
Synchronized input never expands paste.
`run_shell_command` requires a single configured recipient and reserves its
resolved server and pane process-wide until completion or pane closure is
proved. Every MCP pane-input route observes that reservation.
`call_read_tools_batch` accepts at most 16 enabled inspect operations and caps
the complete JSON-RPC response line, including its request ID and newline, at
1,000,000 bytes. Truncated payloads and omitted bytes are explicit, and every
executed row remains present.
A serialized request ID may use at most 512 KiB. A larger ID receives a bounded
invalid-request response before tool dispatch.
`on_error` is either `stop` or `continue`. Its one client approval covers every
nested name in its schema; inner tools do not receive separate approval.

The capability row is not a second hand-maintained catalog. The same row a
client receives under `_meta["com.git-pull.libtmux-mcp/capability"]` carries the
native input and output schemas, process reach, effect and output sets,
schema-keyed input literalization, annotations, and any nested authority. The
native definition also classifies every input sink, but that validation detail
is not duplicated on the wire. For example, `get_tmux_variables.names` is
reported as `validated-variable-name`; it is not falsely described as escaped
literal text.

## Resources

`tmux://capabilities` reports the startup-frozen effective surface, selected
socket, socket provenance, direct process reach, tmux effects, output classes,
schema-keyed input literalization, future-input amplification, nested
authority, and whole-call MCP annotations. No dynamic resource templates are
registered.

| URI | Holds |
|---|---|
| `tmux://capabilities` | The frozen effective tool surface, boundary, and connection provenance |

The report also makes the outer boundary explicit. One process has one socket,
no call can select another socket, no tool executes a host command, and the
resource is static. Its connection object supplies the socket selector,
resolved path, attach command, daemon state, and configuration provenance:

```json
{
  "boundary": {
    "oneSocketPerProcess": true,
    "perCallSocketSelection": false,
    "hostCommandExecution": false,
    "dynamicResources": false
  },
  "connection": {
    "socketSelector": "name:libtmux-mcp",
    "socketProvenance": "default-dedicated",
    "serverState": "created",
    "configurationProvenance": "minimal"
  }
}
```

Read this resource when a client needs to explain its authority or cache the
effective surface. Read tmux state through inspect tools; there are no live
session, window, pane, or output resources to drift away from the tool catalog.

## Toolset selection

The default dedicated socket gets all four toolsets only when `tmux-mcp` can
establish minimal-configuration provenance. An existing dedicated socket, an
explicit socket, or an explicit tmux configuration defaults to `inspect`,
`manage`, and `execute`; select `teardown` explicitly there.

`LIBTMUX_TOOLSETS` replaces the default with a comma-separated unordered set.
An empty string offers no toolsets. Empty tokens and unknown names stop startup.

```console
$ LIBTMUX_TOOLSETS=inspect,execute tmux-mcp
```

`LIBTMUX_TOOLS` adds named tools after toolset expansion.
`LIBTMUX_EXCLUDE_TOOLS` removes named tools last, including aggregate nested
authority. Exclusions always win. Hidden tools are neither listed nor callable.

```console
$ LIBTMUX_TOOLSETS=inspect \
    LIBTMUX_TOOLS=send_keys \
    LIBTMUX_EXCLUDE_TOOLS=capture_pane \
    tmux-mcp
```

Named inclusion also works with the zero-toolset subset. This exposes only the
aggregate while retaining its 16 eligible inspect operations as nested
authority; an exclusion removes the named operation from both authority and
the generated `oneOf` schema:

```console
$ LIBTMUX_TOOLSETS='' \
    LIBTMUX_TOOLS=call_read_tools_batch \
    LIBTMUX_EXCLUDE_TOOLS=show_environment \
    tmux-mcp
```

An excluded tool cannot be restored by naming it in `LIBTMUX_TOOLS`. Unknown
tool names, unknown toolsets, and empty elements in any nonempty list fail
before tmux is opened, so a typo never widens the surface.

The retired `LIBTMUX_SAFETY` and `TMUX_MCP_SAFETY` settings stop startup with
a migration error instead of silently widening or narrowing the surface.

### Moving from an earlier alpha

Retired names are not hidden aliases. No prompts are registered; their recipes
now run in the client. Update existing client calls with this mapping:

| Earlier surface | Current path |
|---|---|
| `--safety`, `LIBTMUX_SAFETY`, `TMUX_MCP_SAFETY` | Select unordered `LIBTMUX_TOOLSETS`, then exact inclusions or exclusions. |
| `--confirm`, `--no-confirm`, `TMUX_MCP_CONFIRM` | Remove them. The server has no confirmation policy; clients decide approval from each tool's MCP annotations. |
| `list_session_windows`, `list_window_panes` | Use `list_windows` or `list_panes`, then filter the returned stable `session_id` or `window_id`. |
| `describe` | Use `get_server_info`, `get_session_info`, `get_window_info`, or `get_pane_info`. |
| `list_servers` or per-call socket selection | Run one MCP process per socket and use `get_server_info` for its pinned server. |
| `expand_format` | Use `get_tmux_variables` for validated variable names; arbitrary tmux-format evaluation has no public replacement. |
| `what_changed`, `watch_pane` | Follow a known pane with `capture_since` or wait with `wait_for_text`; there is no global activity feed or subscription. |
| `find_panes`, `find_sessions` | Use `search_panes` for terminal content, `find_pane_by_position` for layout, or filter `list_panes` and `list_sessions` locally. |
| `run_command` | Use `run_shell_command`. |
| `wait_for_idle` | Use a concrete `wait_for_text` condition or inspect new output with `capture_since`; no idle heuristic remains. |
| `start_command`, `job_status`, `list_jobs`, `forget_job` | No background handle remains. Use bounded `run_shell_command`, or drive a pane and observe it with `capture_since`. |
| `new_window`, `split_pane` | Use `create_window` and `split_window`. |
| `rename` | Use `rename_session`, `rename_window`, or `set_pane_title`. |
| `pipe_pane` | No shell-command pipe route remains; read with `capture_pane` or `capture_since`. |
| `clear_pane` | Use teardown tool `clear_pane_scrollback`. |
| `set_option` | Use constrained tools such as `set_mouse_enabled`, `set_history_limit`, or `set_synchronize_panes`. |
| `set_environment` | No generic caller-controlled environment route remains. |
| `run_plan` | Use `call_read_tools_batch` for inspect-only batches; issue typed state-changing calls separately. |
| `kill_server` | Kill selected sessions explicitly or administer the server outside MCP. |
| `tmux://server` | Use `get_server_info`; `tmux://capabilities` adds the frozen connection and selection boundary. |
| `tmux://sessions`, `tmux://windows`, `tmux://panes` | Use `list_sessions`, `list_windows`, and `list_panes`. |
| `tmux://sessions/{name}`, `tmux://panes/{id}` | Use `get_session_info` and `get_pane_info`. |
| `tmux://sessions/{name}/windows`, `tmux://sessions/{name}/windows/{index}` | Use `list_windows`, filter by `session_id`, then pass the returned stable id to `get_window_info`. |
| `tmux://panes/{id}/content` | Use `capture_pane` or continue from a cursor with `capture_since`. |
| `enter_copy_mode`, `exit_copy_mode` | Read with capture, snapshot, search, or cursor tools. The attached person owns pane modes. |
| Prompt `run_and_wait` | Use one `run_shell_command`; decide from `outcome` and `exit_status`. A deadline stops the wait, not the pane command. |
| Prompt `interrupt_gracefully` | Start with `snapshot_pane`. If `in_mode`, keep observing and let the attached person leave the mode. Otherwise use `send_keys` with `keys: ["C-c"]`, not `text: "C-c"`, then observe with `capture_since` or `wait_for_text`. `keys: ["C-\\"]` is stronger; do not turn pane recovery into teardown. |
| Prompt `diagnose_pane` | Start with `snapshot_pane`; inspect `pane.command`, `dead`, `in_mode`, `mode`, `dropped`, and `content`. Repeat with `history: true` when the visible screen is insufficient, follow later output with `capture_since`, and use `search_panes` if the target is uncertain. |

### Asking first

The server does not implement a separate confirmation or consent policy.
Clients can use each tool's four MCP annotations to decide whether to ask a
person before a whole call. Tool selection shapes the advertised interface; it
does not reduce the tmux user's authority.

When launched from tmux, the process inherits a pane ID and socket. Pane
listings mark that pane `caller: "self"` only when the socket matches the
selected server. Pane-input tools refuse any configured recipient that may be
that caller; target-only paste checks only its named pane. Teardown tools refuse
a target that may contain the caller. The comparison weighs the socket as well
as the pane ID because `%1` names a different pane on every tmux server.

Pane input also refuses a configured pane visible to a non-control tmux client.
In an unzoomed window every visible pane is attended; in a zoomed window only
the active pane is. Control-mode clients do not count. This is a protective
refusal, not a consent signal.

## Choosing a server

Without arguments the server selects the `libtmux-mcp` socket, starts it with
the shipped minimal configuration, and authenticates that this launch created
the daemon before enabling teardown by default. The owning process stops that
daemon at shutdown. An already-running daemon keeps its configuration and gets
conservative provenance. The server does not follow `$TMUX`. Select another
socket by path or name:

```console
$ tmux-mcp --socket /tmp/tmux-1000/work
```

```console
$ tmux-mcp --socket-name work
```

`-S` and `-L` work too. `LIBTMUX_SOCKET_PATH` and `LIBTMUX_SOCKET` provide the
same startup choices. Set `LIBTMUX_TMUX_CONFIG` to use an explicit tmux
configuration at an absolute path. Socket names and paths are mutually
exclusive. `tmux-mcp --help` lists every flag.

## Trust boundary

One process stays bound to one tmux socket. That limits which tmux objects its
structured tools can name; it does not confine filesystem, network, credential,
or process access. Toolset filtering shapes discovery and direct calls. It is
not authorization.

Execute tools run with the tmux user's authority. `run_shell_command` accepts a
pane command; `send_keys`, `send_keys_batch`, and `paste_text` deliver input to
the pane program. Spawn tools start only the configured process and accept no
command or environment payload. No public tool runs a caller-authored host
command.

`run_shell_command` requires a trusted POSIX-compatible pane shell whose
reserved words and special builtins retain their standard meanings. The tmux
server and configuration are trusted too. Its resolved tmux executable and
socket path must contain no ASCII terminal-control bytes; the tool rejects such
routes before attaching its watcher. Command aliases and hooks are executable
configuration, not a sandbox boundary. Valid inherited Bash and zsh `ERR` and
`DEBUG` traps remain visible to the authored command; parent-shell traps and
the `errexit`, `xtrace`, and `noglob` options remain unchanged.

Pane output and tmux metadata may contain sensitive or untrusted text.
Environment values may contain secrets. Hooks may contain executable
configuration. Existing aliases, hooks, plugins, status jobs, and pane
processes can add effects to any call. Standard MCP annotations describe the
whole call for client consent; they do not enforce authority.

## What it feels like

> **You:** Run the API tests in the `api` session and tell me what broke.
>
> **Agent:** `run_shell_command` in pane `%3` finished with exit status 1. Two
> failures, both in `test_auth.py` — `test_token_refresh` and
> `test_expired_session`. Want me to open them?

The agent waited for the command, read its real exit status, and got the
output the command actually wrote — no prompt, no echo, and nothing lost to
scrollback.

For a wider inventory pass, one batch call can list topology, snapshot a pane,
and read an option while keeping each child’s complete MCP envelope:

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

Each result row preserves `content`, `structuredContent`, `_meta`, and
`isError`. If the complete JSON-RPC response line would exceed 1,000,000 bytes,
the batch truncates nested payloads until it fits without dropping an executed
row. It reports `resultTruncated` per affected row plus outer `truncated` and
`truncatedBytes` values.

## When it earns its keep

For a single `tmux send-keys`, it does not. It earns its keep the moment an
agent has to wait, look, or avoid breaking the terminal it is working in.

**Running something.** `run_shell_command` sends the command, waits for it to
finish, and answers with its exit status and output. Reaching the deadline ends
the waiting, not the pane command. Inspect the pane before sending more input;
use `send_keys` with `keys: ["C-c"]` only when pane-wide interruption is
intended. The tool requires one configured input recipient and observes its
cohort, input-off state, mode, liveness, client attention, inherited-caller
relation, foreground shell, route, and active-run ownership at exactly two
checkpoints: before watcher setup and immediately before its single dispatch.
Invalid syntax completes with a nonzero shell status. The process-wide
reservation blocks other MCP pane input until completion is proved, but does
not lock tmux against an external client changing the pane.

**Waiting for something you did not start.** `wait_for_text` watches the
pane's output stream for a pattern, with stop patterns for the failures you
already know. Because it reads the stream rather than polling the screen, a
line that scrolls past between looks is still seen.

**Following a pane over several turns.** `capture_since` returns only what is
new since the cursor it gave you last time, and says `missed: true` if
anything was dropped.

**Finding where something is.** `search_panes` matches across every pane at
once and reports the pane and line. The listing tools will not: they read
names and commands, not what a terminal is showing.

**Asking about layout.** `find_pane_by_position` finds the pane touching a
named window corner. `snapshot_pane` adds geometry, cursor, and mode state to
the visible content.

**Reading several things.** `call_read_tools_batch` runs a bounded serial batch
of enabled inspect operations. Its nested authority shrinks when an operation
is excluded.

**Reading tmux variables.** `get_tmux_variables` accepts one to 32 validated
variable names and constructs bounded `#{variable}` references itself. The
manifest records `validated-variable-name` under `inputLiteralization`. It
does not accept arbitrary or shell-command formats.

## Answers are typed

Every tool publishes an output schema and answers with structured content, so
an agent reads fields rather than parsing text:

```json
{"pane": "%3", "outcome": "completed", "exit_status": 0,
 "output": "ok\n", "bytes": 812, "truncated": false}
```

Failures are typed too. Every error carries the same three fields, so an agent
decides what to do next without reading prose:

```json
{"kind": "object_gone", "retryable": false, "stale": true}
```

`stale` means the target is gone and a fresh listing would say something
different — the answer is to look again, not to retry. `retryable` means the
same call is safe to repeat unchanged and may succeed after the condition
clears. A pane that closed and a tmux that is not running both fail, and only
one of them is worth waiting on. `partial_effect` means tmux accepted part of
a multi-step call before a later step failed; inspect the current state before
choosing another action.

## Using it from Rust

The tool surface is a type, so a program can freeze a `Selection` in code or
serve it over a transport other than stdio. Three runnable examples ship with
the crate:

```console
$ cargo run --example readonly
```

```console
$ cargo run --example surface
```

`readonly` serves the `inspect` toolset. `surface` prints any selected toolset
combination, named inclusions and exclusions, controlled descriptions, schemas,
and capability fields without starting a tmux server. This prints an
aggregate-only surface with one nested operation excluded:

```console
$ cargo run --example surface -- '' call_read_tools_batch show_environment
```

```console
$ cargo run --example budget
```

`budget` measures what a client downloads at `tools/list`, which is the
constraint that decides whether a tool earns its place.

## Development

```console
$ cargo test -p tmux-mcp
```

The tests drive a real tmux on an isolated socket, so tmux must be on `$PATH`.

## Related

- [libtmux](https://docs.rs/libtmux) — the typed tmux client underneath
- [libtmux-mcp](https://github.com/tmux-python/libtmux-mcp) — the Python server
  this one learned its discoverability habits from

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.
