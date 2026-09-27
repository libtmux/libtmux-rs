# Findings

What tmux, the platforms and this crate's own gates turned out to do, each
found by a test or a CI lane, and what the crate does about it. The
[design notes](design.md) explain the shape of the crate; this is the
catalogue of what that shape had to survive.

## Contents

- [An option refusal has three answers, not four](#an-option-refusal-has-three-answers-not-four)
- [The server and session environments are two stores, merged late](#the-server-and-session-environments-are-two-stores-merged-late)
- [`run-shell` output goes nowhere on tmux 3.3 through 3.4](#run-shell-output-goes-nowhere-on-tmux-33-through-34)
- [Muting a control-mode pane kills the server before tmux 3.7](#muting-a-control-mode-pane-kills-the-server-before-tmux-37)
- [A one-binding key listing goes to the message log on tmux 3.7 through 3.7c](#a-one-binding-key-listing-goes-to-the-message-log-on-tmux-37-through-37c)
- [A `wait-for` client cannot be taken back out](#a-wait-for-client-cannot-be-taken-back-out)
- [Two shapes that make a test flaky under load](#two-shapes-that-make-a-test-flaky-under-load)
- [An idle fixture process must idle the right way](#an-idle-fixture-process-must-idle-the-right-way)
- [A client's attachment is a name, so it is read as an id instead](#a-clients-attachment-is-a-name-so-it-is-read-as-an-id-instead)
- [A name with `:` or `.` needs a terminator, and `=` does not help](#a-name-with--or--needs-a-terminator-and--does-not-help)
- [`list-clients` collapses three ways of being absent](#list-clients-collapses-three-ways-of-being-absent)
- [`display-message` answers about a pane you did not ask for](#display-message-answers-about-a-pane-you-did-not-ask-for)
- [A socket path does not identify a tmux server](#a-socket-path-does-not-identify-a-tmux-server)
- [Budgets, because tmux is an unbounded producer](#budgets-because-tmux-is-an-unbounded-producer)
- [Control mode needs its own budgets](#control-mode-needs-its-own-budgets)
- [Which tmux releases the lanes build](#which-tmux-releases-the-lanes-build)
- [macOS is tested, not assumed](#macos-is-tested-not-assumed)
- [What the first macOS lane found](#what-the-first-macos-lane-found)
- [A pane accepts about a kilobyte of input at once](#a-pane-accepts-about-a-kilobyte-of-input-at-once)
- [The short name belongs to the honest form](#the-short-name-belongs-to-the-honest-form)
- [Fuzzing the parsers that read from outside](#fuzzing-the-parsers-that-read-from-outside)
- [The public surface is recorded, because nothing else reports drift](#the-public-surface-is-recorded-because-nothing-else-reports-drift)
- [The MCP server bounds the tmux side, not just its own answers](#the-mcp-server-bounds-the-tmux-side-not-just-its-own-answers)
- [Example coverage is measured, because the gap is invisible](#example-coverage-is-measured-because-the-gap-is-invisible)
- [Waiting was missing from the production surface](#waiting-was-missing-from-the-production-surface)

## An option refusal has three answers, not four

tmux exits 1 for every way an option can be refused, so the classification
reads stderr. It carries four distinct strings, which is what Python libtmux's
`handle_option_error` matches on, but they reduce to three answers: `invalid
option` and `unknown option` both mean no option goes by that name, `ambiguous
option` means the name is a prefix of several, and `bad value` (a flag) and
`value is invalid` (a number) both mean the option will not hold the value.

Python raises a separate `UnknownOption` for `unknown option`. Reading 3.2a,
3.4, 3.5a, 3.6, and 3.7b, that branch is unreachable: `cmd-set-option.c` and
`cmd-show-options.c` both call `options_match` first, which either fails with
`invalid option`/`ambiguous option` or returns the canonical table name. Only
then do they call `options_scope_from_name`, whose own table walk is where
`unknown option` lives -- and it is being handed a name that walk just found.
The prefix is still matched, mapped to the same kind, because the two spellings
mean the same thing and the ordering is tmux's to change.

`real_tmux_compat_error_option_refusal_wording_is_recognized` pins all of it
against whichever tmux the lane runs.

## The server and session environments are two stores, merged late

tmux does not layer the session environment over the server one, and it does
not copy the server's into a session when the session is created. They stay
separate for the session's whole life, and are merged only at the moment tmux
starts a process.

That is observable, and it is the opposite of what the obvious guess predicts:

- `show-environment -t <session> NAME` reports `unknown variable` for a name
  set with `set-environment -g`, whether the session was created before or
  after the global entry existed. Reading a session is not a fallback.
- A pane started in that session is nonetheless handed the value.
- Where both stores hold a name, the process gets the session's.
- A name marked with `-r` is *absent* from the process's environment, not
  empty. This is why `EnvironmentEntry::Removed` is a state of its own rather
  than being folded into absence: absence in the store and absence in the
  merge are different things, and only the first is `None`.

So the two accessors report what each store holds, and neither predicts what a
pane will be handed. `a_started_process_gets_the_server_and_session_environments_merged`
pins the merge itself, by reading the variables back out of a running process,
because that is the only place the rules are visible.

`Server` and `Session` therefore share `internal::environment`, parameterised by
a `Scope` that is either `-g` or `-t <target>`. The part worth sharing is not
the flag but the reading: a value containing a newline occupies more than one
line of `show-environment`, and a continuation line holding an `=` cannot be
told from the next variable. `show-environment -s` prints each entry as a
shell statement with every `"`, `\`, `$` and backtick in the value escaped, so
the first unescaped `"` ends a value on every supported release and the whole
environment is one command. Only that escaping is undone: tmux 3.4 and later
also escape bytes for display, `$` among them, in both listings alike, so a
value read whole matches the same value read by name.

## `run-shell` output goes nowhere on tmux 3.3 through 3.4

`Server::run_shell` reads the command's output from the client's stdout,
which tmux writes with `cmdq_print` when it has no pane to write into. Three
releases do not: 3.3, 3.3a, and 3.4 replaced that branch in
`cmd_run_shell_print` with one that finds a pane and appends to its copy-mode
buffer instead. The command still runs, and tmux still exits zero, so the
caller is handed an empty listing for a command that printed.

The listing would be indistinguishable from a command that genuinely printed
nothing, so the crate refuses instead, with `Error::CapabilityDefective`. That
variant exists because `require` cannot describe this shape: it asks for a
floor, and a floor would refuse 3.2a, which works. Both sides of the range are
fine and only the middle is not, so the error names the range rather than a
minimum.

The range was read from the release tarballs of 3.2a, 3.3, 3.3a, 3.4, 3.5,
3.5a, 3.6, and 3.7b, and confirmed by building 3.2a, 3.4, and 3.7b and running
`run-shell` against each: 3.4 returns an empty stdout with status zero where
the others return the output.

CI found this, not the local gate -- the workspace's own tmux is unaffected,
which is exactly what the compatibility lanes are for.

## Muting a control-mode pane kills the server before tmux 3.7

`ControlSender::mute_pane` takes a pane out of a control client's stream with
`refresh-client -A <pane>:off`. On 3.2a, 3.4, 3.5a and 3.6b that call can kill
the tmux server outright, and the crate pauses the pane instead below
`since::CONTROL_PANE_OFF`.

tmux keeps one read buffer per pane and one offset into it per consumer, and
drains the buffer up to the least-advanced offset. `control_pane_offset`
returns nothing for a pane that is off, so the pane's offset stops holding the
buffer back while the output blocks already queued for it stay queued. The
next drain moves the base offset past that offset, `window_pane_get_new_data`
computes `used = offset - base_offset` as an unsigned subtraction, and the
result wraps: `control_append_data` reads from a pointer far past the buffer
and the server segfaults. Every command after it reports `server exited
unexpectedly`.

Pausing is the same idea without the defect. `control_pause_pane` discards the
pane's queued blocks as it pauses, on every supported release, so nothing is
left holding a stale offset. What it costs is back-pressure: tmux keeps
draining a paused pane's terminal, where a pane that is off lets the write
block. That is the trade below 3.7, and it is the right way round.

Upstream added the same discard to `control_set_pane_off` in tmux 3.7. The
range was measured rather than read: a fixture that floods three panes, mutes
them mid-write and asks whether the daemon is still there kills 3.2a, 3.4,
3.5a and 3.6b on every run and leaves 3.7b up.
`real_tmux_compat_muting_a_producing_pane_leaves_the_server_up` is that
fixture, and the queue is its whole subject -- muting an idle pane never
reaches the defect, which is why the flood test beside it passed on every
release for as long as it has existed.

This surfaced as an intermittent failure two crates away, in a `tmux-mcp` test
that asserted a tool accepted the arguments its schema describes. tmux reports
a server that died the same way it reports a command it refused, so the
assertion blamed the arguments. `TestServer::daemon_state` exists because of
that: a test driving tmux cannot tell the two apart from the reply, and the
fixture is the daemon's parent, so it is the only thing that can.

## A one-binding key listing goes to the message log on tmux 3.7 through 3.7c

`list-keys` gained `-F` in 3.7, and in the same release its print loop reads
`if ((single && tc != NULL) || n == 1) status_message_set(...)`: a listing of
exactly one binding becomes a status message instead of a line of output. With
no attached client the message goes to the server's message log. The command
still exits zero, so `list-keys -T <table>` on a table holding one binding
answers with nothing, in the `bind-key` form and the `-F` form alike.

`Server::typed_key_bindings` therefore never passes `-T`. It lists every table
and narrows the rows itself, and across every table a listing is one binding
only on a server with a single binding left. `Server::key_bindings` still
sends `-T`, since its lines are tmux's own, and says so.

The source of 3.7 and 3.7c has the condition and 3.8-rc does not. Measured:
3.7c prints nothing for a one-binding table and 3.7d prints it.
`real_tmux_compat_key_bindings_read_as_fields` binds two one-binding tables,
and fails on 3.7c when `-T` is sent.
## A `wait-for` client cannot be taken back out

tmux queues a `wait-for` client on the channel and offers nothing to withdraw
it. `cmd_wait_for_signal` releases every waiter it finds and keeps the signal
only when it finds none; `cmd_wait_for_unlock` grants the lock to the first
client queued for it. `server_client_lost` frees every other structure a lost
client owns -- its files, its overlay, its prompt, `input_cancel_requests` --
and never touches `wait_channels`. The client leaves the `clients` list, so
`cmdq_next` never runs its queue again, and the queued item still holds the
reference `cmdq_append` took, so nothing is freed and nothing dangles: one
client struct and one queue item leak, and the channel's list keeps an entry
that can never act. Identical from 3.2a through 3.8-rc; 3.8-rc's new
`wait-for -l` lists the entry by name after the client is gone, which is the
one-line proof.

So a client killed for running out of time takes the channel's next signal, or
its next lock, with it. `internal::wait_for` answers by never killing one: the
dispatch runs without a deadline of its own (`CommandRequest::without_deadline`,
the only request that does), and the caller's deadline ends the *wait* rather
than the client. A lock granted after its caller gave up unlocks at once. A
wait's client stays parked on the channel, at most one per channel per handle
so that a second wait joins it rather than adding a second waiter, and the
signal that releases it is kept in `ChannelWaits` when no caller is left --
tmux keeps a signal nobody is waiting on, and cannot see that a parked client
is nobody.

Two things this does not reach. A process other than this one waiting on the
channel afterwards does not see that signal: it was spent in tmux, and the
only way to put it back is `wait-for -S`, which would release somebody else's
wait. And `Server::shutdown` kills what is parked, because a shutdown that
waits on tmux is not a shutdown, which leaves the tmux defect behind on a
channel that was still parked.

Routing a blocking `wait-for` is refused for a different reason.
`cmdq_fire_command` writes the block's `end` guard as soon as `entry->exec`
returns, and `cmd_wait_for_wait` returns `CMD_RETURN_WAIT` with its item still
on the queue, so a control client is told the command finished and then runs
nothing else until the channel releases it. Measured through the crate on 3.7d: a
routed `wait_for_channel` on a channel nobody signalled returned `Signalled`
in 190us, and the next routed call answered nothing within two seconds. So
`wait_for_channel` and `lock_channel` refuse a routed handle with
`ControlModeErrorKind::BlockingCommand`; `signal_channel` and
`unlock_channel` do not block and still route.

## Two shapes that make a test flaky under load

Both of these passed locally for a long time and failed in CI, which has fewer
cores than a developer machine and runs the whole workspace at once. Neither
was a timeout that needed raising.

**A poll loop must sleep, not yield.** The subprocess tests wait on a separate
process: a child writing its PIDs to a file, or a process to become reapable.
Written with `tokio::task::yield_now`, the waiting task never gives up its
worker thread, so on a two-worker runtime it competes with the very process it
is waiting for. The deadline it then misses is one it caused. They sleep a
millisecond instead, which is still far more often than anything being waited
on can change.

**Two deadlines of the same magnitude race.** `never_observe_fallback_cleans_an_exited_leaders_group`
set a 50ms lifecycle timeout alongside a 50ms observer interval, and asserted
the error was `DaemonExited`. Under load the clock won and the error was
`StartupTimedOut` -- a different path, saying nothing about the one under
test. The ceiling now sits far above the interval. It costs nothing, because a
daemon that exits is noticed when it exits, not when the timeout expires.

**A deadline under test must still lose to setup.** Two subprocess tests gave
the executor a 100ms deadline and then read back a PID the child publishes on
startup. When the deadline wins, the child is killed before it writes, and the
read waits out its own five seconds for a file nobody will write -- so the
failure names the read, not the deadline that caused it. Re-executing the test
binary takes longer on a CI runner than on a developer machine, which is why
only CI saw it. Shrinking the deadline to 1ms reproduces it exactly, which is
how the mechanism was confirmed rather than guessed.

**The same rule applies to blocking waits, and harder.** The fixture's
shutdown polls for a daemon to exit from inside `spawn_blocking`, and it did
so with `std::thread::yield_now`. That spin holds a core the daemon needs to
handle the `SIGTERM` it was just sent, so on a machine with fewer cores than
the suite has concurrent fixtures the grace window expires and cleanup reports
that the daemon did not exit. It was invisible on a twenty-core developer
machine and failed six tests on a macOS runner.

The general rule: a test's deadline should bound the thing it is *not*
testing, by enough that it never becomes the thing it measures.

The rule was written and the deadlines it was about stayed constants. Five
seconds bounds a tmux that starts with a core to spare; on a machine running
several times its cores in work it bounds nothing, and the fixture suite fails
in a set that moves between runs while every member passes alone. That shape
is the signature -- a defect fails the same way every time -- and reading it
takes repetition rather than a result: a run that fails four tests and then
four different ones is saying something a single red run cannot.

`LIBTMUX_TEST_TIMEOUT_SCALE` multiplies every fixture deadline, read once so
two tests in a run cannot measure against different clocks, and never below
`1` because nothing here wants a fixture to fail sooner. Unset, the deadlines
are unchanged, so an idle machine behaves exactly as before. It moves a
ceiling rather than fixing a wait, and a test that synchronises by sleeping
still races -- it is the knob for a loaded machine, not a substitute for
waiting on the right thing.

## An idle fixture process must idle the right way

The process fixtures held a process open with `while :; do :; done`. Nineteen
sites, each burning a core for the length of its test, and the suite runs them
beside tests that measure how long a child takes to start. On a developer
machine with twenty cores that is invisible; on a four-core runner it is what
makes those measurements miss.

Replacing it is not a substitution, because two different things were relying
on the spin:

- **A shell only runs traps between commands.** One blocked in `sleep` defers
  a TERM until the sleep ends, so the fixtures that assert on signal delivery
  broke when the spin became `while :; do sleep 30; done`. `sleep 86400 & wait`
  keeps the prompt handling, because a signal with a trap interrupts `wait`.
- **`exec` takes the trap with it.** `exec sleep 86400` is a single process
  and burns nothing, but it replaces the shell, so any fixture that installed
  a trap first lost it.

So the shape follows the fixture. Helpers that hold a trap use
`sleep 86400 & wait`. Helpers that only have to outlive an assertion use
`exec sleep 86400`, which matters where the helper carries an environment
marker: a `sleep` child inherits it, and a scan that counts marker-bearing
processes then finds two where the test means one.

Measured by pinning the lib suite to two cores at eight test threads: one
failure in six runs before, twelve clean runs after.

## A client's attachment is a name, so it is read as an id instead

The format catalog gives `client_session` and `client_last_session` the
semantic owner `ClientAttachment` rather than `Client`, and leaves them
catalog-only, so the client snapshot has no session field. That classification
was recorded before the reason for it was, and the parity ledger stalled on
"is an attachment part of a client's identity?" -- a question with no useful
answer, since identity here is `(ServerIdentity, client_name)` and every other
field in the snapshot is mutable state too.

The real reason is narrower and settles it. `format_cb_client_session` returns
`c->session->name`, so `client_session` is a *name*, and a name is not a handle
in tmux: this crate has already established that tmux will create a session
called `a:b` and then refuse to address it, because `:` separates a session
from a window in a target. Projecting the field would put a value in the
snapshot that a caller cannot reliably turn back into a `Session`.

A client's format tree resolves the whole chain as ids, which is what the
accessors use:

```console
$ tmux list-clients -F '#{client_session} #{session_id} #{window_id} #{pane_id}'
plain $0 @0 %0
```

So `Client::attached_session`, `attached_window`, and `attached_pane` each
read one id and hand it to the existing by-id lookup. `client_session` stays
catalog-only, and no `ClientAttachment` type is needed: the ownership it
records is about format semantics, not about a public struct.

Two of the three carry a caveat worth stating rather than discovering.
`curw` is a member of `struct session`, not `struct client`, so the window a
client reports is the session's current window: every client attached to that
session reports the same one, and one client changing it changes it for all of
them. The pane follows from the window, because tmux keeps no per-client
focus.

## A name with `:` or `.` needs a terminator, and `=` does not help

`Server::session` and `Server::has_session` find a session named `a:b` or
`a.b` on any release that keeps such a name at all, because they compare
`list-sessions` output in process rather than asking tmux to resolve a `-t`
target for that name. Building a `-t` target from the bare name instead
misreads it on every release: `cmd-find.c` splits the target on the first
`:`, then on the first `.` in whatever follows the colon, before it ever
looks for a leading `=`. `-t my.proj` reads as window `my`, pane `proj` in
the current session; `-t =my.proj` fares no better, because `=` only marks
whichever piece the split left it attached to as exact, and that piece is
not the whole name.

Appending a trailing `:` sidesteps the split: `-t my.proj:` and
`-t =my.proj:` both resolve session `my.proj`, because the colon consumes
the split point and leaves nothing after it for `.` to divide. That works on
tmux 3.7a and later, which is also the range that keeps such a name rather
than rewriting or refusing it. Measured against 3.7a, 3.7c, and master with
`display-message -t <target> -p '#{session_name}'`.

None of this makes the name safe to hand out. `name:` is not a spelling
anyone reaches for, an ordinary `-t name` from a person or another tool
still misreads it, and a session created before 3.7a never had the name to
begin with. `tmux-workspace` refuses `:` and `.` in a `session_name` up
front for that reason: a workspace built around a name most tooling cannot
address is not a workspace `load` could reliably run again. Code in this
crate that must build a `-t` target from something other than an ID resolves
the session first and targets it by `SessionId`, which parses with no split
at all.

## `list-clients` collapses three ways of being absent

A client that is suspended, one that is locked, one that is dying and one that
has already gone all look the same from `list-clients`: absent. `sort.c`'s
`sort_get_clients` skips any client carrying `CLIENT_UNATTACHEDFLAGS`, and
`tmux.h` defines that as `CLIENT_DEAD|CLIENT_SUSPENDED|CLIENT_EXIT`. So the
listing answers "not attached right now", and the crate was reading it as "not
there any more".

The two are a different instruction to a caller. `Error::is_object_gone` is
what decides whether to discard a handle, and a suspended client is listed
again the moment its process continues -- `SIGCONT` for a suspended one, the
`lock-command` exiting for a locked one. Locking is the larger half: it sets
the same flag through `server_lock_client`, so `Client::lock`, `Session::lock`
and `Server::lock_all` all reach it, and `lock-after-time` reaches it with
nobody asking.

tmux does publish the difference; it is just not in the listing.
`server_client_get_flags` puts `suspended` in `#{client_flags}`, and
`display-message` carries `CMD_CLIENT_CANFAIL`, so a target it cannot resolve
expands every format empty and exits zero rather than erroring. A client that
is merely stopped still resolves and names itself. `Client::refresh` asks only
on the miss path, and only tmux's own answer counts: a name that comes back
matching, carrying that flag, is `Error::ClientSuspended`; every other shape,
including a probe that fails outright, stays `Error::ObjectGone`. The probe can
turn a suspended client into something other than gone, never a gone client
into a live one.

Both mechanisms date to 3.2a, which is `MIN_SUPPORTED`, so this needs no
version gate. The filter does not: 3.2a and 3.5a screen `list-clients` on
`c->session == NULL` alone, and `server_client_suspend` never clears the
session, so a suspended client stays listed on those releases and the miss path
is never taken. Read from their sources rather than measured. The two answers
differ and neither is false -- which is the argument for keying on the flag
rather than on the absence.

## `display-message` answers about a pane you did not ask for

`display-message` is the obvious way to ask tmux what a target resolves to, and
it is not an oracle. Its entry declares two separate permissions to fail, and
only one of them is the one above:

```text
.target = { 't', CMD_FIND_PANE, CMD_FIND_CANFAIL },
.flags  = ...|CMD_CLIENT_CANFAIL,
```

`CMD_CLIENT_CANFAIL` governs `-c`: a client that does not resolve expands every
format empty, which is what makes the suspended-client probe work.
`CMD_FIND_CANFAIL` governs `-t` and does something else entirely. An
unresolvable `-t` leaves the target unresolved, so the formats expand against
the client's current pane and the command still exits zero:

```text
current window: @2
-t home:@99    -> @2
-t home:9      -> @2
-t home:nosuch -> @2
-t home:%99    -> @2    a pane id in a window target, still @2
```

Nothing separates "resolved to this" from "resolved to nothing, so here is
where you happen to be standing". A test that asks `display-message` whether a
rendering reaches the right window therefore passes whenever the right window
is also the current one -- which a fixture that just built it guarantees. That
is a probe that cannot fail, and one shipped here in the first version of
`a_rendered_window_target_survives_a_renumber`.

A command whose target is not `CMD_FIND_CANFAIL` refuses instead, which is the
answer a probe wants. `select-window` is the cheap one, and it leaves the
current window alone when it fails. Measured on tmux 3.7c.

## A socket path does not identify a tmux server

`ServerIdentity` is a normalized socket path, and object equality includes it,
so `%0` on two different sockets are two different panes. That is necessary and
not sufficient: the same socket can host more than one server over time, and
the crate could not tell them apart.

Three tmux behaviours combine into a hazard rather than an inconvenience:

- the socket file outlives the daemon -- it is still on disk after
  `kill-server`, and a replacement binds the same path;
- a replacement reissues ids from the start, so its first pane is `%0` too;
- neither the path nor the id carries any mark of which daemon it belongs to.

So a handle held across a restart resolves. It names a real object, and not
the one it meant. A stale *read* is harmless; a stale `kill-pane` or
`send-keys` lands on whatever now wears that id.

`ServerGeneration` is `(pid, start_time)`, read with one `display-message`.
The start time is what makes it a generation rather than a guess: a
replacement daemon can be handed the pid of the one it replaced. Both are
server-scoped -- `start_time` is identical across every session of one daemon,
unlike `session_created` -- and both have been in the format catalog since
3.2a with `ListScope::All`, so a later change can project them into every
listing row and give each snapshot its generation at no extra round trip.

Detection is deliberately explicit rather than automatic. Verifying on every
dispatch would double the command count for a hazard that only exists when a
caller holds a handle across a restart, so `require_generation` is something
the caller reaches for around work that must not be misapplied.

A cheaper token was ruled out by measurement rather than reasoning: the socket
*inode* is unchanged across a restart, because tmux reuses the file rather
than recreating it.

## Budgets, because tmux is an unbounded producer

Two resources had no ceiling, and both are the caller's process rather than
tmux's.

**Output.** Each dispatch drained stdout and stderr with `read_to_end`. A pane
with a long history, a buffer someone pasted a file into, or a `run-shell` that
keeps printing all answer with as many bytes as they have, so the operating
system decided when to stop -- by killing the process. `OutputLimits` bounds
the read where the allocation happens, by taking `limit + 1` bytes and failing
if the extra one arrives.

It fails rather than truncating. A truncated tmux listing is a *shorter
listing*: it decodes cleanly and reports fewer panes than exist, which is worse
than an error because nothing downstream can tell. A caller who wants less asks
tmux for less.

The default is 32 MiB of stdout and 1 MiB of stderr -- generous on purpose. The
point is that a ceiling exists and names itself in an error, not that it is
small. A budget below a listing row breaks every command, which is worth
knowing: the crate's own snapshot projection is a few hundred bytes, and a
64-byte budget was enough to fail `new-session` during testing.

**Dispatches.** Nothing bounded how many tmux clients ran at once. A caller
that fans out -- an agent driving the MCP server, a reconciler sweeping every
pane -- turned its own concurrency into process, descriptor, and memory
pressure, and tmux serializes on the far side regardless, so the extra clients
bought queueing rather than throughput. `DispatchLimits` is a semaphore
acquired before the request is registered, so a refusal costs nothing.
The command deadline starts before that wait. An explicit admission timeout
may shorten it, but cannot extend it.

`Error::Overloaded` is deliberately distinct from `Error::Timeout`: overload
means the work never started, so retrying is safe, where a timeout means
tmux may have run the command already.

Both are measured rather than asserted. The admission test times twelve
dispatches through two permits and fails if they finish in less than the
rounds require; run with the limit raised to 64 they finish in 138ms, which is
what the test is written to catch.

## Control mode needs its own budgets

A subprocess dispatch ends when the process does, which bounds it whatever
else is true. Control mode does not: it reads a framed text protocol from a
tmux that keeps running, so the framing is the only thing standing between a
malformed or unexpectedly verbose answer and unbounded memory. Two shapes
grow, and they grow differently:

- a line that never ends, accumulated across reads because a cancelled
  `read_until` leaves its bytes behind for the next one;
- a `%begin` block whose `%end` never arrives, which grows one *valid* line at
  a time and so cannot be caught by a line budget.

`ControlLimits` bounds both. Neither is recoverable in place: the parser is
mid-frame and does not know where the next one starts, so the connection is
finished and a caller who wants to continue attaches again.

What a caller is told matters as much as the bound. The first version ended
the connection correctly and reported `ControlMode { kind: Closed }` to
everything still waiting, which is true and useless -- a caller who blew a
budget can raise it, where one who merely lost the connection can only
reconnect. The frame reason now reaches the pending requests instead.

The budgets are large -- 8 MiB for a line, 64 MiB for a block -- because they
exist to stop unbounded growth, not to police ordinary output.

Connections need a separate count as well. `ControlClientLimits` bounds the
persistent clients owned by one server, independently of `DispatchLimits`.
Combining the two would let a handful of long-lived watchers starve every
short command. Admission lasts until the control process is cleaned up, and a
full lane returns `Error::Overloaded` before another process starts.

Deadlines divide the same way, and for a while did not. One value -- the
server's `default_timeout` -- bounded both the opening handshake and every
command's reply, because attaching seeds it into the actor and the sender
together. Those are not comparable waits: the handshake forks tmux and waits
for a server to come up, where a command is a round trip on a connection that
is already open. A caller who wanted a command to give up in 100 ms was also
telling `attach` to fork a process in 100 ms, and on a loaded machine it
cannot. Three of this crate's own tests were flaky for that reason before
`ControlSender::reply_timeout` existed.

The deadline sits on the sender rather than in `ControlLimits`, which
`attach_with_limits` already threads through, because limits are fixed when
the connection opens and a sender is not. `ControlSender` is `Clone`, and two
clones of one connection can carry different deadlines; the actor honours the
earliest committed one, which is what
`the_earliest_committed_deadline_ends_the_connection` pins. A limit set once
at attach could not express that. What stays on the server is the connection
budget and the default every sender starts from.

## Which tmux releases the lanes build

The final patch of each series rather than its first: 3.2a, 3.5a, 3.6b, and
3.7b are what a distribution ships and what a user runs. 3.4 is the exception,
because that series has no later patch and is one of the two releases that
wrapped command output in `VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`.

The lane was `3.6` and is now `3.6b` for that reason. Note that `3.6` still
appears in the source as a *behaviour boundary* -- the dialect restore landed
in 3.6 itself -- which is a different statement from which build CI runs.

## macOS is tested, not assumed

The platform contract names macOS, and for a long time the evidence was
Linux-only. What differs there is exactly what this crate leans on: process
groups, Unix sockets, `waitid`, and temporary paths. The lane runs the test
suite rather than the whole gate, and on master rather than every push,
because a macOS runner bills at ten times a Linux one while the lints it would
re-run are platform-independent.

## What the first macOS lane found

Adding the lane failed nine tests immediately, which is the argument for
having added it. Three causes, and finding the third took three rounds of
instrumenting rather than guessing.

**`/var` is `/private/var`.** Three tests compared a resolved path against the
raw temporary one. The library is right to canonicalize -- that is what makes
two selectors for one endpoint compare equal -- so the expectations were the
Linux-shaped part.

**A blocking poll loop must sleep too.** The fixture's shutdown waited for the
daemon with `std::thread::yield_now` from inside `spawn_blocking`, holding a
core the daemon needed to handle the signal it had just been sent. This was
the same defect fixed earlier in the async loops, in the one place nobody
looked. It was not the cause of the remaining failures, but it was a real one.

**`killpg` returns `EPERM` on macOS.** The forced sweep of the leader's own
process group fails with "Operation not permitted" once the leader has exited,
on every fixture shutdown, while the leader itself is killed and reaped
successfully in the same cleanup. The daemon is gone; only the sweep
disagrees. There is nothing further to do about a group the kernel will not
let the caller signal, so that errno is accepted away from Linux -- and only
away from Linux, where the same result would be a real permission bug.

Getting there needed the failure to say more than `ShutdownFailed`, which
named four different problems. `TestServerError` now carries the step that
produced it, which is how a fixture on a machine the author does not have is
debugged at all.

## A pane accepts about a kilobyte of input at once

The MCP server runs a command by typing a completion frame into the user's
own shell, so the command inherits the environment, traps and directory that
shell has. The frame is 2.6 KB for `sh` and 4.7 KB for `bash`. On macOS every
run using it lost most of the frame and then waited for a marker that could
never arrive.

**The bound is the burst, not the line.** Measured on the macOS lane across
`sh`, `bash` and `zsh`: a total up to 1024 bytes arrives whole, and past it
the input is truncated or corrupted outright. Splitting the frame into
158-byte lines and sending one `send-keys` for each fails identically, so this
is not `MAX_CANON`, which caps one line at 1024 bytes there against 4096 on
Linux. It is the pty input queue, which drops whatever a burst adds beyond its
depth while the shell is busy rather than reading -- and a shell echoing a
multi-kilobyte command line through its line editor is busy. Linux delivers
8 KB intact through the same path with the reader stalled for three seconds,
so nothing about this reproduces there.

Only sending fewer bytes works. The frame is written to a private file and the
pane is told to read it, which holds the typed line to a few hundred bytes
whatever the frame contains. The line no longer carries the command either,
so its length stopped growing with it -- a command over 4 KB used to produce a
line that Linux truncated too.

**How the pane reads it back is forced, three times over.** Sourcing the file
with `.` gives it its own scope for trap inheritance, so `trap -p ERR DEBUG`
inside reports nothing, the capture restores nothing, and the command runs
without the traps its shell had; `eval` introduces no scope and does not.
Under an inherited `DEBUG` trap that writes to standard output, zsh captures
that output into `$( cat frame )` and hands `eval` the trap's text ahead of
the frame, which then fails to parse; `$(<file)` runs no command for a trap to
precede. And dash accepts `$(<file)` and quietly yields nothing, which would
hang a run, so every other shell reads through `cat` -- safely, because a
shell without `$(<...)` has no `DEBUG` trap to capture.

Each of those three was found by a test that failed, not by reading a manual.

## The short name belongs to the honest form

Both halves of a listing pair existed from the start, and the short name went
to the collapsing one: `sessions()` returned `Vec<Session>` and swallowed the
reason, while `try_sessions()` returned `Result`.

That is the wrong way round, and the reason is not taste. A Rust caller
reaching for `sessions()` expects fallible I/O to be fallible, and gets a
value that cannot be distinguished from a healthy server with nothing running.
For a status line that is fine. For anything that reconciles -- a supervisor,
a cleanup pass, a workspace builder -- "no sessions" read from an outage is an
instruction to delete everything.

So the names swapped. `sessions()` returns `Result`, and a caller who wants
the old behaviour writes `sessions().await.unwrap_or_default()`, which says what
it does at the call site rather than in the method name. An `_or_empty` twin of
each listing did exist for a while; neither consumer crate ever called one, and
eleven methods whose whole purpose is to discard a reason are eleven ways to
discard one by accident, so they went. The
breaking change is cheap now and would not be later, which is the argument for
doing it during an alpha rather than after one.

## Fuzzing the parsers that read from outside

Every surface that takes bytes this workspace did not write is fuzzed:

- the control-mode line parser (`control_line`), which reads from a tmux that
  keeps running, so a malformed line is not a command that failed but bytes it
  has to survive;
- control-mode block framing (`control_block`): a stream read inside and
  outside `%begin` blocks, each closed block handed to the slots that assemble
  a chain's reply, checked that no line escapes its block and no reply holds
  blocks another command owns;
- the format-row codec every listing decodes through (`format_rows`), whose
  names, paths and titles users and programs write, checked by writing values
  the way each dialect of tmux prints them and decoding them back;
- the versioned filter-expression wire format (`filter_expr_json`), which can
  arrive from a config file, a CLI argument, or an MCP tool call, checked that
  what it accepts writes out and reads back as the same expression;
- the tmuxp-style workspace loader (`workspace_yaml`), which walks a
  hand-written nested document deciding what each value means;
- tmux-mcp's escape filter (`text_filter`), which reads pane output, checked
  that text written after a sequence tmux would have ended is never swallowed.

`fuzz/` is not a workspace member. It needs nightly and a sanitizer, and
`just check` has to stay runnable on stable, so it is excluded and reached
through `just fuzz <target>`.

The seeds are the part worth explaining. Random bytes almost never produce a
line beginning with `%`, so an unseeded control-mode target spends its entire
budget establishing that arbitrary input is text and never reaches `%begin`,
`%output`, or the block-number parsing that correlates a result with its
command. `fuzz/seeds/` carries those shapes -- including a line that is not
UTF-8, because pane output is not required to be. What the fuzzer discovers
from them is not checked in; the seeds are.

The `__fuzz_*` functions exist because the parsers are private and should stay
private. They are behind `unstable-fuzzing`, which is not in `full` and which
nothing but `fuzz/` turns on. tmux-mcp's filter needs only `std`, so the
target compiles its source file instead of adding a feature to a published
binary crate.

CI runs them weekly rather than per-push. This kind of testing finds things by
running for a long time, so a schedule is worth more than a gate nobody can
wait for. Each target's corpus is cached between runs, since the corpus is
what grows, and a crash is uploaded as an artifact rather than left in a log.

## The public surface is recorded, because nothing else reports drift

Dropping the `semver` recipe left no mechanical account of what the API does
between releases. `cargo-semver-checks` could not provide one -- it skips every
lint on a prerelease-to-prerelease step and then reports success -- and human
review does not reliably notice a method that quietly changed shape.

`crates/libtmux/docs/public-api.txt` records every public item with its
callable or data signature, plus each non-blanket trait implementation.
`scripts/public-api.py` generates one record per line from rustdoc's JSON.
`just api` regenerates it and `just api-check` fails when the tree and the
record disagree, naming what moved.

It is deliberately not a semver oracle. It says a change happened, and leaves
whether that change is allowed to the person reading the diff -- which is the
right division while the answer is "yes, it is an alpha".

Built from rustdoc rather than a separate tool because the only thing it then
needs is the nightly the fuzz targets already require. Methods, fields, and
variants have no standalone path in that JSON, so they are attributed to the
type that owns them: an unqualified `sessions` would say nothing about which
handle it belongs to, and a move between types would not show at all.

That attribution reached one level, and an enum's variants sit one level
further down. A struct-like variant's fields are items in their own right and
nothing mapped them to the variant holding them, so `Error::LinkGone` recorded
its fields as `kind` and `index` -- bare names that seven other variants of the
same enum also spell. The record carried 42 such lines. Removing both of
`LinkGone`'s fields and adding one produced a diff of one inserted line,
because something else still spelled `kind` and `index`: the gate whose whole
purpose is saying that a change happened could not see the change described
two sections above. Variant fields are attributed like everything else now,
which named 121 of them and left no bare field records.

## The MCP server bounds the tmux side, not just its own answers

`tmux-mcp` already capped what it returns: 256 KiB of captured output, eight
concurrent tails. Those bound the response and nothing else. An agent that
fans out still turned into as many tmux client processes as it had questions,
and truncating a response after the fact does not unspend the memory the core
already allocated to read it.

So the binary now configures the `Server` it builds with a dispatch limit of
four and an output budget, which is where those costs are actually incurred.
Four because tmux serializes commands on its own thread: past that, more
clients buy queueing rather than throughput, and an agent should meet a
bounded queue rather than a fork bomb.

An agent is the caller most able to ask for too much at once and the least
able to notice that it did, which is the argument for the limits being on by
default here rather than something an operator remembers to set.

## Example coverage is measured, because the gap is invisible

"Every public item has a runnable example" was stated as a goal and never
counted. Counting it found 15 of the 67 types a caller reaches through
`use libtmux::X` had no example of their own -- including `Server`, `Session`,
`Window`, `Pane`, and `Error`, which are the pages someone arriving from a
search lands on first.

A type whose *methods* are well documented still leaves that person with
nothing to copy, which is why the measure is per type rather than per item:
`Pane::id` inherits the example on `Pane`, and counting accessors separately
would drown the signal in items nobody needs an example for.

`just example-coverage` reports it, and `example-coverage-check` fails when a
crate-root type has none. The count belongs to that command rather than to
this page, which cannot be re-read when the number moves.

What "runnable" means here changed after this was written. A counted example
was one rustdoc would compile, which is not the same as one that runs: eleven
of them wrapped their body in a hidden function nobody called. `just
doctests-run` closes that, so the coverage number and the guarantee behind it
now agree.

Writing them was worth more than the count suggests. Three doctests failed on
first run and each was a belief this crate held wrongly: `split` is detached by
default, so an example that assumed focus followed the new pane was wrong; a
new session does not copy the server environment; and `status` is not a flag,
because tmux accepts `on`, `off`, and `2` through `5` for it. That last one is
the argument for generating the option schema from tmux's own table rather
than inferring a type from the value, and the example now says so.

## Waiting was missing from the production surface

One kind of waiting is now offered, and it is worth naming so the gap below is
not read as wider than it is. `Server::wait_for_channel` is the blocking half
of `wait-for`, which `signal_channel` had for a long time without it: the
missing side was deferred in that method's own documentation until a wait
running out of time could be told from tmux failing to reply, and that is what
`ChannelWait` now carries. tmux latches a signal nobody is waiting on -- one
signal releases every waiter present, and the latch then releases one later
wait -- so signalling before the wait starts is safe, measured on 3.7c. That
removed the `Server::cmd(wait-for)` workaround from `tmux-mcp`.

It answers a narrower question than the section it sits in. `wait-for` is a
rendezvous between commands: something has to signal it, so it serves "tell me
when this is done" only for work written to announce itself. Watching a pane
that was not is the case below, and a different mechanism answers it.

`Pane::wait_for_text` and `Pane::wait_for_quiet` now answer the pane case, on
the polling path this section argued for: no feature, because a caller who
dispatches a command needs to know when it finished and a doorbell needs
`control-mode`. Each look reads the scrollback with wrapped lines joined,
which is what the two constrained failures demanded -- text that scrolled off
before the look reads as absent, and a line wider than the pane arrives split,
so a needle spanning the wrap never matches. A dead pane ends the wait rather
than holding it to the deadline, and running out of time is
`PaneWait::TimedOut` rather than an error.

Both numbers that would justify a doorbell have now been taken, and they say
something narrower than the argument they replaced. `benches/waits.rs` takes
them against the same pane, on both paths:

```console
$ cargo bench --features test-support,control-mode --bench waits
```

Latency is a capture round-trip rather than a fraction of the poll interval,
because the loop looks before it sleeps. A marker printed into a pane answers
in 5.7ms polled and 3.0ms streamed, measured from dispatching the key that
produces the text. A doorbell removes the round trip rather than an interval,
and the round trip is those under three milliseconds: real, and not what a
caller notices.

A flood is where the interval shows. `seq 1 20000` into a pane, waiting for a
marker printed after its last line: 132ms polled against 21ms streamed.
Polling costs one capture per `POLL_INTERVAL` whatever the pane is doing, so a
flood never reaches it -- but that interval is 120ms, and anything finishing
inside one is rounded up to it.

Ten times the flood closes the gap rather than widening it. At 200,000 lines
polling holds [151, 184]ms and the stream [118, 260]ms: medians within 4% of
each other, reached across an interval 33ms wide against one 142ms wide. The
stream rings per notification where polling looks once per interval however
much arrived in between, and that four-fold spread is where the Swift port's
coalescing comes from. A capture poll cannot have the problem it solves.

So the doorbell stays unbuilt, and what settles it is the feature argument
below rather than the clock. The clock now says it would be worth having --
under three milliseconds on a marker, six times on a moderate flood, nothing
once the flood is large enough -- and a default build still cannot reach it.

`Pane::wait_until` runs the same loop with a predicate over the captured lines,
for what a literal needle cannot say. It polls on a handle from
`Server::over_control_mode` as well, where `control-mode` is on and the feature
argument does not apply. The reason there is ownership: a connection's
`%output` goes to whoever holds its events, and the handle holds only the
sender, so waking on output would attach a second client for every wait -- the
cost the `streamed` lane leaves out of its number. Over a connection a look is
a line rather than a process, so the round trip a doorbell would save shrinks
as well.

What follows is why, and it is kept because the constraints it records are the
ones the implementation had to meet.

`libtmux::test::retry_until` was the only waiting primitive this crate
exposed, and `test` sits behind `test-support`, which the manifest calls out as
belonging "to a dev-dependency, not to a build of the library". A caller who
needed to wait for anything a pane did wrote that loop themselves. It was a
missing category rather than a missing convenience, and it was inherited rather
than dropped in the port: the Python library keeps `retry_until` in
`libtmux/test/retry.py` for the same reason, and of the seven ports only the
Swift one shipped a pane wait a production caller could reach.

What filled the gap downstream is the measure of it. `tmux-mcp` reconstructs
run-and-report in `exec.rs`: sentinels bracketing the command, a scanner
reassembling output around them, and separate waits for text and for quiet.
AGENTS.md says a workaround there is a finding here, and this is the largest
one.

A rebuilt version is not merely incomplete. Thirty-five lines against the
public API run a command and report its exit status correctly, and then
`seq 1 100` returns status 0 with no output: the opening sentinel scrolled off
the visible screen before the closing one arrived, so the body came back empty
while the status still parsed. A three-hundred-character line arrives as four,
wrapped at the pane's width. Both failures report success, which is the
direction that costs a consumer the most.

So a candidate is constrained before it is designed. It must not report success
while losing output, and it must survive a line wider than the pane. Those two
together are what force scrollback capture, `OutputLimits`, and
width-independent reassembly instead of a screen read.

One decision is settled by precedent rather than by measurement: a wait that
runs out of time is an outcome, not an error. `RetryTimeout` already says so,
and a caller who cannot separate "it never happened" from "the connection
broke" has to guess which of them is worth retrying.

The substrate is not settled, and the question is narrower than it first looks.
The Swift port does not choose between streaming and polling. It subscribes to
`%output` as a doorbell and captures for the content, because a notification
carries escape sequences and can split a word across two of them. Around that
sit a primed first capture, so output produced while the connection opens is
not lost; a `#{pane_dead}` subscription, so a dead pane ends the wait instead
of holding it to the deadline; and coalescing, because an unbatched burst is
one notification per character.

Two of the four questions that shape are measured. The machinery exists on
every release the lanes build: `%output` and `%subscription-changed` both
arrive on 3.2a, 3.4, 3.5a, 3.6b, 3.7 and 3.7c, with no errors anywhere, so the
oldest supported release is not the constraint it might have been -- the
`#{pane_dead}` half needs `refresh-client -B`, which landed in 3.2.

The feature cost is the constraint instead, and it settles more than it looks
like it does. A doorbell needs `control-mode`; a capture poll needs only the
base API, and `default = ["query"]`. So a doorbell-only wait would be absent
from a default build -- the capability existing, but not for you, decided by a
flag its signature never mentions. This manifest says a feature is for "API
surface a caller who only dispatches commands never needs", and a caller who
dispatches a command does need to know when it finished: `send_keys` without
that is half of one. Waiting therefore fails the test for being opt-in, which
makes the polling path the floor and the doorbell an optimisation above it
rather than an alternative to it. What the doorbell saves, and what a flood
does to a wait that rings on every byte, are the numbers at the top of this
section.
