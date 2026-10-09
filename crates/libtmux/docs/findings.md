# Findings

What tmux, the platforms and this crate's own gates were found to do, and what
the crate does about each. The [design notes](design.md) explain why the crate
is shaped the way it is.

## Contents

- [Five option-refusal messages map to three error kinds](#five-option-refusal-messages-map-to-three-error-kinds)
- [tmux merges the server and session environments only when it starts a process](#tmux-merges-the-server-and-session-environments-only-when-it-starts-a-process)
- [tmux 3.3 through 3.4 send `run-shell` output to a pane](#tmux-33-through-34-send-run-shell-output-to-a-pane)
- [Muting a control-mode pane kills the server before tmux 3.7](#muting-a-control-mode-pane-kills-the-server-before-tmux-37)
- [A one-binding key listing goes to the message log on tmux 3.7 through 3.7c](#a-one-binding-key-listing-goes-to-the-message-log-on-tmux-37-through-37c)
- [tmux cannot withdraw a queued `wait-for` client](#tmux-cannot-withdraw-a-queued-wait-for-client)
- [Poll loops and deadlines that made tests flaky under load](#poll-loops-and-deadlines-that-made-tests-flaky-under-load)
- [How a fixture process idles without burning a core](#how-a-fixture-process-idles-without-burning-a-core)
- [`Client` reads its attached session, window and pane by id](#client-reads-its-attached-session-window-and-pane-by-id)
- [A session named with `:` or `.` needs a trailing `:` to be targeted](#a-session-named-with--or--needs-a-trailing--to-be-targeted)
- [`list-clients` omits suspended, locked and exiting clients](#list-clients-omits-suspended-locked-and-exiting-clients)
- [`display-message` falls back to the current pane when `-t` does not resolve](#display-message-falls-back-to-the-current-pane-when--t-does-not-resolve)
- [A socket path does not identify a tmux server](#a-socket-path-does-not-identify-a-tmux-server)
- [Limits on tmux output and concurrent dispatches](#limits-on-tmux-output-and-concurrent-dispatches)
- [Control-mode limits and reply deadlines](#control-mode-limits-and-reply-deadlines)
- [Which tmux releases the lanes build](#which-tmux-releases-the-lanes-build)
- [The macOS lane and what it found](#the-macos-lane-and-what-it-found)
- [On macOS a pane accepts about a kilobyte of input at once](#on-macos-a-pane-accepts-about-a-kilobyte-of-input-at-once)
- [Listing methods return `Result` under the short name](#listing-methods-return-result-under-the-short-name)
- [Fuzz targets for the parsers that read external bytes](#fuzz-targets-for-the-parsers-that-read-external-bytes)
- [`public-api.txt` records the public surface](#public-apitxt-records-the-public-surface)
- [`tmux-mcp` limits its own dispatches and output](#tmux-mcp-limits-its-own-dispatches-and-output)
- [Example coverage is counted per crate-root type](#example-coverage-is-counted-per-crate-root-type)
- [Waiting on a channel or a pane](#waiting-on-a-channel-or-a-pane)
- [Why pane waits poll the scrollback](#why-pane-waits-poll-the-scrollback)

## Five option-refusal messages map to three error kinds

tmux exits 1 for every way an option can be refused, so the classification
reads stderr. It carries one of five strings, and they reduce to three
`OptionErrorKind`s: `invalid option` and `unknown option` both mean no option
goes by that name (`Unknown`), `ambiguous option` means the name is a prefix of
several (`Ambiguous`), and `bad value` (a flag) and `value is invalid` (a
number) both mean the option will not hold the value (`BadValue`). Python
libtmux's `handle_option_error` matches the first three strings and raises one
of four exceptions.

Python raises a separate `UnknownOption` for `unknown option`. In the sources of
3.2a, 3.4, 3.5a, 3.6 and 3.7b that branch is unreachable: `cmd-set-option.c`
and `cmd-show-options.c` both call `options_match` first, which either fails
with `invalid option`/`ambiguous option` or returns the canonical table name.
Only then do they call `options_scope_from_name`, whose own table walk is where
`unknown option` lives, and by then the name has already matched the table.
The crate still matches `unknown option` and maps it to `Unknown`, because the
two strings mean the same thing and tmux is free to reorder the calls.

`real_tmux_compat_error_option_refusal_wording_is_recognized` pins all of it
against whichever tmux the lane runs.

## tmux merges the server and session environments only when it starts a process

tmux keeps the server environment and each session's environment as separate
stores for the session's whole life. It does not copy the server's into a new
session, and does not layer one over the other when either is read. It merges
them only when it starts a process.

From outside, that looks like this:

- `show-environment -t <session> NAME` reports `unknown variable` for a name
  set with `set-environment -g`, whether the session was created before or
  after the global entry existed. A session read does not fall back to the
  server's store.
- A pane started in that session is nonetheless handed the value.
- Where both stores hold a name, the process gets the session's.
- A name marked with `-r` is absent from the process's environment, not empty.
  `EnvironmentEntry::Removed` is its own state for that reason: a name missing
  from the store is `None`, and a name the store marks removed is `Removed`,
  which the merge turns into absence.

The two accessors report what each store holds, and neither predicts what a
pane will be handed.
`a_started_process_gets_the_server_and_session_environments_merged` pins the
merge by reading the variables back out of a running process, the only place
the merge can be seen.

`Server` and `Session` share `internal::environment`, parameterised by a
`Scope` that is either `-g` or `-t <target>`, so both parse the listing the
same way. A value containing a newline occupies more than one line of
`show-environment`, and a continuation line holding an `=` cannot be told from
the next variable. `show-environment -s` prints each entry
as a shell statement with every `"`, `\`, `$` and backtick in the value
escaped, so the first unescaped `"` ends a value on every supported release and
the whole environment is one command. Only that escaping is undone: tmux 3.4
and later also escape bytes for display, `$` among them, in both listings
alike, so a value read whole matches the same value read by name.

## tmux 3.3 through 3.4 send `run-shell` output to a pane

`Server::run_shell` reads the command's output from the client's stdout,
which tmux writes with `cmdq_print` when it has no pane to write into. Three
releases do not: 3.3, 3.3a and 3.4 replaced that branch in
`cmd_run_shell_print` with one that finds a pane and appends to its copy-mode
buffer instead. The command still runs and tmux still exits zero, so the caller
is handed an empty listing for a command that printed.

That empty listing cannot be told from a command that printed nothing, so on
those releases the crate refuses with `Error::CapabilityDefective`. `require`
cannot express this: it asks for a minimum release, and a minimum would refuse
3.2a, which works. The error names the defective range instead.

The range was read from the release tarballs of 3.2a, 3.3, 3.3a, 3.4, 3.5,
3.5a, 3.6 and 3.7b, and confirmed by building 3.2a, 3.4 and 3.7b and running
`run-shell` against each: 3.4 returns an empty stdout with status zero where
the others return the output.

The compatibility lanes found this. The tmux the local gate runs is outside the
range.

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

Pausing also takes the pane out of the stream, and `control_pause_pane`
discards the pane's queued blocks as it pauses, on every supported release, so
no stale offset is left behind. It costs back-pressure, because tmux keeps
draining a paused pane's terminal where a pane that is off lets the write block.
Below 3.7 the crate gives up that back-pressure to keep the server up.

tmux 3.7 added the same discard to `control_set_pane_off`. The range was
measured: a fixture that floods three panes, mutes them mid-write and then
checks the daemon is still running kills 3.2a, 3.4, 3.5a and 3.6b on every run
and leaves 3.7b up. That fixture is
`real_tmux_compat_muting_a_producing_pane_leaves_the_server_up`. It needs output
queued at the moment of muting. Muting an idle pane never reaches the defect,
which is why the flood test beside it passed on every release.

The defect first showed up as an intermittent failure in a `tmux-mcp` test that
asserted a tool accepted the arguments its schema describes. tmux reports a
dead server the same way it reports a refused command, so the assertion blamed
the arguments. `TestServer::daemon_state` was added for this: a test cannot
tell the two apart from the reply, but the fixture is the daemon's parent and
can.

## A one-binding key listing goes to the message log on tmux 3.7 through 3.7c

`list-keys` gained `-F` in 3.7, and in the same release its print loop reads
`if ((single && tc != NULL) || n == 1) status_message_set(...)`: a listing of
exactly one binding becomes a status message instead of a line of output. With
no attached client the message goes to the server's message log. The command
still exits zero, so `list-keys -T <table>` on a table holding one binding
answers with nothing, in the `bind-key` form and the `-F` form alike.

`Server::typed_key_bindings` therefore never passes `-T`. It lists every table
and narrows the rows itself, and a listing of every table holds one binding only
on a server with a single binding left. `Server::key_bindings` still sends
`-T`, because it returns tmux's own lines, and its documentation names the
defect.

The condition is in the source of 3.7 and 3.7c and gone from 3.8-rc. Measured,
3.7c prints nothing for a one-binding table and 3.7d prints it.
`real_tmux_compat_key_bindings_read_as_fields` binds two one-binding tables,
and fails on 3.7c when `-T` is sent.

## tmux cannot withdraw a queued `wait-for` client

tmux queues a `wait-for` client on the channel and offers nothing to withdraw
it. `cmd_wait_for_signal` releases every waiter it finds and keeps the signal
only when it finds none; `cmd_wait_for_unlock` grants the lock to the first
client queued for it. `server_client_lost` frees every other structure a lost
client owns (its files, its overlay, its prompt, `input_cancel_requests`) and
never touches `wait_channels`. The client leaves the `clients` list, so
`cmdq_next` never runs its queue again, and the queued item still holds the
reference `cmdq_append` took, so nothing is freed and nothing dangles: one
client struct and one queue item leak, and the channel's list keeps an entry
that can never act. The code is the same from 3.2a through 3.8-rc, and 3.8-rc's
new `wait-for -l` shows the leak: it lists the entry by name after the client
is gone.

A client killed for running out of time therefore takes the channel's next
signal, or its next lock, with it. `internal::wait_for` never kills one. Its
dispatch has no deadline of its own (`CommandRequest::without_deadline`, the
only request without one), and the caller's deadline ends the wait while the
client stays. A lock granted after its caller gave up unlocks at once. A wait's
client stays parked on the channel, at most one per channel per handle, so a
second wait joins it instead of adding a second waiter. When the signal that
releases a parked client arrives with no caller left, `ChannelWaits` keeps it,
as tmux keeps a signal nobody is waiting on; tmux cannot tell that a parked
client has no caller.

That leaves two gaps. Another process that waits on the channel afterwards does
not see a signal kept this way: tmux spent it on the parked client, and the only
way to put it back is `wait-for -S`, which would release somebody else's wait.
And `Server::shutdown` kills parked clients instead of waiting on tmux, which
leaves the tmux leak behind on any channel that still had one parked.

A blocking `wait-for` over control mode is refused for a different reason.
`cmdq_fire_command` writes the block's `end` guard as soon as `entry->exec`
returns, and `cmd_wait_for_wait` returns `CMD_RETURN_WAIT` with its item still
on the queue, so a control client is told the command finished and then runs
nothing else until the channel releases it. Measured through the crate on
3.7d, a routed `wait_for_channel` on a channel nobody signalled returned
`Signalled` in 190us, and the next routed call answered nothing within two
seconds. `wait_for_channel` and `lock_channel` therefore refuse a routed handle
with `ControlModeErrorKind::BlockingCommand`; `signal_channel` and
`unlock_channel` do not block and still route.

## Poll loops and deadlines that made tests flaky under load

Each of these passed locally for a long time and failed in CI, which has fewer
cores than a developer machine and runs the whole workspace at once. Raising a
timeout fixed none of them.

**Async poll loops sleep a millisecond.** The subprocess tests wait on a
separate process: a child writing its PIDs to a file, or a process to become
reapable. Written with `tokio::task::yield_now`, the waiting task never gives
up its worker thread, so on a two-worker runtime it competes for a core with
the process it is waiting for, and misses its deadline because of that. They
sleep a millisecond instead, which still looks far more often than anything
being waited on can change.

**Two deadlines of the same magnitude race.**
`never_observe_fallback_cleans_an_exited_leaders_group` set a 50ms lifecycle
timeout alongside a 50ms observer interval, and asserted the error was
`DaemonExited`. Under load the timeout fired first and the error was
`StartupTimedOut`, from a different path than the one under test. The timeout
now sits far above the interval. That costs the test no time, because the
observer notices a daemon that exits when it exits, not when the timeout
expires.

**A deadline under test must outlast setup.** Two subprocess tests gave the
executor a 100ms deadline and then read back a PID the child publishes on
startup. When the deadline fires first, the child is killed before it writes,
and the read waits out its own five seconds for a file nobody will write, so
the failure names the read instead of the deadline. Re-executing the test
binary takes longer on a CI runner than on a developer machine, which is why
only CI saw it. Shrinking the deadline to 1ms reproduces the failure every
time, which confirmed the mechanism.

**Blocking poll loops sleep too.** The fixture's shutdown polls for a daemon to
exit from inside `spawn_blocking`, and it did so with `std::thread::yield_now`.
That spin holds a core the daemon needs to handle the `SIGTERM` it was just
sent, so on a machine with fewer cores than the suite has concurrent fixtures
the grace window expires and cleanup reports that the daemon did not exit. It
never failed on a twenty-core developer machine and failed six tests on a
macOS runner.

In each case a test's deadline has to bound the part it is not testing, with
enough margin that the deadline never decides the result.

The fixture deadlines themselves stayed constants. Five seconds bounds a tmux
that starts with a core to spare. On a machine running several times its cores
in work it bounds nothing, and the fixture suite fails in a set that changes
between runs while every member passes alone. A defect fails the same tests
every time and load fails a different set each run, so telling them apart takes
several runs: a run that fails four tests followed by one that fails four
different ones points at load.

`LIBTMUX_TEST_TIMEOUT_SCALE` multiplies every fixture deadline. It is read
once, so two tests in a run cannot measure against different clocks, and never
goes below `1`, because nothing here wants a fixture to fail sooner. Unset, the
deadlines are unchanged. The scale only moves a ceiling. A test that
synchronises by sleeping still races, and the fix for that is to wait on the
event itself.

## How a fixture process idles without burning a core

The process fixtures used to hold a process open with `while :; do :; done`,
at nineteen sites, each burning a core for the length of its test. The suite
runs them beside tests that measure how long a child takes to start. A
twenty-core developer machine absorbed that; on a four-core runner it made
those measurements miss.

Two behaviours relied on the spin, so no single replacement fits every fixture:

- **A shell only runs traps between commands.** One blocked in `sleep` defers
  a TERM until the sleep ends, so the fixtures that assert on signal delivery
  broke when the spin became `while :; do sleep 30; done`. `sleep 86400 & wait`
  keeps the prompt handling, because a signal with a trap interrupts `wait`.
- **`exec` discards the shell's traps.** `exec sleep 86400` is a single process
  and burns nothing, but it replaces the shell, so any fixture that installed a
  trap first lost it.

Helpers that hold a trap use `sleep 86400 & wait`. Helpers that only have to
outlive an assertion use `exec sleep 86400`, which matters where the helper
carries an environment marker: a `sleep` child inherits it, and a scan that
counts marker-bearing processes then finds two where the test means one.

Measured by pinning the lib suite to two cores at eight test threads: one
failure in six runs before, twelve clean runs after.

## `Client` reads its attached session, window and pane by id

The format catalog gives `client_session` and `client_last_session` the
semantic owner `ClientAttachment` rather than `Client`, and leaves them
catalog-only, so the client snapshot has no session field. The classification
was recorded before its reason, and the parity ledger stalled on whether an
attachment is part of a client's identity. That question has no useful answer:
identity here is `(ServerIdentity, client_name)`, and every other field in the
snapshot is mutable state too.

The reason is narrower. `format_cb_client_session` returns
`c->session->name`, so `client_session` is a name, and tmux cannot always
address a session by its name: it will create a session called `a:b` and then
refuse to address it, because `:` separates a session from a window in a
target. Projecting the field would put a value in the snapshot that a caller
cannot reliably turn back into a `Session`.

A client's format tree resolves the whole chain as ids, which is what the
accessors use:

```console
$ tmux list-clients -F '#{client_session} #{session_id} #{window_id} #{pane_id}'
plain $0 @0 %0
```

`Client::attached_session`, `attached_window` and `attached_pane` each read one
id and hand it to the by-id lookup. `client_session` stays catalog-only, and
there is no `ClientAttachment` type: the catalog's owner records format
semantics and needs no public struct.

The window and pane accessors report the session's current window and pane,
which every client attached to that session shares. `curw` is a member of
`struct session`, not `struct client`, so one client changing the window
changes it for all of them. The pane follows from the window, because tmux
keeps no per-client focus.

## A session named with `:` or `.` needs a trailing `:` to be targeted

`Server::session` and `Server::has_session` find a session named `a:b` or
`a.b` on any release that keeps such a name at all, because they compare
`list-sessions` output in process instead of asking tmux to resolve a `-t`
target for that name. Building a `-t` target from the bare name misreads it on
every release: `cmd-find.c` splits the target on the first `:`, then on the
first `.` in whatever follows the colon, before it ever looks for a leading
`=`. `-t my.proj` reads as window `my`, pane `proj` in the current session.
`-t =my.proj` fares no better, because `=` only marks whichever piece the split
left it attached to as exact, and that piece is not the whole name.

Appending a trailing `:` sidesteps the split: `-t my.proj:` and
`-t =my.proj:` both resolve session `my.proj`, because the colon consumes
the split point and leaves nothing after it for `.` to divide. That works on
tmux 3.7a and later, which is also the range that keeps such a name instead of
rewriting or refusing it. Measured against 3.7a, 3.7c and master with
`display-message -t <target> -p '#{session_name}'`.

The name is still unsafe to hand out. Few people or tools write `name:`, an
ordinary `-t name` from either still misreads it, and a tmux older than 3.7a
never kept the name in the first place. `tmux-workspace` refuses `:` and `.` in
a `session_name` up front for that reason: `load` could not reliably run a
workspace again if most tooling cannot address its session. Code in this crate
that must build a `-t` target from something other than an ID resolves the
session first and targets it by `SessionId`, which parses with no split at all.

## `list-clients` omits suspended, locked and exiting clients

A client that is suspended, locked, dying or already gone is absent from
`list-clients` in every case. `sort.c`'s `sort_get_clients` skips any client
carrying `CLIENT_UNATTACHEDFLAGS`, and `tmux.h` defines that as
`CLIENT_DEAD|CLIENT_SUSPENDED|CLIENT_EXIT`. The listing answers "not attached
right now", and the crate used to read it as "gone".

A caller acts on the difference. `Error::is_object_gone` decides whether to
discard a handle, and a suspended client is listed again the moment its process
continues: on `SIGCONT` for a suspended one, and when the `lock-command` exits
for a locked one. Locking is the more common route. It sets the same flag
through `server_lock_client`, so `Client::lock`, `Session::lock` and
`Server::lock_all` all reach it, and so does `lock-after-time`, with nobody
asking.

tmux publishes the difference outside the listing. `server_client_get_flags`
puts `suspended` in `#{client_flags}`, and `display-message` carries
`CMD_CLIENT_CANFAIL`, so a client it cannot resolve expands every format empty
and exits zero instead of erroring, while a client that is only stopped still
resolves and names itself. `Client::refresh` asks `display-message` only when
the listing misses. A reply that names the client and carries that flag is
`Error::ClientSuspended`; every other reply, including a probe that fails
outright, stays `Error::ObjectGone`. The probe can mark a suspended client as
suspended, and can never report a gone client as live.

Both mechanisms date to 3.2a, which is `MIN_SUPPORTED`, so this needs no
version gate. The listing filter differs by release: 3.2a and 3.5a screen
`list-clients` on `c->session == NULL` alone, and `server_client_suspend` never
clears the session, so a suspended client stays listed there and the miss path
never runs. That was read from their sources, not measured. Because releases
disagree about whether a suspended client is listed, the crate keys on the flag
instead of on the absence.

## `display-message` falls back to the current pane when `-t` does not resolve

`display-message` looks like a way to ask tmux what a target resolves to, but
its entry declares two separate permissions to fail, and only one of them is
the `CMD_CLIENT_CANFAIL` described above:

```text
.target = { 't', CMD_FIND_PANE, CMD_FIND_CANFAIL },
.flags  = ...|CMD_CLIENT_CANFAIL,
```

`CMD_CLIENT_CANFAIL` governs `-c`: a client that does not resolve expands every
format empty, which the suspended-client probe relies on. `CMD_FIND_CANFAIL`
governs `-t`. An unresolvable `-t` leaves the target unresolved, so the formats
expand against the client's current pane and the command still exits zero:

```text
current window: @2
-t home:@99    -> @2
-t home:9      -> @2
-t home:nosuch -> @2
-t home:%99    -> @2    a pane id in a window target, still @2
```

A target that resolved and a target that fell back to the current pane give
the same reply. A test that asks `display-message` whether a rendering reaches
the right window passes whenever the right window is also the current
one, and a fixture that has just built the window makes it current. The first
version of `a_rendered_window_target_survives_a_renumber` shipped with that
probe, which could not fail.

A command whose target lacks `CMD_FIND_CANFAIL` refuses an unresolvable target
instead, which is what a probe needs. `select-window` is a cheap one, and it
leaves the current window alone when it fails. Measured on tmux 3.7c.

## A socket path does not identify a tmux server

`ServerIdentity` is a normalized socket path, and object equality includes it,
so `%0` on two different sockets are two different panes. It cannot separate
servers in time: the same socket can host more than one server in succession,
and the crate could not tell them apart.

Three tmux behaviours make that hazardous:

- the socket file outlives the daemon: it is still on disk after
  `kill-server`, and a replacement binds the same path;
- a replacement reissues ids from the start, so its first pane is `%0` too;
- neither the path nor the id carries any mark of which daemon it belongs to.

A handle held across a restart still resolves, to a different object
that now has the same id. A stale read is harmless; a stale `kill-pane` or
`send-keys` lands on the new object.

`ServerGeneration` is `(pid, start_time)`, read with one `display-message`. The
pid alone is not enough, because a replacement daemon can be handed the pid of
the one it replaced. Both fields are server-scoped (`start_time` is identical
across every session of one daemon, unlike `session_created`), and both have
been in the format catalog since 3.2a with `ListScope::All`, so a later change
can project them into every listing row and give each snapshot its generation
at no extra round trip.

The caller checks the generation explicitly. Verifying on every dispatch would
double the command count for a hazard that exists only when a caller holds a
handle across a restart, so a caller wraps `require_generation` around work
that must not be misapplied.

The socket's inode would be a cheaper token, and measurement ruled it out: the
inode is unchanged across a restart, because tmux reuses the file instead of
recreating it.

## Limits on tmux output and concurrent dispatches

Two resources in the caller's process had no ceiling.

**Output.** Each dispatch drained stdout and stderr with `read_to_end`. A pane
with a long history, a buffer someone pasted a file into, or a `run-shell` that
keeps printing all answer with as many bytes as they have, so the operating
system decided when to stop, by killing the process. `OutputLimits` bounds the
read where the allocation happens, by taking `limit + 1` bytes and failing if
the extra one arrives.

A read over the limit fails instead of truncating. A truncated tmux listing
decodes cleanly and reports fewer panes than exist, and nothing downstream can
tell. A caller who wants less asks tmux for less.

The default is 32 MiB of stdout and 1 MiB of stderr, high enough that only
runaway output reaches it, and the error names the limit when it does. A budget
smaller than one listing row breaks every command: the crate's own snapshot
projection is a few hundred bytes, and a 64-byte budget was enough to fail
`new-session` during testing.

**Dispatches.** Nothing bounded how many tmux clients ran at once. A caller
that fans out, such as an agent driving the MCP server or a reconciler sweeping
every pane, turned its own concurrency into process, descriptor and memory
pressure. tmux serializes commands regardless, so the extra clients only
queued. `DispatchLimits` is a semaphore acquired before the request is
registered, so a refusal costs nothing. The command deadline starts before that
wait; an explicit admission timeout may shorten it but cannot extend it.

`Error::Overloaded` is separate from `Error::Timeout` because overload means the
work never started, so retrying is safe, while a timeout means tmux may have run
the command already.

Tests measure both limits. The admission test times twelve dispatches through
two permits and fails if they finish in less time than the rounds require. With
the limit raised to 64 they finish in 138ms, and the test fails.

## Control-mode limits and reply deadlines

A subprocess dispatch ends when its process does, which bounds it. A
control-mode connection reads a framed text protocol from a tmux that keeps
running, so only the framing stops a malformed or unexpectedly verbose answer
from growing without bound. Two shapes grow, each in its own way:

- a line that never ends, accumulated across reads because a cancelled
  `read_until` leaves its bytes behind for the next one;
- a `%begin` block whose `%end` never arrives, which grows one valid line at a
  time and so cannot be caught by a line budget.

`ControlLimits` bounds both. Neither is recoverable in place: the parser is
mid-frame and does not know where the next one starts, so the connection ends
and a caller who wants to continue attaches again.

Pending requests receive the frame reason. The first version reported
`ControlMode { kind: Closed }` to everything still waiting, which was true but
gave the caller nothing to act on: a caller who exceeded a budget can raise it,
where one who lost the connection can only reconnect.

The budgets are 8 MiB for a line and 64 MiB for a block, large enough that
ordinary output never reaches them.

Connections have a separate count. `ControlClientLimits` bounds the persistent
clients owned by one server, independently of `DispatchLimits`. Combining the
two would let a handful of long-lived watchers starve every short command.
Admission lasts until the control process is cleaned up, and a full lane
returns `Error::Overloaded` before another process starts.

Deadlines are split the same way. Before `ControlSender::reply_timeout`, the
server's `default_timeout` bounded both the opening handshake and every
command's reply, because attaching seeds it into the actor and the sender
together. The handshake forks tmux and waits for a server to come up; a command
is a round trip on a connection that is already open. A caller who wanted a
command to give up in 100 ms was also telling `attach` to fork a process in
100 ms, which a loaded machine cannot do, and three of this crate's own tests
were flaky for that reason.

The reply deadline sits on the sender, not in `ControlLimits` (which
`attach_with_limits` already threads through), because limits are fixed when
the connection opens and a sender's deadline is not. `ControlSender` is
`Clone`, and two clones of one connection can carry different deadlines; the
actor honours the earliest committed one, which
`the_earliest_committed_deadline_ends_the_connection` pins. A limit set once at
attach could not express that. The server keeps the connection budget and the
default every sender starts from.

## Which tmux releases the lanes build

The lanes build the final patch of each series: 3.2a, 3.5a, 3.6b and 3.7b,
which are what a distribution ships and a user runs. 3.4 is built too: that
series has no later patch, and 3.4 is one of the two releases that wrapped
command output in `VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`.

`3.6` still appears in the source as a behaviour boundary, because the dialect
restore landed in 3.6 itself. That says nothing about which build CI runs.

## The macOS lane and what it found

The platform contract names macOS, and for a long time only Linux was tested.
macOS differs in what this crate depends on most: process groups, Unix
sockets, `waitid` and temporary paths. The lane runs the test suite, not the
whole gate, and only on master, because a macOS runner bills at ten times a
Linux one and the lints it would re-run give the same answer on every platform.

Its first run failed nine tests, from three causes. Finding the third took
three rounds of instrumenting.

**`/var` is `/private/var`.** Three tests compared a resolved path against the
raw temporary one. The library canonicalizes paths so that two selectors for
one endpoint compare equal; the tests' expectations were what assumed Linux.

**A blocking poll loop yielded.** The fixture's shutdown waited for the daemon
with `std::thread::yield_now` from inside `spawn_blocking`, holding a core the
daemon needed to handle the signal it had just been sent. This was the async
poll-loop defect above, in a blocking loop. It was a real defect, though not
the cause of the remaining failures.

**`killpg` returns `EPERM` on macOS.** The forced sweep of the leader's own
process group fails with "Operation not permitted" once the leader has exited,
on every fixture shutdown, while the leader itself is killed and reaped
successfully in the same cleanup. The daemon is gone, and nothing more can be
done about a group the kernel will not let the caller signal, so the fixture
accepts that errno on other platforms. On Linux the same result would be a real
permission bug, so it still fails there.

Diagnosing these needed more than `ShutdownFailed`, which covered four
different problems. `TestServerError` now carries the step that produced it,
so a failure on a runner nobody can log in to names its own step.

## On macOS a pane accepts about a kilobyte of input at once

The MCP server runs a command by typing a completion frame into the user's
own shell, so the command inherits the environment, traps and directory that
shell has. The frame is 2.6 KB for `sh` and 4.7 KB for `bash`. On macOS every
run using it lost most of the frame and then waited for a marker that could
never arrive.

**The limit applies to a burst of input, however it is split into lines.**
Measured on the macOS lane across `sh`, `bash` and `zsh`: a total up to 1024
bytes arrives whole, and past it the input is truncated or corrupted outright.
Splitting the frame into 158-byte lines and sending one `send-keys` for each
fails identically, so this is not `MAX_CANON`, which caps one line at 1024
bytes there against 4096 on Linux. It is the pty input queue, which drops
whatever a burst adds beyond its depth while the shell is busy instead of
reading, and a shell echoing a multi-kilobyte command line through its line
editor is busy. Linux delivers 8 KB intact through the same path with the
reader stalled for three seconds, so this does not reproduce there.

The fix is to send fewer bytes. The frame is written to a private file and the
pane is told to read it, which keeps the typed line to a few hundred bytes
whatever the frame contains. The line no longer carries the command either, so
its length no longer grows with the command; a command over 4 KB used to
produce a line that Linux truncated too.

**Three shell behaviours, each found by a failing test, fix how the pane reads
the file.** Sourcing it with `.` gives it its own scope for trap inheritance, so
`trap -p ERR DEBUG` inside reports nothing, the capture restores nothing, and
the command runs without the traps its shell had; `eval` introduces no scope
and keeps them. Under an inherited `DEBUG` trap that writes to standard output,
zsh captures that output into `$( cat frame )` and hands `eval` the trap's text
ahead of the frame, which then fails to parse; `$(<file)` runs no command for a
trap to precede. And dash accepts `$(<file)` and yields nothing, which would
hang a run, so every other shell reads through `cat`. That is safe, because a
shell without `$(<...)` has no `DEBUG` trap to capture.

## Listing methods return `Result` under the short name

Each listing had two forms from the start, and the short name went to the one
that discarded errors: `sessions()` returned `Vec<Session>` and dropped the
error, while `try_sessions()` returned `Result`.

A Rust caller reaching for `sessions()` expects fallible I/O to return
`Result`, and instead got a value indistinguishable from a healthy server with
nothing running. A status line can live with that. Anything that reconciles,
such as a supervisor, a cleanup pass or a workspace builder, reads "no
sessions" from an outage as an instruction to kill everything.

The names swapped. `sessions()` returns `Result`, and a caller who wants the old
behaviour writes `sessions().await.unwrap_or_default()`, which shows the discard
at the call site. An `_or_empty` twin of each listing existed for a while;
neither consumer crate called one, and the eleven of them were eleven ways to
discard an error by accident, so they were removed. The swap broke callers,
which costs least during an alpha.

## Fuzz targets for the parsers that read external bytes

Every surface that takes bytes this workspace did not write is fuzzed:

- the control-mode line parser (`control_line`), which reads from a tmux that
  keeps running, so it has to survive any malformed line;
- control-mode block framing (`control_block`): a stream read inside and
  outside `%begin` blocks, each closed block handed to the slots that assemble
  a chain's reply, checked that no line escapes its block and no reply holds
  blocks another command owns;
- the format-row codec every listing decodes through (`format_rows`), whose
  names, paths and titles users and programs write, checked by writing values
  the way each dialect of tmux prints them and decoding them back;
- lifecycle ownership and creation receipts (`lifecycle_receipts`), checked
  that accepted identities and object-ID receipts survive reconstruction, with
  seeds captured from a disposable tmux daemon;
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

Random bytes almost never produce a line beginning with `%`, so an unseeded
control-mode target spends its whole budget establishing that arbitrary input
is text and never reaches `%begin`, `%output`, or the block-number parsing that
correlates a result with its command. `fuzz/seeds/` carries those shapes,
including a line that is not UTF-8, because pane output is not required to be.
Only the seeds are checked in, not what the fuzzer discovers from them.

The `__fuzz_*` functions let `fuzz/` reach the parsers while they stay private.
They are behind `unstable-fuzzing`, which is not in `full` and which nothing but
`fuzz/` turns on. tmux-mcp's filter needs only `std`, so the target compiles
its source file instead of adding a feature to a published binary crate.

CI runs them weekly instead of on every push, because fuzzing finds defects by
running for longer than a push gate can wait. Each target's corpus is cached
between runs, since the corpus is what grows, and a crash is uploaded as an
artifact instead of left in a log.

## `public-api.txt` records the public surface

Once the `semver` recipe was dropped, nothing recorded mechanically how the API
changed between releases. `cargo-semver-checks` could not: it skips every lint
on a prerelease-to-prerelease step and then reports success. Human review does
not reliably notice a method whose signature changed.

`crates/libtmux/docs/public-api.txt` records every public item with its
callable or data signature, plus each non-blanket trait implementation.
`scripts/public-api.py` generates one record per line from rustdoc's JSON.
`just api` regenerates it and `just api-check` fails when the tree and the
record disagree, naming what moved.

It reports that a change happened and leaves whether it is allowed to the
person reading the diff. During the alpha every change is allowed, so a semver
verdict would add nothing.

It is built from rustdoc's JSON because that needs only the nightly toolchain
the fuzz targets already require. Methods, fields and variants have no
standalone path in that JSON, so each is attributed to the type that owns it:
an unqualified `sessions` would not say which handle it belongs to, and a move
between types would not show at all.

Attribution first reached only one level, and a struct-like variant's fields
sit one level further down. Nothing mapped them to their variant, so
`Error::LinkGone` recorded its fields as `kind` and `index`, bare names that
seven other variants of the same enum also use. The record carried 42 such
lines. Removing both of `LinkGone`'s fields and adding one produced a diff of a
single inserted line, because other variants still had fields named `kind` and
`index`, so the gate missed the kind of change it exists to report.
Variant fields are now attributed like everything else, which named 121 of them
and left no bare field records.

## `tmux-mcp` limits its own dispatches and output

`tmux-mcp` already capped what it returns, at 256 KiB of captured output and
eight concurrent tails. Those caps bound only the response. An agent that fans
out still started as many tmux client processes as it had questions, and
truncating a response afterwards does not free the memory the core already
allocated to read it.

The binary configures the `Server` it builds with a dispatch limit of four and
an output budget, which bound those costs where they are incurred. The limit is
four because tmux serializes commands on its own thread: past that, more
clients only queue, and an agent should wait in a bounded queue instead of
spawning a process per request.

The limits are on by default because an agent is the caller most likely to ask
for too much at once and least likely to notice that it did.

## Example coverage is counted per crate-root type

"Every public item has a runnable example" was a goal nobody counted. The first
count found 15 of the 67 types a caller reaches through `use libtmux::X` had no
example of their own, including `Server`, `Session`, `Window`, `Pane` and
`Error`, the pages a search lands on first.

Well-documented methods still leave a reader of the type's page with nothing to
copy, so the measure is per type: `Pane::id` inherits the example on `Pane`,
and counting accessors separately would bury the missing types under items
nobody needs an example for.

`just example-coverage` reports the count, and `example-coverage-check` fails
when a crate-root type has no example. The current count comes from that
command; this page does not track it.

The check first counted any example rustdoc would compile, and eleven of those
wrapped their body in a hidden function nobody called, so they never ran.
`just doctests-run` now runs them, so a counted example is one that runs.

Three doctests failed on their first run, each on a wrong belief this crate
held about tmux: `split` is detached by default, so focus does not follow the
new pane; a new session does not copy the server environment; and `status` is
not a flag, because tmux accepts `on`, `off`, and `2` through `5` for it. The
`status` case is why the option schema should come from tmux's own table
instead of a type inferred from the value, and its example says so.

## Waiting on a channel or a pane

`Server::wait_for_channel` is the blocking half of `wait-for`, which
`signal_channel` long lacked. Its documentation deferred it until a wait running
out of time could be told from tmux failing to reply, which `ChannelWait` now
carries. tmux latches a signal nobody is waiting on: one signal releases every
waiter present, and the latch then releases one later wait, so signalling
before the wait starts is safe, measured on 3.7c. That removed the
`Server::cmd(wait-for)` workaround from `tmux-mcp`.

`wait-for` is a rendezvous between commands. Something has to signal it, so it
serves "tell me when this is done" only for work written to announce itself.
Watching a pane whose work does not announce itself needs a different
mechanism.

`Pane::wait_for_text` and `Pane::wait_for_quiet` watch a pane by polling, and
need no feature, because a caller who dispatches a command needs to know when
it finished. Each look reads the scrollback with wrapped lines joined, so text
that scrolled off before the look still counts, and a needle that spans a wrap
still matches. A dead pane ends the wait instead of holding it to the deadline,
and running out of time is `PaneWait::TimedOut`, not an error.

`Pane::wait_until` runs the same loop with a predicate over the captured lines,
for what a literal needle cannot express. It also polls on a handle from
`Server::over_control_mode`, where `control-mode` is on and the feature cost
does not apply. There the reason is ownership. A connection's `%output` goes to
whoever holds its events, and the handle holds only the sender, so waking on
output would attach a second client for every wait, a cost the `streamed` bench
lane leaves out of its number. Over a connection a look is a line instead of a
process, so the round trip a doorbell would save is smaller too.

## Why pane waits poll the scrollback

Before these waits, `libtmux::test::retry_until` was the only waiting primitive
this crate exposed, and `test` sits behind `test-support`, which the manifest
says belongs "to a dev-dependency, not to a build of the library". A caller who
needed to wait for anything a pane did wrote that loop themselves. The port
inherited the gap: the Python library keeps `retry_until` in
`libtmux/test/retry.py` for the same reason, and of the seven ports only the
Swift one shipped a pane wait a production caller could reach.

`tmux-mcp` filled the gap downstream by rebuilding run-and-report in `exec.rs`:
sentinels bracketing the command, a scanner reassembling output around them,
and separate waits for text and for quiet. AGENTS.md treats a workaround there
as a finding about this crate, and this was the largest.

Thirty-five lines against the public API run a command and report its exit
status correctly, and then `seq 1 100` returns status 0 with no output: the
opening sentinel scrolled off the visible screen before the closing one
arrived, so the body came back empty while the status still parsed. A
three-hundred-character line arrives as four, wrapped at the pane's width.
Both failures report success, the failure a consumer pays for most.

Those two failures set two requirements before any design. A wait must not
report success while losing output, and it must survive a line wider than the
pane. Together they force scrollback capture, `OutputLimits`, and
width-independent reassembly instead of a screen read.

Precedent settled a third. A wait that runs out of time is an outcome, not an
error: `RetryTimeout` already treats it that way, and a caller who cannot
separate "it never happened" from "the connection broke" has to guess which of
them is worth retrying.

The Swift port does not choose between streaming and polling. It subscribes to
`%output` as a doorbell and captures for the content, because a notification
carries escape sequences and can split a word across two of them. Around that
sit a primed first capture, so output produced while the connection opens is
not lost; a `#{pane_dead}` subscription, so a dead pane ends the wait instead
of holding it to the deadline; and coalescing, because an unbatched burst is
one notification per character.

tmux supports that design on every release the lanes build. `%output` and
`%subscription-changed` both arrive on 3.2a, 3.4, 3.5a, 3.6b, 3.7 and 3.7c,
with no errors anywhere, and the `#{pane_dead}` half needs `refresh-client -B`,
which landed in 3.2.

The feature cost rules a doorbell out as the only path. A doorbell needs
`control-mode`; a capture poll needs only the base API, and
`default = ["query"]`. A doorbell-only wait would be absent from a default
build, decided by a flag its signature never mentions. This manifest reserves a
feature for "API surface a caller who only dispatches commands never needs",
and a caller who dispatches a command does need to know when it finished.
Waiting cannot be opt-in, which makes the polling path the floor and a doorbell
an optimisation above it.

`benches/waits.rs` measures what a doorbell would buy, against the same pane on
both paths:

```console
$ cargo bench --features test-support,control-mode --bench waits
```

Latency is one capture round trip, not a fraction of the poll interval, because
the loop looks before it sleeps. A marker printed into a pane answers in 5.7ms
polled and 3.0ms streamed, measured from dispatching the key that produces the
text. A doorbell removes the round trip, which is under three milliseconds here:
real, and too small for a caller to notice.

A flood shows the poll interval. With `seq 1 20000` in a pane and a wait for a
marker printed after its last line, polling takes 132ms and the stream 21ms.
Polling costs one capture per `POLL_INTERVAL` whatever the pane is doing, so a
flood does not slow it, but the interval is 120ms and anything finishing inside
one is rounded up to it.

Ten times the flood closes the gap. At 200,000 lines polling holds [151, 184]ms
and the stream [118, 260]ms: medians within 4% of each other, with polling's
spread 33ms wide against the stream's 142ms. The stream wakes per notification,
where polling looks once per interval however much arrived in between, so
polling needs none of the coalescing the Swift port adds for that four-fold
spread.

The doorbell stays unbuilt, and the feature cost decides that more than the
clock does. The clock says a doorbell would help: under three milliseconds on a
marker, six times faster on a moderate flood, and nothing once the flood is
large enough. A default build still could not use it.
