# Tokio driver design

The Tokio client shares creation requests, receipts and object keys with the
blocking client. The [Rust architecture decision](https://github.com/libtmux/libtmux-rs/pull/186)
describes the responsibilities of all four crates.

## Contents

- [Choose a connection](#choose-a-connection)
- [Recover a creation receipt](#recover-a-creation-receipt)

## Choose a connection

A normal request runs a tmux subprocess. To consume pane output, acquire a
control client on a supported daemon with a compatible executable. A gap in
the output stream means some bytes were lost. A new capture can show what
is on the screen at that point. [Control and stream contracts](contracts.md)

## Recover a creation receipt

A failing after-hook can make tmux exit unsuccessfully after it created the
session. The error retains any confirmed creation receipt, including the
pane key. Use that receipt to inspect or clean up the session. Repeating
the creation when its effect is unknown can create a second session.
