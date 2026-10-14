# Ownership, discovery and find-or-create

Use borrowed `Server`, `Session`, `Window` and `Pane` handles to inspect or
change existing tmux objects. Dropping them, or awaiting `Server::shutdown`,
closes client work and leaves remote objects alive. Accept destruction
responsibility with `adopt`, or create it with `owned_session`, `owned_window`
and `owned_pane`.

## Owned scopes

`lifecycle::Owned<T>::scope` runs a body and awaits remote cleanup. It retains
the accepted endpoint, daemon PID/start time, random ownership token and object ID. A rename, a moved
pane, a moved window or a host-environment change cannot retarget cleanup.
The cleanup command compares daemon identity inside tmux's command queue;
a stale owner refuses to kill a replacement daemon at the same path, including
a replacement with equal PID and start-time fields.

Creation and explicit adoption initialize the reserved server option
`@libtmux_owner_generation` only when it is absent. Its value is 16 random
bytes encoded as exactly 32 ASCII hexadecimal characters. A valid existing
value is reused unchanged; an empty or malformed value returns
`OwnershipMetadata`. Applications must preserve this reserved metadata for
the daemon lifetime, including avoiding the same key at narrower option
scopes. Removing or changing it makes old owners refuse cleanup
with `OwnershipTokenChanged`, even when the public PID/start-time generation
still matches. The token prevents accidental identity reuse; it does not
authenticate a daemon against a client deliberately copying its metadata.

A window owner kills the window, its links and its panes. `Window::unlink`
removes one link and has a different effect. Session and server owners also
kill their descendants.

`Owned::close` supports explicit cleanup. Clones share its completed state:
a successful close is harmless to repeat, while a failed close leaves
`is_closed()` false and retains the target for inspection and another attempt.
Dropping an owner does not perform asynchronous work. Use `scope` or await
`close`.

`ScopeError::Operation` retains the body's error. `Cleanup` retains a
successful body value and the cleanup error. `OperationAndCleanup` retains
both original errors. Cleanup after a created resource carries `AfterEffect`;
replaying the body could repeat its effects.

Creation and cleanup have supervised Tokio tasks. Cancelling an acquisition
before handoff releases a known resource to cleanup. Cancelling or panicking
inside a scope releases its accepted resource to cleanup. After joining the
cancelled task, await `Server::drain_cleanup()` to collect unobserved errors,
then shut down the client and runtime. Keep the runtime alive through this
drain. A runtime that has already been destroyed cannot run async cleanup.
Do not drain concurrently with a caller still awaiting its scope/acquisition:
the drain consumes the errors that caller would otherwise receive.

Creation replies carry an independent daemon-and-ID receipt before the
snapshot, followed by the token and identity from the same tmux command
connection. Adoption likewise initializes and accepts the token on one
connection. A later snapshot failure rolls back that ID against its creating
daemon. `AcquisitionRollback` preserves the original error and rollback
failure. An unreadable token after creation cannot authorize rollback.
`UnknownCreation` says the response did not establish a complete receipt or the
transport lost the result; inspect the accepted endpoint before retrying.
The library does not infer ownership from another client's matching name.

Run the ordinary example, which imports the public API and calls
`Server::new()`:

```console
$ cargo run --example lifecycle
```

[`lifecycle.rs`](../examples/lifecycle.rs) demonstrates window adoption,
created/reused panes, repeated close and bounded discovery. The outer session
scope cleans up after a body failure. Its child-environment harness executes
the same source with a private endpoint:

```console
$ python3 scripts/test-lifecycle-examples.py
```

[`adopt_resources.rs`](../examples/adopt_resources.rs) adopts an existing
session by exact name, then adopts an existing window and pane inside that
scope. The named session and its descendants are destroyed:

```console
$ cargo run --example adopt_resources -- disposable-session
```

[`adopt_server.rs`](../examples/adopt_server.rs) accepts an explicit disposable
socket and adopts the whole daemon. The harness observes its process exit
before removing the socket directory. Both examples return grouped operation
and teardown failures to their caller.

## Find-or-create

`lifecycle::FindOrCreate<T>` distinguishes `Created(Owned<T>)` from `Reused(T)`.
A reused handle stays borrowed until you call `adopt`. Inspect the variant
before deciding to close a resource.

| API | Match and concurrency boundary |
| --- | --- |
| `Server::find_or_create_server` | One captured endpoint. At most one daemon can answer there. |
| `Server::find_or_create_session` | Exact name bytes. Names containing tmux's `:` or `.` separators are rejected. tmux enforces session-name uniqueness; a duplicate-create refusal triggers exact relookup. |
| `Session::find_or_create_window` | Exact name bytes within the session. Duplicate names return `LifecycleAmbiguous`. Creation disables automatic rename and establishes the requested name. |
| `Window::find_or_create_pane` | Exact user-option key/value within the window. `PaneIdentity` validates the key and creation sets its value. Duplicate identities return `LifecycleAmbiguous`. |

Calls on clones of one `Server` share serialization. Independently constructed
clients, control-mode handles and external tmux clients do not share that
boundary. External writers can rename or move an object after lookup, or
create duplicate window names and pane identities. These APIs do not promise
a transaction spanning those clients. A creation or identity-assignment
failure returns its original error and any rollback error.

The final window name and automatic-rename setting, and a created pane's
identity value, are written under the original creation receipt's PID, start
time and token guard. tmux checks that guard on the connection executing the
writes. A replacement refuses finalisation and its rollback; the caller gets
both failures instead of `Created`. The returned owner retains the original
receipt and snapshot, without a follow-up lookup through the endpoint. Names
and option values remain literal through the nested tmux command parser.
As with any remote operation, the accepted daemon can exit after replying;
later cleanup still checks the captured authority.

Server creation uses a per-launch environment nonce before accepting daemon
ownership. A startup reply and nonce must name the same daemon before the
library changes `exit-empty` to keep it alive without sessions. A competing
daemon stays borrowed; cleanup removes only this call's bootstrap session.
If a startup result is lost, a matching startup nonce permits bounded recovery
of this call's daemon. Without that proof, `UnknownCreation` retains the
recovery boundary. The nonce uses the reserved `LIBTMUX_LIFECYCLE_NONCE` child
input and tmux global environment entry; application configuration must not
rewrite that internal entry during startup.

## Bounded discovery

`Discovery::new(roots)` scans the immediate children of explicit directories.
`Discovery::configured()` captures `/tmp/tmux-UID`, the current nonempty
`TMUX_TMPDIR/tmux-UID`, and the selected endpoint's parent. Extend `roots` to
include other locations. This is a bounded inventory of those directories.

Defaults allow 1,024 entries, 64 probes, 250 ms per probe and five seconds
for the search. `DiscoveryReport` returns borrowed servers, per-root and
per-candidate diagnostics, entry/probe counts and the first exhausted bound.
An empty readable directory differs from a failed probe or unreadable root.
Probes pass tmux's no-start option, so a stale socket stays stale.

Discovery skips root paths whose final component is a symlink and skips
symlink entries. Earlier path components follow the filesystem's ordinary
resolution. It records duplicate socket inodes and duplicate responding daemon
generations. Root enumeration runs on a blocking worker with an entry cap;
a filesystem call stalled in the kernel can outlive the caller's deadline,
but the caller returns with time truncation and starts no further probes.
