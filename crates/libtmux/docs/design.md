# Rust client design

The facade exposes native blocking and Tokio clients. Both clients accept the
same `NewSession`. Its creation receipt identifies the daemon, session, first
window and first pane. A caller can create the session synchronously and hand
the pane key directly to Tokio. [Architecture decision](adr/0001-native-blocking-and-tokio.md)

## Where a change belongs

| Package | Owns |
| --- | --- |
| `libtmux-core` | IDs, byte-preserving text and arguments, command encoding, receipt decoding, version rules, offline plans and filters |
| `libtmux-blocking` | Blocking process I/O, deadlines, cancellation and child cleanup |
| `libtmux-tokio` | Async process I/O, control connections, live handles and execution of shared plans |
| `libtmux` | Runtime selection and public re-exports |

Cargo enables blocking by default. Enabling `tokio` adds a second namespace.
With default features disabled, the facade can expose just the shared types.
`tmux-mcp` uses the Tokio driver. `tmux-workspace` constructs plans from core
operations.

## Identity and effects

A tmux ID belongs to the daemon that issued it. The socket path identifies
the endpoint. The daemon PID and whole-second start time identify the
observed generation. A PID reused in the same second is indistinguishable.
A dependent command checks its expected generation in the tmux queue before
using an ID. A window can occupy several session indexes. Operations that
remove a link also need its observed session and index.

`NewSession` defines the command, literal and format operands, and receipt
decoder for both drivers. An ordinary subprocess must preserve the receipt
even if a later hook fails. Failure before submission, acknowledged creation
and a lost acknowledgement have different replay consequences. Read the
receipt and effect classification before deciding whether to issue another
mutation. [Failure and control contracts](adr/0001-native-blocking-and-tokio.md)

## Verification

CI checks the normal dependency graph of external programs, runs private
real-tmux fixtures and exercises the examples from the packaged crates.
The release gate also runs the examples copied from rendered HTML and
Markdown. A successful preview applies to the source revision it built.

The published 0.1.2 design explains the original Tokio executor, format
dialects, query grammar and test fixtures. [0.1.2 design](https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/docs/design.md)
