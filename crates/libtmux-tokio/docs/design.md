# Tokio driver design

The Tokio client shares creation requests, receipts and object keys with the
blocking client. The [Rust architecture decision](https://github.com/libtmux/libtmux-rs/pull/186)
describes the responsibilities of all four crates.

## Contents

- [Choose a connection](#choose-a-connection)
- [Recover a creation receipt](#recover-a-creation-receipt)
- [Bound parked clients](#bound-parked-clients)

## Choose a connection

A normal request runs a tmux subprocess. To consume pane output, acquire a
control client on a supported daemon with a compatible executable. A gap in
the output stream means some bytes were lost. A new capture can show what
is on the screen at that point. [Control and stream contracts](contracts.md)

The ordinary executor takes ownership of the OS child before registering
its pipes with Tokio. If pipe registration fails, the supervisor signals
the process group, reaps the child and returns the error. A fallback reaper
owns the child and its admission slot if the runtime shuts down first.
If a request reaches its deadline while startup or cleanup is still pending,
the caller waits at most one further second for acknowledgement.
[Tokio pipe registration](https://github.com/tokio-rs/tokio/blob/b53b2001ed2ccfd9b87e32a0c6da1a1eebdc80b8/tokio/src/process/unix/mod.rs#L122-L145)

## Recover a creation receipt

A failing after-hook can make tmux exit unsuccessfully after it created the
session. The error retains any confirmed creation receipt, including the
pane key. Use that receipt to inspect or clean up the session. Repeating
the creation when its effect is unknown can create a second session.

## Bound parked clients

`DispatchLimits::max_parked` defaults to 16 places shared by a server's clones
and its control-derived handles. Channel waits and queued locks use this
budget, leaving ordinary dispatch capacity available for signal and unlock.
Each place follows its client through reaping.

A timed-out wait stays on its channel. A later wait joins that client, and a
signal received with no caller left is retained until consumed. Retained
signals keep their places. Saturation refuses new clients with
`Error::ParkedLimitExceeded`; it never evicts a current generation's signal.
Fresh discovery of a replacement retires the prior generation's state and
cancels its parked clients. Their places return after reaping, and old
signals cannot satisfy waits on the replacement.

Literal raw `wait-for` commands and flat chains use parked capacity while
keeping the ordinary request deadline. Alias bodies and nested scripts are
not inspected; they retain their ordinary deadline and dispatch capacity.
