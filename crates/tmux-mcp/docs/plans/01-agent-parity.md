# Agent parity: caller identity, waiting, focus

What this crate needed to be usable by an agent rather than merely to exercise
`libtmux`. Each decision below was settled by running the alternatives against
real tmux, not by reasoning about them.

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

**Invisible APC sentinels on the output stream — chosen.** The command is
bracketed by two `printf` calls emitting APC strings, which terminals discard:

```
printf '\033_<nonce>s\033\\'; ( <command> ); s=$?; printf '\033_<nonce>e;%d\033\\' $s
```

`ControlMode` is attached before the keys are sent, so the reply arrives on a
byte stream that began earlier than the command. Everything between the two
sentinels is the command's true output, stdout and stderr interleaved in the
order the program wrote them, with nothing scrolled past and no screen
rendering in the way.

The echo of the typed line cannot be mistaken for a sentinel. A shell echoes
the *source* text, in which `\033` is four characters; the escape byte `0x1b`
appears only when `printf` runs. Matching the raw byte sequence is therefore
unambiguous, which is what lets this replace the regex-over-`capture-pane`
scrubbing the Python server needs.

The command is wrapped in `( ... )` because a bare `exit` would otherwise end
the caller's shell.

## Waiting for text

`wait_for_text` reads the same stream rather than polling `capture-pane`. That
removes the two failure modes polling has: output that scrolls past between
polls is still seen, and there is no grid anchor for tmux to invalidate when
`history-limit` trims. Neither is there a reason for a wait ceiling, so a wait
is bounded only by the deadline the caller asked for.

The trade is real and worth stating: a stream is what programs wrote, not what
the screen shows. Escape sequences are stripped before matching, but a
cursor-addressed redraw — a progress bar rewriting one line — is not resolved
the way a terminal would resolve it. For detecting that text appeared, which is
what a wait is for, the stream is the better source.

## Incremental capture

`capture_since` keeps a live tail per pane instead of anchoring into
scrollback. The cursor names an offset in a byte ring the crate owns, so
"what changed since I last looked" is answered exactly, and the answer is
`missed` only when the ring itself overflowed — a condition this crate can
observe, unlike scrollback tmux has already trimmed.

## Two tmux details that read the wrong way

Both cost a working tool and a test that agreed with the bug, so they are
written down rather than left to be rediscovered.

**`Server::shutdown` is not `kill-server`.** It closes this crate's subprocess
executor and, as libtmux's own documentation says, "never stops the tmux
daemon itself". A `kill_server` built on it reports success while every
session survives, and leaves the handle unusable for later calls. Worse, a
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
`split_pane` gave `"%3"`, `resize_pane` gave `"80x24"`, `kill_server` gave
`"killed"` — with nothing describing any of it.

Lists are wrapped in a named object rather than returned bare. The protocol
says structured content is an object, and rmcp will serve a top-level array
happily, so this is a rule to keep rather than a thing the types enforce. The
wrapper also leaves somewhere to put a count or a cursor later.

The cost is real and worth naming: `tools/list` went from about 25 KB to
53 KB, of which 28 KB is output schemas. That is the same budget the
`alwaysLoad` anchors exist to protect, so the two decisions pull against each
other. Output schemas won because an agent that can read what `run_command`
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

Tools are what an agent calls; resources are what a person attaches. A client
shows resources in a picker, so "put that pane in the conversation" is a
gesture someone makes rather than a call an agent has to be talked into. That
is the whole reason these exist: every value behind them is already reachable
through a tool, so a client without resource support loses nothing.

Nine URIs mirror the hierarchy, four listable and five templated. Templates
carry the ones that name something, because a listing that enumerated every
pane would go stale the moment a pane closed.

Two details are not obvious. Session names are percent-decoded and pane ids
are not: tmux spells a pane `%1`, so the sigil that starts every id is also
the character that starts an escape. Decoding would make `%25` ambiguous
between pane 25 and an encoded percent, and pane 25 is the reading that can
actually occur. And the Python server takes a `{?socket_name}` query on each
resource because it picks a server per call; this one is bound to a socket at
launch, so that query would be a way to reach a tmux the operator did not
choose. It is left out.

Errors carry the same `kind`/`retryable`/`stale` classification the tools use,
so a client that reads those fields does not need a second vocabulary.

## A cancelled wait used to keep its connection

rmcp cancels a withdrawn request by firing a `CancellationToken`. Nothing
here watched it, so `wait_for_text` and `run_command` held their control-mode
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
`run_command` owns the deadline problem itself -- reaching the deadline ends
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

## What was left out

**Resources.** The protocol offers a browsable URI space alongside tools, and
the Python server publishes one: `tmux://sessions`, `tmux://sessions/{name}`,
`tmux://panes/{id}/content`, and so on. It is not built here.

The data is identical to what `describe`, `list_windows`, `list_panes` and
`capture_pane` already return, so a resource space is a second path to the same
answers — the thing the surface is meant to stay clear of. What resources add
over tools is that a *person* can attach one to a conversation, and that is
exactly where tmux is a poor fit: attaching `tmux://panes/%1/content` pins what
a pane showed at the moment it was attached, and a pane's whole value is being
current. An agent that wants to know what a pane holds should call a tool and
get the answer as of now.

Worth revisiting if a client appears that subscribes to resources and re-reads
them, since that turns the staleness argument around.

**A `search_tools` meta-tool.** The Python server has one; it is for servers
whose schema list is too large to browse. Measured here, `tools/list` is around
25 KB across 31 tools. The three `alwaysLoad` anchors address the same problem
more directly, and at this size an index would cost more than it saves.

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
filter fields on the pane target, so "the bottom-right pane" is one
`find_panes` expression and needs no tool of its own.
