# libtmux Rust delivery roadmap

## Where the plan lives now

This file used to carry the slice-by-slice delivery plan. Every slice it
described has shipped, and it went on claiming otherwise -- that public
hierarchy discovery was "the next slice" -- long after that was true. A plan
that has been overtaken is worse than no plan, because it is read as current.

What replaced it:

- [parity.md](parity.md) is the ledger and the definition of done. Each row is
  a Python capability with a Rust answer and a status of `implemented`,
  `excluded`, or `planned`. Counting the rows is the progress report.
- [design.md](design.md) carries the reasoning: transport, the snapshot and
  format boundary, the query grammar, control mode, test architecture, the
  compatibility lanes, and every tmux defect worked around.
- [AGENTS.md](../../../AGENTS.md) carries the conventions, the release
  process, and the things that will bite.

Keep this file short. If it needs a status, it will be wrong within a week.

## Baseline

Parity is measured against Python libtmux at commit
[`c4a980b`](https://github.com/tmux-python/libtmux/tree/c4a980b). Behavioral
equivalence is the target, not syntactic imitation: where Python's shape is an
artifact of Python, the ledger records an intentional divergence and why.

## Contracts that outlive any slice

These hold across the whole crate, and a change that breaks one is a design
decision rather than an implementation detail:

- public handles are concrete, cheap to clone, and `Send + Sync`;
- handles share connection state but own immutable snapshots;
- equality and hashing include normalized server identity;
- linked windows preserve separate object and winlink-edge identity;
- command arguments are never shell-expanded;
- command results preserve the exact transport bytes tmux emitted until a
  caller asks for decoding;
- sensitive arguments and raw output never appear through `Debug`, errors, or
  tracing;
- caller cancellation, timeouts, and explicit shutdown kill and reap owned
  child processes;
- list-shaped accessors are lenient by default and have loud `try_*` forms;
- expression construction, remote query execution, and mutations are loud;
- no executor claims per-command attribution without protocol evidence;
- tests use unique explicit socket paths under the fixture root and bounded
  observable polling;
- no public API is added without documentation, a runnable example, and a
  parity-ledger row.

## The gate

`just check` is authoritative and runs what CI runs. A slice is not done until
it passes on the commit that claims it. Read its exit status directly: piping
it into anything reports the pipe's status instead, which has already hidden a
real failure once.
