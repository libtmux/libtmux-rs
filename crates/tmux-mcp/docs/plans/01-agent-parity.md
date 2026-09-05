# Agent parity: caller identity, waiting, focus

What this crate needed to be usable by an agent rather than merely to exercise
`libtmux`. Each decision below was settled by running the alternatives against
real tmux, not by reasoning about them. Names and surface decisions reflect the
current 45-tool capability contract; rejected prototypes remain where their
measurements are useful.

## Completion signalling

An agent's most common question is "run this and tell me whether it passed".
Answering it needs a completion signal, an exit status, and the output. Four
mechanisms were measured.

**`refresh-client -B` format subscriptions — rejected.** tmux checks
subscriptions on a one-second timer (`control.c`, `struct timeval tv = {
.tv_sec = 1 }`), so a subscription on `#{pane_dead}` reports a finished command
up to a second late. Push delivery is worthless at that granularity.

**A control-mode `wait-for` — rejected.** The hope was that tmux would hold the
command's `%begin`/`%end` block open until the channel fired, giving an exact
signal that cancels by dropping the future. It does not: the block closes
immediately, 309 µs against a signal sent at 600 ms.

**A dedicated pane with `remain-on-exit` — viable, not chosen.** tmux reports
the status itself, and distinguishes a signal from an exit: `exit 42` yields
`pane_dead=1 pane_dead_status=42`, and `kill -TERM $$` yields `pane_dead=1
pane_dead_signal=15` with no status. It costs a visible pane and a layout
change in the caller's window, and `capture-pane` on the dead pane includes
tmux's own `Pane is dead (status 42, ...)` banner. Kept in reserve; it is the
only option that survives a pane whose foreground process is not a shell.

**Printable completion records on the output stream — chosen.** The pane shell
uses the resolved tmux executable and exact socket to ask `display-message -p`
clients to emit an empty separator, a 128-bit nonce opening line, and a closing
line carrying the status. Each inherited-xtrace and inherited-errexit branch
has this variable-free shape:

```
\set +e
if ( \exec '<resolved-tmux>' -N -S '<socket>' display-message -p '' ) &&
   ( \exec '<resolved-tmux>' -N -S '<socket>' display-message -p '__LIBTMUX_MCP_DONE_''<nonce>''__:BEGIN' ); then
  ( \set -e; \eval '<one quoted command operand>' )
  \set -- "$?"
  ( \exec '<resolved-tmux>' -N -S '<socket>' display-message -p '' )
  ( \exec '<resolved-tmux>' -N -S '<socket>' display-message -p '__LIBTMUX_MCP_DONE_''<nonce>''__:'"$1" )
fi
```

The paired branch uses `set +e`. When xtrace is inherited, the outer frame
turns it off and prefixes the quoted `eval` operand with `set -x` plus a
newline, so caller commands retain tracing without exposing frame bookkeeping.
The opening client gates the command; a failed opening marker cannot run it.

`ControlMode` is attached before the keys are sent, so the reply arrives on a
byte stream that began earlier than the command. Everything between the exact
opening and closing physical lines is the command's true output, stdout and
stderr interleaved in the order the program wrote them, with nothing scrolled
past and no screen rendering in the way.

The echo of the typed line cannot be mistaken for a completion record because
the marker is split across adjacent quoted fragments in the source. The
scanner accepts only an exact complete physical line; prefixes, lookalikes,
and incomplete closing lines remain ordinary or unfinished output. Frame
generation retries the negligible case where a random marker occurs in the
caller command.

The complete command is one quoted `eval` operand inside a subshell. Invalid
syntax closes with a nonzero status; trailing comments, `cd`, exports, traps,
functions, and bare `exit` cannot consume or mutate the outer frame or parent
shell. No fixed variable or pane-side `printf` name is trusted. Synchronous
`run-shell` records were rejected because tmux 3.3 through 3.4 send their
output to the pane's copy-mode buffer rather than the exact client. APC records
and bare, `command`-qualified, or fixed-path pane `printf` were rejected because
they made transport version-dependent or remained shadowable or nonportable.

The pane shell is trusted to preserve POSIX meanings for `case`, `set`, `eval`,
and `exec`; the tmux server and configuration, including command aliases, are
trusted too. The frame does not claim to resist a hostile mutated shell.

## Waiting for text

`wait_for_text` reads the same stream rather than polling `capture-pane`. That
removes the two failure modes polling has: output that scrolls past between
polls is still seen, and there is no grid anchor for tmux to invalidate when
`history-limit` trims. A call accepts at most 32 patterns per list, 4,096 bytes
per pattern, and 16 KiB across each compiled set. Rust's regex engine gives a
linear-time bound, and the caller's deadline is capped at 600 seconds.

The trade is real and worth stating: a stream is what programs wrote, not what
the screen shows. Escape sequences are stripped before matching, but a
cursor-addressed redraw — a progress bar rewriting one line — is not resolved
the way a terminal would resolve it. For detecting that text appeared, which is
what a wait is for, the stream is the better source.

## Incremental capture

`capture_since` keeps a live tail per pane instead of anchoring into
scrollback. The cursor names an offset in a byte ring the crate owns, so
"what changed since I last looked" is answered exactly while that cursor
still names retained output. `missed` reports a gap after buffer overrun, live
tail eviction, or server restart — conditions this crate can observe, unlike
scrollback tmux has already trimmed.

## Two tmux details that read the wrong way

Both cost a working tool and a test that agreed with the bug, so they are
written down rather than left to be rediscovered.

**`Server::shutdown` is not `kill-server`.** It closes this crate's subprocess
executor and, as libtmux's own documentation says, "never stops the tmux
daemon itself". A retired `kill_server` prototype built on it reported success
while every session survived, and left the handle unusable for later calls. Worse, a
test that asks *that same handle* whether the server is alive gets the answer
the closed executor gives, and agrees. Checking the outcome of a destructive
command needs a handle the command never touched.

**`select-window -l` ignores `-t`.** `cmd-select-window.c` calls
`session_next`, `session_previous` or `session_last` on the target's *session*
and never looks at the window, so making a step relative to a named window
means selecting that window first. For `last` that is wrong: it means the
session's previously active window, and selecting the named one first rewrites
the pointer being asked about. Proving it needs three windows — with two, the
named window is already active and the extra selection is a no-op that hides
the difference.

## Answering with values

Every tool returns a typed value, so its shape is published as an output
schema and the value arrives as structured content. Before this, eighteen
tools encoded JSON inside a text block and thirteen returned a bare string —
the old `split_pane` gave `"%3"`, `resize_pane` gave `"80x24"`, and
`kill_server` gave `"killed"` — with nothing describing any of it.

Lists are wrapped in a named object rather than returned bare. The protocol
says structured content is an object, and rmcp will serve a top-level array
happily, so this is a rule to keep rather than a thing the types enforce. The
wrapper also leaves somewhere to put a count or a cursor later.

The cost is real and worth naming: `tools/list` went from about 25 KB to
53 KB, of which 28 KB is output schemas. That is the same budget the
`alwaysLoad` anchors exist to protect, so the two decisions pull against each
other. Output schemas won because an agent that can read what `run_shell_command`
answers with does not have to call it to find out, and because the schemas
carry the field documentation with them.

Where a tool's answer genuinely is prose, it stays prose inside a field:
`capture_pane` returns `{pane, text, lines}` rather than pretending its text
is structured.

## Saying what a failure means

An agent's next move after a failure differs completely by cause: a pane that
closed wants the listing refreshed, a tmux that is not running wants the agent
to stop. Both are failures with a message, and picking between them by reading
prose is guesswork. So every error carries `kind`, `retryable` and `stale` on
its `data`, and the vocabulary is total — a protocol test calls eleven tools in
ways that must fail and asserts all three fields are present on each, because
a classification an agent has to check for the absence of is barely better
than none.

The JSON-RPC code answers a different question — whose move it is — and the
two do not always agree. A pane that dies between two of this server's own
calls is `internal_error`, because the caller did nothing wrong, but it is
classified `stale`, because looking again is still what helps.

The classification travels further than libtmux's own errors. The `find_*`
helpers discover a missing target themselves, by not finding it in a listing,
and the caller guard refuses under its own `self_protection` kind rather than
borrowing `refused` — an agent that reads `refused` might reasonably try
different arguments, and no argument gets past that guard.

One edge is libtmux's rather than ours: a socket with no server behind it
arrives as `Refused`, whose documentation reads "the arguments were wrong",
because the tmux binary did run and did exit nonzero. `Unreachable` would be
the better fit. The message says "no server running", so nothing is hidden,
and the actionable fields are right either way — retrying unchanged will not
help and nothing is stale. Correcting it means changing libtmux's taxonomy,
not string-matching tmux's stderr here.

## The hierarchy as resources

An earlier surface mirrored the live tmux hierarchy with four listable and five
templated resources. That duplicated inspect tools, made URI listings stale as
objects closed, and created a second schema and error surface.

The current server exposes one static resource, `tmux://capabilities`. It
reports the frozen effective tool surface, socket/configuration provenance,
input interpreter boundaries, effects, outputs, and annotations from the same
native registry used for tool registration. Live sessions, windows, panes, and
output remain tool results. Because one process is pinned to one socket, no
resource URI can select another server.

That resource is also the check against registration drift. Every advertised
tool carries the same complete capability row in its `_meta`, including its
native input and output schemas. Listing, calling, documentation generation,
and reporting therefore cannot disagree without a registry test failing.

Selection is frozen before the first tmux command. The four toolsets are an
unordered subset; named inclusion follows expansion, and exclusion wins last.
An empty `LIBTMUX_TOOLSETS` value is the valid zero subset, while an empty
element in a nonempty list is an error. This distinction matters for an
aggregate-only server: `call_read_tools_batch` can be the only advertised tool
and still own its 16 nested inspect routes. Excluding one route prunes both its
dispatch authority and the native operation union in the input schema. With
all 16 excluded, the operation item schema is deliberately unsatisfiable.

Interpreter disclosure follows the value to its actual boundary. A name used
to construct `#{name}` remains a `tmux-format` sink even after validation;
`inputLiteralization.names = validated-variable-name` says why it cannot inject
a format. Literal names, titles, and start directories instead report
`double-hash-once` under the same schema-keyed field.

The read aggregate keeps the full nested MCP envelope rather than extracting
only structured content. Its 1,000,000-byte ceiling is measured on the
serialized outer `CallToolResult`, including the SDK's text rendering. When a
row would cross the ceiling, payload removal is rolled back into explicit
`resultTruncated` and `truncatedBytes` accounting before the response is
emitted.

## A cancelled wait used to keep its connection

rmcp cancels a withdrawn request by firing a `CancellationToken`. Nothing
here watched it, so `wait_for_text` and `run_shell_command` held their control-mode
connection for the whole deadline the caller originally asked for -- long
after anyone was waiting for the answer. A client that cancels routinely, on
an escape key or its own timeout, would accumulate one tmux process per
abandoned wait.

Finding it needed the right test. The obvious one -- disconnect and check for
strays -- passes either way, because a server that exits closes its children's
pipes and tmux reaps them regardless. It even passes with `kill_on_drop`
turned off, which is how it was caught: a mutation that should have broken it
did not. The test that means something cancels one request while the server
keeps running, then asserts the connection is gone and the server still
answers.

Both waits now select on the token, biased so a request cancelled while a
chunk is already in flight stops rather than reading one more.

## Tasks: measured, not adopted

The MCP tasks extension is the one capability whose shape fits this domain.
`run_shell_command` owns the deadline problem itself -- reaching the deadline ends
the waiting, not the command, and the agent is told to send `C-c` -- which is
what not having a task model looks like.

It is not implemented because there is nothing to negotiate with. A probe
server that records what each client declares at `initialize` was registered
with every agent CLI on the development machine. A real opencode session, one
that answered a prompt rather than merely connecting, declares `roots` and
nothing else on protocol 2025-11-25. Gemini's connectivity client declares no
capabilities at all.

Server-side support in rmcp is not the constraint; client support is. Worth
re-measuring with the same probe when a client ships the extension, rather
than shipping a capability nothing asks for.

## Modal terminal interfaces stay client-owned

The core `libtmux` crate retains `Pane::copy_mode` and `Pane::exit_mode` for
applications that own an attached interaction. The MCP omits both operations.
Library parity is broader than a detached agent surface.

Copy mode is pane-global state with a key table, selection, scroll position,
mouse behaviour, and clipboard or pipe actions. An MCP client cannot know who
entered it, and a later `exit-mode` can discard a person's selection or cancel
a different pane mode. A paired enter/exit cleanup also stops being reliable
when the client disconnects between calls.

The retained observation tools cover the agent use case without taking over
that interface. `capture_pane` reads the visible screen or retained history,
`snapshot_pane` adds mode and cursor metadata, `search_panes` can include
history, and `capture_since` follows later output with an opaque cursor.
`run_shell_command` refuses a pane while a mode owns input and tells the caller
to wait for the attached person to leave it.

The `pane_unseen_changes` measurement below describes tmux's historical mode
behaviour. It does not describe a callable MCP copy-mode route.

## What was left out

**A `search_tools` meta-tool.** The Python server has one; it is for servers
whose schema list is too large to browse. The three `alwaysLoad` anchors
address the same problem more directly, and an index would have to be read
before it could save anything.

The budget it would be paid from is measured rather than guessed at:
`cargo run --example budget` reports what a client downloads at `tools/list`,
per toolset and per tool. That number is what makes adding a tool a decision.

**`what_changed` at pane granularity.** tmux has no signal for it. The two
that look like one are not:

- `window_activity_flag` is an alert, and `monitor-activity` defaults to off
  (`options-table.c`), so on a default tmux it is always false.
- `pane_unseen_changes` is set only while a pane is in a mode (`input.c`),
  so it means "wrote while you were scrolled back in copy mode".

`window_activity` is the one that works: tmux stamps it on every byte a pane
writes, whatever the options say. So the tool answers at window granularity
and says so, rather than answering per pane and being wrong.

## What clients actually ask for

Three capabilities were measured the same way before any of them was built: a
probe server that answers just enough protocol to get past the handshake, then
records what each installed agent CLI declares and sends. Client support still
informs the design, but it does not create server-side authority.

| Capability | What a client does | Built |
| --- | --- | --- |
| `progressToken` | Codex attaches one to every `tools/call` | yes |
| `elicitation` | Codex declares it, with `form` and `url` | no internal consent gate |
| MCP tasks | nothing declares it | no |
| `resources/subscribe` | Codex reads resources and never subscribes, even against a server advertising `subscribe: true` | no |

The last row also supports keeping `tmux://capabilities` static. Live hierarchy
resources and `notifications/resources/updated` stay out: they would duplicate
inspect tools and add a capability clients did not exercise. Elicitation is not
an authorization boundary; clients can apply their own approval UI to the four
whole-call annotations.

Worth re-measuring with the same probe rather than re-reasoned about.

## Where tmux disagrees with itself

Running the tools across the supported range turns up behaviour that changes
between releases. Each one is a decision about whether this crate hides the
difference or reports it.

**A split percentage above 100** is refused by tmux 3.7b and accepted by 3.2a.
Hidden: the tool bounds it to `1..=100` itself, so the same call gets the same
answer whatever tmux is underneath. A caller should not have to know which
tmux is installed to predict whether an argument is valid.

**A `$` in a session name** is escaped by tmux 3.2a and 3.4 into the name they
actually *store*: `new-session -s 'dol$lar'` creates a session called
`dol\$lar` there, and `kill-session -t 'dol$lar'` fails while
`kill-session -t 'dol\$lar'` works. 3.7b keeps the name as given. Reported: the
listing shows the name tmux really has, because the alternative is guessing at
per-version escaping rules and corrupting names that legitimately contain a
backslash. An agent on old tmux should kill by the name it was given back.

The line between the two: an *argument* this crate passes to tmux is worth
normalising, because the caller chose it and deserves a predictable answer. A
*name tmux owns* is not, because reporting anything other than the truth breaks
the next call that uses it.

## Focus and geometry

`select_pane` is a tool because changing focus is an action. Position is not:
`pane_at_top`, `pane_at_bottom`, `pane_at_left`, `pane_at_right` and the
`pane_left`/`pane_right`/`pane_top`/`pane_bottom` coordinates are already
available in pane metadata. `find_pane_by_position` resolves the pane touching
a requested edge or direction without accepting a raw tmux format.

## Commands that outlive the call

`run_shell_command` waits for its sentinel-bracketed command and returns the
exit status and bounded output in the same call. Background job handles,
`job_status`, and `forget_job` were prototyped, but they created authority that
outlived the call and a second retained-output lifecycle. They are not part of
the 45-tool surface.

MCP can still run unrelated calls concurrently while a command waits. A caller
that reaches its deadline gets an honest incomplete outcome rather than an
unreachable handle. To interrupt whatever a pane is running, use `send_keys`
with `keys: ["C-c"]`; that pane-wide operation affects unrelated queued input
too.

## Where a fallback has to be bounded

`capture_pane last_command` was measured against a real agent before it was
believed. In a bash pane it answered with 2,014 lines: the shell marks no
prompts, so there was no run to find, and falling back to the history returned
everything the pane had ever written -- the most expensive answer available,
from the request that exists to be the cheapest.

The visible screen is the bounded approximation, and the same probe then
returned 24 lines. The lesson generalises: a fallback for a
cost-reducing feature has to be cheaper than the thing it replaces, or it
inverts the feature.

`marks` is reported for the same reason. An answer that fell back looks
exactly like a command that printed a great deal, and an agent cannot tell
those apart from the text.

## What shell integration actually costs

OSC 133 is what tmux reads to know where a prompt begins, and it was measured
across the shells rather than assumed:

| Shell | Marks prompts by default |
| --- | --- |
| fish | yes |
| bash | no |
| zsh | no |

So prompt-aware capture is exact where it applies and absent for most panes.
It is shipped because it costs one flag and is strictly better where it works,
and because the answer says which case it is. Installing shell integration
into a live pane would widen it, and is not done here: it means typing into a
shell that may not be at a prompt, which is the failure `run_shell_command` already
reports as `no_shell`.
