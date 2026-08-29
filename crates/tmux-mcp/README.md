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
you want the tools that destroy work; see below.

## What it offers

Forty-eight tools. Each says what it does to the server, so a client can
decide what to run unattended and what to put in front of you.

| | Tools |
|---|---|
| **Look** | `list_sessions`, `list_windows`, `list_panes`, `list_session_windows`, `list_window_panes`, `describe`, `list_servers`, `expand_format`, `what_changed` |
| **Read a pane** | `capture_pane`, `snapshot_pane`, `capture_since`, `watch_pane`, `search_panes` |
| **Find** | `find_panes`, `find_sessions` |
| **Run and wait** | `run_command`, `send_keys`, `wait_for_text`, `wait_for_idle`, `wait_for_channel`, `signal_channel` |
| **Run in the background** | `start_command`, `job_status`, `list_jobs`, `cancel_job` |
| **Arrange** | `create_session`, `new_window`, `split_pane`, `select_pane`, `select_window`, `resize_pane`, `rename`, `select_layout`, `respawn_pane` |
| **Move text** | `paste_text`, `pipe_pane`, `clear_pane` |
| **Configure** | `show_option`, `set_option`, `show_environment`, `set_environment`, `show_hooks` |
| **Destroy** | `kill_pane`, `kill_window`, `kill_session`, `kill_server` |

And three prompts — `run_and_wait`, `interrupt_gracefully`, `diagnose_pane` —
for the combinations that are easy to get wrong.

## Resources

Alongside the tools, the hierarchy is browsable as URIs. Clients show these in
a picker, so attaching a pane to a conversation is something you do directly
rather than something you ask an agent to go and fetch.

| URI | Holds |
|---|---|
| `tmux://server` | Which tmux this is attached to, and the pane it runs in |
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

## Safety

The four dedicated tools that destroy work are **not offered by default**.
This server can end every session on a machine, so reaching that far should be
a decision you made rather than one you inherited. `run_plan` keeps one name
at every tier, advertises that tier's ceiling, and checks every operation
before it runs any of them.

```console
$ tmux-mcp --safety destructive
```

Three tiers, also settable with `TMUX_MCP_SAFETY`:

| Tier | Offers |
|---|---|
| `readonly` | Read-only tools and plans |
| `mutating` | Default; destructive tools and plan operations are refused |
| `destructive` | Everything |

Tools above the tier are not advertised at all, because an agent cannot choose
what it cannot see.

### Asking first

A tier is decided once, at launch. `--confirm` instead puts a person in the
loop for each irreversible act:

```console
$ tmux-mcp --safety destructive --confirm
```

Every destructive call, including a destructive plan, then asks the client to
put the question to you, and proceeds only on a yes. It fails closed: a client
that cannot ask gets a refusal rather than a destroyed session, because the
alternative is the unattended destruction the setting exists to prevent.
`TMUX_MCP_CONFIRM=1` does the same.

Separately, when tmux started the server it knows which pane it is in. Pane
listings mark that pane `caller: "self"`, and the tools that would destroy it
refuse — so an agent cannot end the conversation it is having. That comparison
weighs the socket as well as the pane id, because `%1` names a different pane
on every tmux server.

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
following the same run, or to `cancel_job` to stop and forget it.

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
unreachable.

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
same call could succeed on its own. A pane that closed and a tmux that is not
running both fail, and only one of them is worth waiting on.

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

`readonly` serves a read-only server; `surface` prints what each tier offers
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
