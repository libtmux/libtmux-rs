# tmux-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server for
[tmux](https://github.com/tmux/tmux), built on
[libtmux](https://docs.rs/libtmux).

> [!WARNING]
> **Alpha.** The tool surface changes between releases, including in ways that
> will not be called out as breaking, because nothing here is stable yet.
> `cargo install` will not pick a prerelease unless asked, so the install
> command below names the version. Feedback welcome.

Give an agent hands inside the terminal: create sessions, run commands and
learn whether they worked, watch output as it arrives, and find which pane is
showing the thing you are looking for.

Reading a pane goes through tmux's control mode rather than screen captures,
so output that scrolled past between calls is still seen, and `run_command`
reports a real exit status instead of leaving an agent to guess from text.

## Requirements

tmux 3.2a or newer, on `$PATH`. Rust 1.88 to build.

## Install

```console
$ cargo install tmux-mcp --version 0.1.0-alpha.9
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
`tmux-mcp`, and no arguments. Add `--safety destructive` to the arguments if
you want the dedicated kill tools and destructive plan operations; see below.

## What it offers

Forty-eight tools. Each publishes MCP hints for its direct operation, so a
client can distinguish reads, additive changes, and potentially destructive
effects. The hints describe effects; they do not decide which surface tier
offers a route.

| | Tools |
|---|---|
| **Look** | `list_sessions`, `list_windows`, `list_panes`, `list_session_windows`, `list_window_panes`, `describe`, `list_servers`, `expand_format`, `what_changed` |
| **Read a pane** | `capture_pane`, `snapshot_pane`, `capture_since`, `watch_pane`, `search_panes` |
| **Find** | `find_panes`, `find_sessions` |
| **Run and wait** | `run_command`, `send_keys`, `wait_for_text`, `wait_for_idle`, `wait_for_channel`, `signal_channel` |
| **Run in the background** | `start_command`, `job_status`, `list_jobs`, `forget_job` |
| **Arrange** | `create_session`, `new_window`, `split_pane`, `select_pane`, `select_window`, `resize_pane`, `rename`, `select_layout`, `respawn_pane` |
| **Move text** | `paste_text`, `pipe_pane`, `clear_pane` |
| **Configure** | `show_option`, `set_option`, `show_environment`, `set_environment`, `show_hooks` |
| **Batch** | `run_plan` |
| **Destroy** | `kill_pane`, `kill_window`, `kill_session`, `kill_server` |

The mutating and destructive tiers also offer three prompts — `run_and_wait`,
`interrupt_gracefully`, and `diagnose_pane` — for combinations that are easy
to get wrong. Read-only offers `diagnose_pane` alone. Each prompt names only
tools its tier offers.

## Resources

Alongside the tools, the hierarchy is browsable as URIs. Clients show these in
a picker, so attaching a pane to a conversation is something you do directly
rather than something you ask an agent to go and fetch.

| URI | Holds |
|---|---|
| `tmux://server` | The selected tmux server and inherited caller context |
| `tmux://sessions` | Every session |
| `tmux://windows` | Every window, across sessions |
| `tmux://panes` | Every pane, across sessions |
| `tmux://sessions/{name}` | One session |
| `tmux://sessions/{name}/windows` | Its windows |
| `tmux://sessions/{name}/windows/{index}` | One window |
| `tmux://panes/{id}` | One pane |
| `tmux://panes/{id}/content` | What that pane is showing, as text |

Everything above is also reachable through a tool, so an agent loses nothing
if its client does not support resources.

## Surface tiers

`--safety` filters the tools advertised to a client. It is not a sandbox or an
authorization boundary.

The four dedicated kill tools are **not offered by default**. `run_plan` keeps
one name at every tier, advertises that tier's ceiling, and checks every
operation before it runs any of them.

```console
$ tmux-mcp --safety destructive
```

Three tiers, also settable with `TMUX_MCP_SAFETY`:

| Tier | Offers |
|---|---|
| `readonly` | Read-only routes; plans accept read-only operations |
| `mutating` | Default; dedicated kill tools and destructive plan operations are refused |
| `destructive` | Everything |

Tools above the tier are not advertised. This reduces the available choices;
it does not constrain the effects of tools that remain. `run_command` and
`start_command` run shell commands in a pane, `send_keys` can type and submit
one, and `expand_format` can start a shell command through literal or recursive
format expansion. A caller can use those paths to kill work without calling a dedicated kill
tool. Treat the `mutating` and `destructive` tiers as shell-equivalent access.
Use an isolated tmux server and operating-system permissions when effects need
an authority boundary.

The live-stream tools (`watch_pane`, `wait_for_text`, `wait_for_idle`, and
`capture_since`) attach a tmux client without updating the session environment.
That changes the session's attached-client state, and `capture_since` retains
its client, so the `readonly` tier withholds them. Configured `client-attached`
hooks remain an indirect tmux effect, as do configured `after-*` hooks on
ordinary read commands.

### Asking first

A tier is decided once, at launch. `--confirm` asks before dedicated kill tools
and destructive plan operations:

```console
$ tmux-mcp --safety destructive --confirm
```

Those calls proceed only on a yes. They fail closed when the client cannot
ask. Confirmation does not inspect command text or keys passed to open-ended
tools, so indirect destructive effects do not ask. `TMUX_MCP_CONFIRM=1` does
the same. `--confirm` and `--no-confirm` override that environment setting.
Invalid safety values narrow the surface to `readonly`; invalid confirmation
values enable the gate.

When launched from tmux, the process inherits a pane ID and socket. Pane
listings mark that pane `caller: "self"` only when the socket matches the
selected server. Dedicated kill tools and destructive plan operations refuse
confirmed or conservatively matched caller context. Open-ended command and
terminal tools do not carry that guard. The comparison weighs the socket as
well as the pane id, because `%1` names a different pane on every tmux server.

## Choosing a server

Without arguments the server follows `$TMUX` when it was started inside tmux,
and otherwise talks to tmux's default socket. To pick one:

```console
$ tmux-mcp --socket /tmp/tmux-1000/work
```

```console
$ tmux-mcp --socket-name work
```

`-S` and `-L` work too, spelled as tmux spells them. `tmux-mcp --help` lists
everything.

## What it feels like

> **You:** Run the API tests in the `api` session and tell me what broke.
>
> **Agent:** `run_command` in pane `%3` finished with exit status 1. Two
> failures, both in `test_auth.py` — `test_token_refresh` and
> `test_expired_session`. Want me to open them?

The agent waited for the command, read its real exit status, and got the
output the command actually wrote — no prompt, no echo, and nothing lost to
scrollback.

## When it earns its keep

For a single `tmux send-keys`, it does not. It earns its keep the moment an
agent has to wait, look, or avoid breaking the terminal it is working in.

**Running something.** `run_command` sends the command, waits for it to
finish, and answers with its exit status and the output the command wrote.
Reaching the deadline ends the waiting, not the command, so the answer says
`deadline` and includes a job id. Pass that id to `job_status` to keep
following the same run, or to `forget_job` to stop collecting and forget its
retained output. It leaves pane activity alone; use `send_keys` with
`keys: ["C-c"]` to interrupt whatever that pane is running.

**Running something slow.** `start_command` returns a job id immediately and
collects the answer whether or not anyone is waiting, so a ten-minute build
does not cost an agent its turn. `job_status` reports the exit status once
there is one, and returns only what the command has written since the cursor
it gave you last. Several can run at once, in different panes.

**Waiting for something you did not start.** `wait_for_text` watches the
pane's output stream for a pattern, with stop patterns for the failures you
already know. Because it reads the stream rather than polling the screen, a
line that scrolls past between looks is still seen. When you cannot name what
success looks like -- a TUI settling, a prompt glyph you cannot predict --
`wait_for_idle` waits for the pane to stop writing instead.

**Following a pane over several turns.** `capture_since` returns only what is
new since the cursor it gave you last time, and says `missed: true` if
anything was dropped.

**Finding where something is.** `search_panes` matches across every pane at
once and reports the pane and line. The listing tools will not: they read
names and commands, not what a terminal is showing.

**Working out what to look at.** `what_changed` reports which windows have
written since the timestamp it gave you last, most recent first. That is one
call instead of capturing every pane to find the one that is doing something.

**Asking tmux something no tool covers.** `expand_format` evaluates any tmux
format, so a field with no tool of its own — `#{pane_unseen_changes}`,
`#{window_activity_flag}`, `#{client_termname}` — is one call away rather than
unreachable. Literal command formats and recursively expanded values can start
shell commands, so this tool is not offered by the `readonly` tier.

**Asking about layout.** `find_panes` takes a filter expression over a pane's
own tmux format fields, so "the bottom-right pane" is one call:

```json
{"version": 1, "target": "pane", "expr": {"op": "and", "args": [
  {"op": "eq", "field": "pane_at_bottom", "value": true},
  {"op": "eq", "field": "pane_at_right", "value": true}
]}}
```

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

The tool surface is a type, so you can build a server with a tier chosen in
code rather than by whoever launched it, or serve it over a transport other
than stdio. Two runnable examples ship with the crate:

```console
$ cargo run --example readonly
```

```console
$ cargo run --example surface
```

`readonly` serves the read-only routes; `surface` prints what each tier offers
and what each tool answers with, without starting one.

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
