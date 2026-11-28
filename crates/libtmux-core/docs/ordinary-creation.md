# Ordinary creation receipts

An ordinary `new-session -d -P -F …` can report creation without attaching a
control client. Its first complete output record is accepted only when it
carries the fresh 128-bit nonce supplied for this dispatch. The same nonce
appearing again anywhere in collected stdout makes the reply unconfirmed.

## Where the receipt comes from

In upstream [3.3a][new-33], [3.4][new-34] and [3.7][new-37], the built-in
`new-session` command prints its `-P` projection synchronously before inserting
`after-new-session` hooks into the command queue. A later hook can print
arbitrary bytes or fail after the receipt has already been delivered.

Cold configuration commands run in the global queue while the initial client
waits for configuration to finish; see [3.4 `cfg.c`][cfg]. A global command
does not print to the initial client's stdout: [`server_client_print`][print]
discards output when there is no client. Configuration diagnostics go through
`cfg_show_causes`, which uses control notifications or a pane's view mode.

This ordering distinguishes the first action's projection from ordinary
follow-up output. The nonce binds that projection to the request. The decoder
requires the complete first line, both fixed boundary markers and exactly one
occurrence of its nonce; it never searches later lines for plausible IDs.

## Counterexamples and limits

A tmux server can redefine the canonical `new-session` spelling through
`command-alias`. An alias which prints a valid-looking ID row before the
built-in command defeats a decoder which accepts the first ID-shaped row.
The nonce decoder rejects that preface, even if a valid stamped receipt follows.

An `after-new-session` hook can expand `#{E:hook_flag_F}` and print the exact
stamped projection again. The duplicate nonce makes that output ambiguous;
neither copy is selected. An incomplete first line is also unconfirmed.

The marker does not authenticate a daemon, client executable or configuration
which reads the request and fabricates its marker. Those already control the
operation being requested. The native drivers require the trusted built-in
route described by the runtime architecture; an extra configuration read
cannot authenticate a malicious daemon.

The nonce must come from fresh OS entropy for every dispatch. A reused token
allows independently produced output to imitate an old projection. The core
accepts entropy from the driver and performs no I/O itself.

## Failure evidence

A complete, unique first record acknowledges creation. A nonzero or absent
process status retains any decoded receipt in `CreationFollowup`; a reply
observed after its deadline retains it in `LateCreation`. Receipt payloads
which cannot be decoded do not enter diagnostics.

Interrupted collection is stricter: only a complete typed receipt upgrades
the original failure to `CreationInterrupted`. Missing, malformed, prefaced or
duplicate-stamped output preserves the original cause and its uncertainty.
Cleanup errors wrap that cause without discarding its creation evidence.

## Reproduction evidence

The hand checks used `/usr/bin/tmux` 3.4 on Linux and private sockets under
`/tmp/libtmux-rs-dev/ordinary-*/`. Every server was killed and its socket
directory removed after the check. The print projection was:

```text
libtmux-create:<fresh-32-hex-digits> #{pid} #{start_time} #{pane_id} #{session_id} #{window_id} :end
```

The following server settings produced these results:

| Setting or action | Observed stdout and status |
| --- | --- |
| Ordinary detached creation | One stamped receipt, status 0 |
| `after-new-session` prints a literal fake ID row | Stamped receipt, then fake row, status 0 |
| `after-new-session` selects a missing window | Stamped receipt, status 1; error on stderr |
| `after-new-session` prints `#{E:hook_flag_F}` | Two identical stamped receipts, status 0 |
| Alias prints a fake ID row before `new-session` | Fake row, then stamped receipt, status 0 |
| Cold configuration prints or creates its own session | Only the requested action's stamped receipt reaches stdout |
| Client terminated after the receipt while a hook waited | The complete receipt was collected and the session remained present |

The focused pure regression tests cover those byte sequences, malformed
markers and receipt payloads, interruption and cleanup evidence. A deliberate
mutation removing duplicate-nonce rejection made
`duplicate_nonce_is_ambiguous_even_after_a_well_formed_receipt` fail by
returning a receipt instead of an unconfirmed effect.

[new-33]: https://github.com/tmux/tmux/blob/0b355ae8114511e1ff6359272b164f1cdf718e80/cmd-new-session.c#L343-L358
[new-34]: https://github.com/tmux/tmux/blob/9ae69c3795ab5ef6b4d760f6398cd9281151f632/cmd-new-session.c#L336-L351
[new-37]: https://github.com/tmux/tmux/blob/81f88f8517c9fc5371b56cf117530c6b477c96ac/cmd-new-session.c#L347-L362
[cfg]: https://github.com/tmux/tmux/blob/9ae69c3795ab5ef6b4d760f6398cd9281151f632/cfg.c#L64-L94
[print]: https://github.com/tmux/tmux/blob/9ae69c3795ab5ef6b4d760f6398cd9281151f632/server-client.c#L3274-L3305
