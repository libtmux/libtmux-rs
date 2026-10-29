# tmux-mcp contracts

What `tmux-mcp` promises beyond the [README](../README.md): which panes
receive input, what a tool selection removes, what a client can learn about
the server, and what the server trusts. Each tool's page on libtmux.org has
its arguments and results.

## Pane input

[`send_keys`][send_keys], [`send_keys_batch`][send_keys_batch],
[`paste_text`][paste_text] and [`run_shell_command`][run_shell_command] type
into panes, and what they type runs with your user's permissions. Before one
acts, the server looks at the panes the input would reach. If any of them
fails a check below, that input is refused and nothing is sent.

Which panes receive input:

- [`send_keys`][send_keys] and [`send_keys_batch`][send_keys_batch] reach the
  pane you name, plus every other synchronized pane in its window when that
  pane has synchronized input on.
  [`set_synchronize_panes`][set_synchronize_panes] sets the window default,
  and a pane's own setting overrides it.
- [`paste_text`][paste_text] reaches only the pane you name, even when
  synchronized input is on.
- [`run_shell_command`][run_shell_command] reaches exactly one pane, and
  refuses when synchronized input would reach more.
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

### run_shell_command

- Needs a pane running a POSIX shell that the server recognises.
- Reserves its pane until the command ends or the pane closes. Until then the
  other pane-input tools refuse that pane.
- Lets `send_keys` with only `C-c` or `C-\` through, to interrupt the command.
  The interrupted command still reports completion, which releases the pane.
- Stops waiting at its deadline, but not the command, and the reservation
  stays. [`respawn_pane`][respawn_pane] with `kill_first` set to `true` ends a
  program that ignores both interrupts.

The tool requires one configured input recipient. It observes the recipient's
cohort, input-off state, mode, liveness, client attention, inherited-caller
relation, foreground shell, route, and active-run ownership at two
checkpoints: before watcher setup and immediately before its single dispatch.

Invalid syntax completes with a nonzero shell status. The process-wide
reservation blocks other MCP pane input until completion is proved. It does
not lock tmux against an external client changing the pane.

`run_shell_command` and [`capture_since`][capture_since] return the output
stream with escape sequences removed. [`wait_for_text`][wait_for_text] uses
that stream on tmux 3.7 or newer; below that it polls and returns the last
visible screen read, so text that appears and scrolls off between reads can
be missed. A present-at-entry match can also return captured screen text.
In a streamed result, a line redrawn in place, such as a line editor's echo
or a progress bar, repeats once per redraw. [`capture_pane`][capture_pane]
returns what the screen displays.

`run_shell_command` retains a bounded tail of command output. Its `truncated`
field says whether older output was discarded, and `bytes` reports the size
before truncation.

### Batches

- `send_keys_batch` takes 1 to 64 operations and runs them in order, repeating
  the checks before each one. A refused operation does not undo the ones
  before it. `on_error` is `stop`, the default, or `continue`.
- [`call_read_tools_batch`][call_read_tools_batch] takes 1 to 16 inspect
  operations, and one approval covers the whole batch. Each result row keeps
  `content`, `structuredContent`, `_meta`, and `isError`.
- If the complete JSON-RPC response line would exceed 1,000,000 bytes, the
  batch truncates nested payloads until it fits, and drops no executed row. It
  reports `resultTruncated` per affected row, plus outer `truncated` and
  `truncatedBytes` values.

### select_layout

[`select_layout`][select_layout] accepts saved tmux layouts, named layouts,
and unique name abbreviations. Names follow the running daemon's version;
mirrored main layouts require tmux 3.5. Malformed syntax is refused before
window lookup.

tmux owns geometry and pruning, and a successful reply contains
the saved layout actually applied. A readback failure after mutation remains a
`partial_effect` error.

### get_tmux_variables

[`get_tmux_variables`][get_tmux_variables] accepts one to 32 validated
variable names and constructs the bounded `#{variable}` references itself. The
manifest records `validated-variable-name` under `inputLiteralization`. It
accepts no arbitrary or shell-command formats, and refuses a name the tmux
environment holds unless the operator allowed it (see
[Environment values](#environment-values)).

## Capability records

Each tool carries one machine-readable capability record, and registration,
descriptions, annotations, selection, and `tmux://capabilities` all read it.

A client receives it under `_meta["com.git-pull.libtmux-mcp/capability"]`,
holding only what the tool does not already carry: toolset, process reach,
effect and output sets, the secret and untrusted-content flags, schema-keyed
input literalization, any nested authority, and future-input amplification.

Name, title, description, annotations, and schemas stay on the tool. The
native definition also classifies every input sink, but that validation detail
is not repeated on the wire. For example, `get_tmux_variables.names` is
reported as `validated-variable-name`.

## The capabilities resource

`tmux://capabilities` is the server's one resource. It reports the
startup-frozen effective surface, the selected socket, socket provenance,
direct process reach, tmux effects, output classes, schema-keyed input
literalization, future-input amplification, nested authority, and whole-call
MCP annotations. No dynamic resource templates are registered.

For each tool it reports the name, title, annotations and capability row. The
description and schemas stay with `tools/list`, which a client already holds,
so the whole report is about 26 KB.

The report states the outer boundary. One process has one socket, no call can
select another socket, no tool executes a host command, and the resource is
static. Its connection object supplies the socket selector, resolved path,
attach command, daemon state, and configuration provenance:

```json
{
  "boundary": {
    "oneSocketPerProcess": true,
    "perCallSocketSelection": false,
    "hostCommandExecution": false,
    "dynamicResources": false
  },
  "connection": {
    "socketSelector": "path:/tmp/tmux-1000/default",
    "socketProvenance": "operator-current",
    "serverState": "existing",
    "configurationProvenance": "unknown"
  }
}
```

Read this resource when a client needs to explain its authority or cache the
effective surface. Read tmux state through inspect tools: there are no live
session, window, pane, or output resources to drift away from the tool
catalog.

## Toolset selection

The default is `inspect`, `manage`, and `execute`; select `teardown`
explicitly.

- [`LIBTMUX_TOOLSETS`] replaces the default with a comma-separated unordered
  set. An empty string offers no toolsets. Empty tokens and unknown names stop
  startup.
- [`LIBTMUX_TOOLS`] adds named tools after toolset expansion.
- [`LIBTMUX_EXCLUDE_TOOLS`] removes named tools last, including aggregate
  nested authority. Exclusions always win, and an excluded tool cannot be
  restored by naming it in `LIBTMUX_TOOLS`.
- Hidden tools are neither listed nor callable.
- Unknown tool names, unknown toolsets, and empty elements in any nonempty
  list fail before tmux is opened, so a typo never widens the surface.

Named inclusion also works with the zero-toolset subset. Naming
[`call_read_tools_batch`][call_read_tools_batch] alone exposes only the aggregate, and it keeps its 16
eligible inspect operations as nested authority. An exclusion removes the
named operation from both that authority and the generated `oneOf` schema.

The retired `LIBTMUX_SAFETY` and `TMUX_MCP_SAFETY` settings stop startup with
a migration error, so they never silently widen or narrow the surface. The
[migration table](migration.md) maps every retired name to its replacement.

## Asking first

The server implements no confirmation or consent policy. A client can use
each tool's four MCP annotations to decide whether to ask a person before a
whole call. Tool selection shapes the advertised interface, and does not
reduce the tmux user's authority.

The annotations derive from each tool's capability row:

- A tool that only observes tmux is `readOnlyHint: true`. Every `inspect` tool
  is read-only: the observer client that [`wait_for_text`][wait_for_text] or
  [`capture_since`][capture_since] attaches only reads, and changes no session, window, pane, or option.
- Deleting tmux state or sending pane input is `destructiveHint: true`,
  because the receiving shell runs whatever arrives.
- Reads, teardown, and changes that set a named value, such as a rename, are
  `idempotentHint: true`.
- Starting or driving a process, or returning terminal text, is
  `openWorldHint: true`.

[`set_history_limit`][set_history_limit] is destructive: from tmux 3.7 a lower
limit discards existing panes' scrollback past it, so it is in `teardown`
beside [`clear_pane_scrollback`][clear_pane_scrollback].

## The caller's pane

When launched from tmux, the process inherits a pane ID, session number,
server PID, and socket. Pane listings mark that pane `caller: "self"` only
when the socket matches the selected server.

Pane-input tools and the three kill tools additionally resolve the complete
identity against a fresh selected-daemon snapshot before acting. A complete
identity on another physical socket is foreign, and malformed or inconsistent
context on the selected socket fails closed. The comparison weighs the socket
as well as the pane ID because `%1` names a different pane on every tmux
server.

Pane input also refuses a configured pane visible to a non-control tmux
client. In an unzoomed window every visible pane is attended, and in a zoomed
window only the active pane is. Control-mode clients do not count. The refusal
protects the person watching that pane, and nobody has to consent for it to
apply.

## Choosing a server

The server selects a socket in this order, and the first that names one wins:

1. `-S` and `--socket` name a socket by path, and `-L` and `--socket-name` by
   name. The two flags are mutually exclusive.
2. `LIBTMUX_SOCKET_PATH`, a socket path, as `-S`.
3. `LIBTMUX_SOCKET`, a socket name, as `-L`.
4. None. tmux gets neither `-S` nor `-L` and picks the server itself: the one
   `TMUX` names when the server runs inside tmux, and its default socket
   otherwise.

A flag leaves both variables unread. `LIBTMUX_SOCKET_PATH` wins when both are
set, and a variable that is empty or only whitespace counts as unset. The
server starts no daemon of its own and stops none: it reports a daemon that
answers as `existing` and one that does not as `absent`, with configuration
provenance `unknown`, and teardown is not in the default toolsets.

Set `LIBTMUX_TMUX_CONFIG` to use an explicit tmux configuration at an
absolute path. `tmux-mcp --help` lists every flag and environment variable.

## Trust boundary

One process stays bound to one tmux socket. That limits which tmux objects its
structured tools can name. It does not confine filesystem, network,
credential, or process access. Toolset filtering shapes discovery and direct
calls, and does not authorize anything.

Execute tools run with the tmux user's authority. `run_shell_command` accepts
a pane command; `send_keys`, `send_keys_batch`, and `paste_text` deliver input
to the pane program. Spawn tools start only the configured process and accept
no command or environment payload. No public tool runs a caller-authored host
command.

`run_shell_command` requires a trusted POSIX-compatible pane shell whose
reserved words and special builtins retain their standard meanings. The tmux
server and configuration are trusted too.

Its resolved tmux executable and socket path must contain no ASCII
terminal-control bytes; the tool rejects such routes before attaching its
watcher. Command aliases and hooks are executable configuration and sit
outside any sandbox.

Valid inherited Bash and zsh `ERR` and `DEBUG` traps remain visible to the
authored command. Parent-shell traps and the `errexit`, `xtrace`, and `noglob`
options remain unchanged.

Pane output and tmux metadata may contain sensitive or untrusted text. Hooks
may contain executable configuration. Existing aliases, hooks, plugins, status
jobs, and pane processes can add effects to any call. Standard MCP annotations
describe the whole call for client consent, and enforce nothing.

### Environment values

A tmux server holds the environment of the shell that started it, tokens and
keys included. No tool returns an environment value by default.

[`show_environment`][show_environment] lists each name and whether it is set or
marked for removal, and withholds the value. [`get_tmux_variables`][get_tmux_variables] refuses a
name the server or session environment holds, because tmux expands a name it
does not know as a format from the environment.

[`LIBTMUX_ENVIRONMENT_VALUES`] names, at startup, the variables whose values
both tools may return, and a named value reaches the client in clear. It takes
up to 32 comma-separated names, matched exactly:

```console
$ LIBTMUX_ENVIRONMENT_VALUES=TERM,LANG tmux-mcp
```

This covers what tmux holds. It does not cover what a pane's program can
print: a command sent through an execute tool runs with the tmux user's
environment.

## Typed answers

Every tool publishes an output schema and answers with structured content.
Every error carries `kind`, `retryable`, and `stale`. That includes arguments
that do not match a tool's schema, and a call to a tool the startup selection
left out, whose message names the settings that offer it.

- `stale` means the target is gone and a fresh listing would say something
  different. Look again before you retry.
- `retryable` means the same call is safe to repeat unchanged and may succeed
  after the condition clears. A pane that closed and a tmux that is not
  running both fail, and only one of them is worth waiting on.
- `partial_effect` means tmux accepted part of a multi-step call before a
  later step failed. Inspect the current state before choosing another action.

[call_read_tools_batch]: https://libtmux.org/en/rs/latest/mcp/tools/call_read_tools_batch/
[capture_pane]: https://libtmux.org/en/rs/latest/mcp/tools/capture_pane/
[capture_since]: https://libtmux.org/en/rs/latest/mcp/tools/capture_since/
[clear_pane_scrollback]: https://libtmux.org/en/rs/latest/mcp/tools/clear_pane_scrollback/
[get_tmux_variables]: https://libtmux.org/en/rs/latest/mcp/tools/get_tmux_variables/
[paste_text]: https://libtmux.org/en/rs/latest/mcp/tools/paste_text/
[respawn_pane]: https://libtmux.org/en/rs/latest/mcp/tools/respawn_pane/
[run_shell_command]: https://libtmux.org/en/rs/latest/mcp/tools/run_shell_command/
[select_layout]: https://libtmux.org/en/rs/latest/mcp/tools/select_layout/
[send_keys]: https://libtmux.org/en/rs/latest/mcp/tools/send_keys/
[send_keys_batch]: https://libtmux.org/en/rs/latest/mcp/tools/send_keys_batch/
[set_history_limit]: https://libtmux.org/en/rs/latest/mcp/tools/set_history_limit/
[set_synchronize_panes]: https://libtmux.org/en/rs/latest/mcp/tools/set_synchronize_panes/
[show_environment]: https://libtmux.org/en/rs/latest/mcp/tools/show_environment/
[wait_for_text]: https://libtmux.org/en/rs/latest/mcp/tools/wait_for_text/
[`LIBTMUX_TOOLSETS`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-toolsets_env/
[`LIBTMUX_TOOLS`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-tools_env/
[`LIBTMUX_EXCLUDE_TOOLS`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-exclude_tools_env/
[`LIBTMUX_ENVIRONMENT_VALUES`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-environment_values_env/
