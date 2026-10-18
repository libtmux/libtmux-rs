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
[libtmux][libtmux-docs].

</div>

> **Alpha.** The tool surface changes between releases, including in ways that
> will not be called out as breaking, because nothing here is stable yet.
> `cargo install` will not pick a prerelease unless asked, so the install
> command below names the version. Feedback welcome.

Give an agent typed operations inside the terminal: inspect tmux, arrange its
objects, drive pane programs, and wait for observable outcomes.

The [tool pages][tools] describe each tool, [Connect a client][connect] makes a
first call, and the [API reference][api-reference] covers running the server
from Rust. The API is also on [docs.rs][docs-rs].

Reading a pane goes through tmux's control mode rather than screen captures,
so output that scrolled past between calls is still seen, and
`run_shell_command` reports a real exit status instead of leaving an agent to
guess from text.

## Requirements

tmux 3.3a or newer, on `$PATH`. Rust 1.88 to build.

## Install

```console
$ cargo install tmux-mcp --version 0.1.0-alpha.16
```

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) fetches a
prebuilt binary for Linux or macOS instead, when the release carries one:

```console
$ cargo binstall tmux-mcp --version 0.1.0-alpha.16
```

Either puts a `tmux-mcp` binary on your path. Check that the shell finds it:

```console
$ tmux-mcp --version
```

It speaks MCP on stdin and stdout, so every client below is really the same
thing: run `tmux-mcp`.

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

### Passing options

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

[Toolset selection](#toolset-selection) and
[Choosing a server](#choosing-a-server) list every setting.

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
directly from that registry, and the [tool pages][tools] publish it.

| Toolset | Intent | Tools |
|---|---|---|
| [`inspect`][toolset-inspect] (18) | Read bounded tmux state and terminal output | [`call_read_tools_batch`][call_read_tools_batch], [`capture_pane`][capture_pane], [`capture_since`][capture_since], [`find_pane_by_position`][find_pane_by_position], [`get_pane_info`][get_pane_info], [`get_server_info`][get_server_info], [`get_session_info`][get_session_info], [`get_tmux_variables`][get_tmux_variables], [`get_window_info`][get_window_info], [`list_panes`][list_panes], [`list_sessions`][list_sessions], [`list_windows`][list_windows], [`search_panes`][search_panes], [`show_environment`][show_environment], [`show_hooks`][show_hooks], [`show_option`][show_option], [`snapshot_pane`][snapshot_pane], [`wait_for_text`][wait_for_text] |
| [`manage`][toolset-manage] (13) | Change tmux objects without starting a process | [`move_window`][move_window], [`rename_session`][rename_session], [`rename_window`][rename_window], [`resize_pane`][resize_pane], [`resize_window`][resize_window], [`select_layout`][select_layout], [`select_pane`][select_pane], [`select_window`][select_window], [`set_mouse_enabled`][set_mouse_enabled], [`set_pane_title`][set_pane_title], [`signal_channel`][signal_channel], [`swap_pane`][swap_pane], [`wait_for_channel`][wait_for_channel] |
| [`execute`][toolset-execute] (9) | Start configured processes or drive pane programs | [`create_session`][create_session], [`create_window`][create_window], [`paste_text`][paste_text], [`respawn_pane`][respawn_pane], [`run_shell_command`][run_shell_command], [`send_keys`][send_keys], [`send_keys_batch`][send_keys_batch], [`set_synchronize_panes`][set_synchronize_panes], [`split_window`][split_window] |
| [`teardown`][toolset-teardown] (5) | Delete tmux state | [`clear_pane_scrollback`][clear_pane_scrollback], [`kill_pane`][kill_pane], [`kill_session`][kill_session], [`kill_window`][kill_window], [`set_history_limit`][set_history_limit] |

### Pane input

[`send_keys`][send_keys], [`send_keys_batch`][send_keys_batch],
[`paste_text`][paste_text] and [`run_shell_command`][run_shell_command] type
into panes, and what they type runs with your user's permissions. Before one
acts, the server looks at the panes the input would reach. If any of them
fails a check below, that input is refused and nothing is sent.

Which panes receive input:

- `send_keys` and `send_keys_batch` reach the pane you name, plus every other
  synchronized pane in its window when that pane has synchronized input on.
  [`set_synchronize_panes`][set_synchronize_panes] sets the window default,
  and a pane's own setting overrides it.
- `paste_text` reaches only the pane you name, even when synchronized input is
  on.
- `run_shell_command` reaches exactly one pane, and refuses when synchronized
  input would reach more.
- A reply lists the panes the input was meant for. It does not prove the input
  arrived.

Input is refused when a pane it would reach:

- is dead, has input turned off, or is in a tmux mode such as copy mode
- is on screen in a terminal that a person has attached. With a zoomed window
  only the zoomed pane counts, and control-mode clients never do
- is reserved by a running `run_shell_command`
- is the pane the server was started from

It is also refused when `TMUX` and `TMUX_PANE` are set but incomplete or out
of date, because the server cannot tell which pane is its own. Start the
server outside tmux, or with both set correctly.

The checks look; they do not lock. A person can attach, or a pane can change,
between the check and the keystrokes.

`run_shell_command`:

- Needs a pane running a POSIX shell that the server recognises.
- Reserves its pane until the command ends or the pane closes. Until then the
  other pane-input tools refuse that pane.
- Lets `send_keys` with only `C-c` or `C-\` through, to interrupt the command.
- Stops waiting at its deadline, but not the command, and the reservation
  stays. [`respawn_pane`][respawn_pane] with `kill_first` set to `true` ends a
  program that ignores both interrupts.

Batches:

- `send_keys_batch` takes 1 to 64 operations and runs them in order, repeating
  the checks before each one. A refused operation does not undo the ones
  before it. `on_error` is `stop`, the default, or `continue`.
- [`call_read_tools_batch`][call_read_tools_batch] takes 1 to 16 inspect
  operations, and one approval covers the whole batch. Its reply is capped at
  1,000,000 bytes. Anything trimmed is flagged, and no executed row is
  dropped.

Each tool's page, linked in the table above, has the full contract.

### Capability records

The capability row is not a second hand-maintained catalog. A client receives
it under `_meta["com.git-pull.libtmux-mcp/capability"]`, holding only what the
tool does not already carry: toolset, process reach, effect and output sets,
the secret and untrusted-content flags, schema-keyed input literalization, any
nested authority, and future-input amplification. Name, title, description,
annotations, and schemas stay on the tool. `tmux://capabilities` reports each
tool's name, title, annotations and capability row, and leaves the description
and schemas to `tools/list`, which a client already holds: the whole report is
about 26 KB, read by an agent that wants the socket. The native definition
also classifies every input sink, but that validation detail is not
duplicated on the wire. For example, `get_tmux_variables.names` is
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
| `list_session_windows`, `list_window_panes` | Use [`list_windows`][list_windows] or [`list_panes`][list_panes], then filter the returned stable `session_id` or `window_id`. |
| `describe` | Use [`get_server_info`][get_server_info], [`get_session_info`][get_session_info], [`get_window_info`][get_window_info], or [`get_pane_info`][get_pane_info]. |
| `list_servers` or per-call socket selection | Run one MCP process per socket and use [`get_server_info`][get_server_info] for its pinned server. |
| `expand_format` | Use [`get_tmux_variables`][get_tmux_variables] for validated variable names; arbitrary tmux-format evaluation has no public replacement. |
| `what_changed`, `watch_pane` | Follow a known pane with [`capture_since`][capture_since] or wait with [`wait_for_text`][wait_for_text]; there is no global activity feed or subscription. |
| `find_panes`, `find_sessions` | Use [`search_panes`][search_panes] for terminal content, [`find_pane_by_position`][find_pane_by_position] for layout, or filter [`list_panes`][list_panes] and [`list_sessions`][list_sessions] locally. |
| `run_command` | Use [`run_shell_command`][run_shell_command]. |
| `wait_for_idle` | Use a concrete [`wait_for_text`][wait_for_text] condition or inspect new output with [`capture_since`][capture_since]; no idle heuristic remains. |
| `start_command`, `job_status`, `list_jobs`, `forget_job` | No background handle remains. Use bounded [`run_shell_command`][run_shell_command], or drive a pane and observe it with [`capture_since`][capture_since]. |
| `new_window`, `split_pane` | Use [`create_window`][create_window] and [`split_window`][split_window]. |
| `rename` | Use [`rename_session`][rename_session], [`rename_window`][rename_window], or [`set_pane_title`][set_pane_title]. |
| `pipe_pane` | No shell-command pipe route remains; read with [`capture_pane`][capture_pane] or [`capture_since`][capture_since]. |
| `clear_pane` | Use teardown tool [`clear_pane_scrollback`][clear_pane_scrollback]. |
| `set_option` | Use constrained tools such as [`set_mouse_enabled`][set_mouse_enabled], [`set_history_limit`][set_history_limit], or [`set_synchronize_panes`][set_synchronize_panes]. |
| `set_environment` | No generic caller-controlled environment route remains. |
| `show_environment` values | Values are withheld by default; allow names with `LIBTMUX_ENVIRONMENT_VALUES`. |
| `run_plan` | Use [`call_read_tools_batch`][call_read_tools_batch] for inspect-only batches; issue typed state-changing calls separately. |
| `kill_server` | Kill selected sessions explicitly or administer the server outside MCP. |
| `tmux://server` | Use [`get_server_info`][get_server_info]; `tmux://capabilities` adds the frozen connection and selection boundary. |
| `tmux://sessions`, `tmux://windows`, `tmux://panes` | Use [`list_sessions`][list_sessions], [`list_windows`][list_windows], and [`list_panes`][list_panes]. |
| `tmux://sessions/{name}`, `tmux://panes/{id}` | Use [`get_session_info`][get_session_info] and [`get_pane_info`][get_pane_info]. |
| `tmux://sessions/{name}/windows`, `tmux://sessions/{name}/windows/{index}` | Use [`list_windows`][list_windows], filter by `session_id`, then pass the returned stable id to [`get_window_info`][get_window_info]. |
| `tmux://panes/{id}/content` | Use [`capture_pane`][capture_pane] or continue from a cursor with [`capture_since`][capture_since]. |
| `enter_copy_mode`, `exit_copy_mode` | Read with capture, snapshot, search, or cursor tools. The attached person owns pane modes. |
| Prompt `run_and_wait` | Use one [`run_shell_command`][run_shell_command]; decide from `outcome` and `exit_status`. A deadline stops the wait, not the pane command. |
| Prompt `interrupt_gracefully` | Start with [`snapshot_pane`][snapshot_pane]. If `in_mode`, keep observing and let the attached person leave the mode. Otherwise use [`send_keys`][send_keys] with `keys: ["C-c"]`, not `text: "C-c"`, then observe with [`capture_since`][capture_since] or [`wait_for_text`][wait_for_text]. `keys: ["C-\\"]` is stronger. Either, sent alone, reaches a command an active [`run_shell_command`][run_shell_command] still reserves; [`respawn_pane`][respawn_pane] with `kill_first: true` replaces a program that ignores both. Do not turn pane recovery into teardown. |
| Prompt `diagnose_pane` | Start with [`snapshot_pane`][snapshot_pane]; inspect `pane.command`, `dead`, `in_mode`, `mode`, `dropped`, and `content`. Repeat with `history: true` when the visible screen is insufficient, follow later output with [`capture_since`][capture_since], and use [`search_panes`][search_panes] if the target is uncertain. |

### Asking first

The server does not implement a separate confirmation or consent policy.
Clients can use each tool's four MCP annotations to decide whether to ask a
person before a whole call. Tool selection shapes the advertised interface; it
does not reduce the tmux user's authority.

The annotations derive from each tool's capability row. A tool that only
observes tmux is `readOnlyHint: true`. Deleting tmux state or sending pane
input is `destructiveHint: true`, because the receiving shell runs whatever
arrives. Reads, teardown, and changes that set a named value, such as a
rename, are `idempotentHint: true`. Starting or driving a process, or
returning terminal text, is `openWorldHint: true`. Every `inspect` tool is
read-only: the observer client that `wait_for_text` or `capture_since` attaches
only reads, and changes no session, window, pane, or option.
`set_history_limit` is destructive: from tmux 3.7 a lower limit discards
existing panes' scrollback past it, so it is in `teardown` beside
`clear_pane_scrollback`.

When launched from tmux, the process inherits a pane ID, session number, server
PID, and socket. Pane listings mark that pane `caller: "self"` only when the
socket matches the selected server. Pane-input tools and the three kill tools
additionally resolve the complete identity against a fresh selected-daemon
snapshot before acting. A complete identity on another physical socket is
foreign; malformed or inconsistent context on the selected socket fails closed.
The comparison weighs the socket as well as the pane ID because `%1` names a
different pane on every tmux server.

Pane input also refuses a configured pane visible to a non-control tmux client.
In an unzoomed window every visible pane is attended; in a zoomed window only
the active pane is. Control-mode clients do not count. This is a protective
refusal, not a consent signal.

## Choosing a server

Without arguments the server selects the `libtmux-mcp` socket, starts it with
the shipped minimal configuration, and authenticates that this launch created
the daemon before enabling teardown by default. An already-running daemon
keeps its configuration and gets conservative provenance. The server does not
follow `$TMUX`.

Several `tmux-mcp` processes without arguments share that one daemon. Each
holds a shared lock on `libtmux-mcp.lease` beside the socket, and the process
that started the daemon stops it at shutdown only when no other process holds
one. No process stops a daemon it did not start, so when the owner exits
first the daemon outlives every process using it, and the next launch finds
it already running. Stop it with `tmux -L libtmux-mcp kill-server`.

Select another socket by path or name:

```console
$ tmux-mcp --socket /tmp/tmux-1000/work
```

```console
$ tmux-mcp --socket-name work
```

`-S` and `-L` work too. `LIBTMUX_SOCKET_PATH` and `LIBTMUX_SOCKET` provide the
same startup choices. Set `LIBTMUX_TMUX_CONFIG` to use an explicit tmux
configuration at an absolute path. Socket names and paths are mutually
exclusive. `tmux-mcp --help` lists every flag and environment variable.

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

Pane output and tmux metadata may contain sensitive or untrusted text. Hooks
may contain executable configuration. Existing aliases, hooks, plugins, status
jobs, and pane processes can add effects to any call. Standard MCP annotations
describe the whole call for client consent; they do not enforce authority.

A tmux server holds the environment of the shell that started it, tokens and
keys included. No tool returns an environment value by default.
`show_environment` lists each name and whether it is set or marked for
removal, and withholds the value. `get_tmux_variables` refuses a name the
server or session environment holds, because tmux expands a name it does not
know as a format from the environment. `LIBTMUX_ENVIRONMENT_VALUES` names, at
startup, the variables whose values both tools may return; a named value
reaches the client in clear. It takes up to 32 comma-separated names, matched
exactly:

```console
$ LIBTMUX_ENVIRONMENT_VALUES=TERM,LANG tmux-mcp
```

This covers what tmux holds, not what a pane's program can print: a command
sent through an execute tool runs with the tmux user's environment.

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
not lock tmux against an external client changing the pane. `C-c` or `C-\`
sent alone passes the reservation, and the interrupted command still reports
completion, which releases the pane; `respawn_pane` with `kill_first: true`
releases it from a program that ignores both.

**Waiting for something you did not start.** `wait_for_text` watches the
pane's output stream for a pattern, with stop patterns for the failures you
already know. Because it reads the stream rather than polling the screen, a
line that scrolls past between looks is still seen.

**Following a pane over several turns.** `capture_since` returns only what is
new since the cursor it gave you last time, and says `missed: true` if
anything was dropped.

`run_shell_command`, `wait_for_text`, and `capture_since` return the output
stream with escape sequences removed, not the rendered screen. A line redrawn in place -- a line editor's
echo, a progress bar -- repeats once per redraw. `capture_pane` shows what the
screen displays.

**Finding where something is.** `search_panes` matches across every pane at
once and reports the pane and line. The listing tools will not: they read
names and commands, not what a terminal is showing.

**Asking about layout.** `find_pane_by_position` finds the pane touching a
named window corner. `snapshot_pane` adds geometry, cursor, and mode state to
the visible content.

`select_layout` accepts saved tmux layouts, named layouts, and unique name
abbreviations. Names follow the running daemon's version; mirrored main
layouts require tmux 3.5. Malformed syntax is refused before window lookup.
tmux owns geometry and pruning, and a successful reply contains the saved
layout actually applied. A readback failure after mutation remains a
`partial_effect` error.

**Reading several things.** `call_read_tools_batch` runs a bounded serial batch
of enabled inspect operations. Its nested authority shrinks when an operation
is excluded.

**Reading tmux variables.** `get_tmux_variables` accepts one to 32 validated
variable names and constructs bounded `#{variable}` references itself. The
manifest records `validated-variable-name` under `inputLiteralization`. It
does not accept arbitrary or shell-command formats, and refuses a name the
tmux environment holds unless the operator allowed it (see
[Trust boundary](#trust-boundary)).

## Answers are typed

Every tool publishes an output schema and answers with structured content, so
an agent reads fields rather than parsing text:

```json
{"pane": "%3", "outcome": "completed", "exit_status": 0,
 "output": "ok\n", "bytes": 812, "truncated": false}
```

Failures are typed too. Every error carries the same three fields, so an agent
decides what to do next without reading prose. That includes arguments that
do not match a tool's schema, and a call to a tool the startup selection left
out, whose message names the settings that offer it:

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

The tool surface is a type, [`TmuxTools`][tmuxtools-docs], so a program can
freeze a `Selection` in code, serve it over a transport other than stdio, or
call a tool directly. The crate ships runnable examples. `run_and_wait` runs a
command and reads its real exit status, then waits for a line a background job
prints, through the same tools a client calls:

```console
$ cargo run --example run_and_wait
```

`follow` pushes 200 lines through a 24-row pane between two `capture_since`
calls, and the second call returns all 200:

```console
$ cargo run --example follow
```

`refused` serves only `inspect`, calls `send_keys` anyway, and prints the
refusal a client gets: a JSON-RPC invalid-params error naming why the tool is
missing and the setting an operator would change to offer it:

```console
$ cargo run --example refused
```

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

- [libtmux][libtmux-docs] — the typed tmux client underneath
- [libtmux-mcp](https://github.com/tmux-python/libtmux-mcp) — the Python server
  this one learned its discoverability habits from

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.

[tools]: https://libtmux.org/en/rs/latest/mcp/tools/
[connect]: https://libtmux.org/en/rs/latest/mcp/guides/connect-client/
[api-reference]: https://libtmux.org/en/rs/latest/mcp/reference/
[docs-rs]: https://docs.rs/tmux-mcp/latest/tmux_mcp/
[libtmux-docs]: https://docs.rs/libtmux/latest/libtmux/
[tmuxtools-docs]: https://docs.rs/tmux-mcp/latest/tmux_mcp/struct.TmuxTools.html
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
