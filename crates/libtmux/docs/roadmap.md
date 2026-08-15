# libtmux Rust delivery roadmap

## Outcome

Build one Tokio-native `libtmux` crate with behavioral parity to the Python API
at commit
[`c4a980b`](https://github.com/tmux-python/libtmux/tree/c4a980b), using the
architecture in [design.md](design.md). Each delivery slice begins with failing
tests, is implemented from a blank production surface rather than copied spike
code, and must pass the complete crate gate before the next slice starts.

Foundation and the public local filtering kernel are implemented. The format
catalog, intrinsic snapshots, and winlink projections are also implemented but
crate-private. Public hierarchy discovery, snapshots, and listings remain the
next slice; this formats slice is not verified until its final independent
review gate closes.

## Scope lock

The pinned Python commit is the first-release baseline. The parity ledger is
the source of truth for coverage. Later Python changes enter scope only through
an explicit baseline update that records the added, changed, and removed public
behavior.

The Rust crate keeps the same tmux 3.2a floor. Compatibility claims require
real tmux execution; parser fixtures alone do not establish support.

## Dependency order

```text
foundation
    |
    v
formats, snapshots, winlinks, and queries
    |
    v
discovery, traversal, refresh, and environment resolution
    |
    v
object mutations and interactions
    |
    v
options, hooks, and advanced command families
    |
    v
documentation, compatibility, and parity closure
```

The future engine-ops integration seam is checked in every slice but does not
add a second plan model, and control mode stays behind its own feature rather
than becoming the default transport.

## Cross-cutting contracts

Every slice preserves these contracts:

- public handles are concrete, cheap to clone, and `Send + Sync`;
- handles share connection state but own immutable snapshots;
- equality and hashing include normalized server identity;
- linked windows preserve separate object and winlink-edge identity;
- command arguments are never shell-expanded;
- command results preserve the exact transport bytes emitted by tmux until
  callers request decoding; original callback-byte recovery is a separate
  version-sensitive format contract;
- sensitive arguments and raw output never appear through `Debug`, errors, or
  tracing;
- caller cancellation, timeouts, and explicit shutdown kill and reap owned
  child processes;
- list-shaped accessors are lenient by default and have loud `try_*` forms;
- expression construction, explicit remote query execution, and mutations are
  loud;
- no executor claims per-command attribution without protocol evidence;
- tests use unique explicit socket paths and bounded observable polling;
- no public API is added without documentation, an executable example, and a
  parity-ledger row.

## Foundation

Detailed plan: [01-foundation.md](plans/01-foundation.md)

Deliver:

- Cargo, pinned Rust, lint, feature, package, and just recipe configuration;
- public errors, version values, typed IDs, targets, and structural server
  identity;
- redaction-aware commands, request identity, and raw results;
- immutable engine capabilities;
- a private object-safe executor and supervised Tokio subprocess transport;
- the initial concrete `Server` and raw command boundary;
- public `test-support` with isolated real-tmux lifecycle management.

Acceptance:

- target and version parsers reject malformed input without lossy fallback;
- independent servers aimed at the same endpoint compare equally;
- no diagnostic surface exposes sentinel secrets or raw output;
- non-zero tmux status remains a raw result;
- timeout, dropped future, and task abort paths terminate and reap the client;
- every exported foundation type is `Send + Sync`;
- parallel, cancellation, and panic-path test servers prove their owned
  foreground daemon PIDs and sockets are gone.

## Formats, snapshots, winlinks, and queries

Detailed plan: [02-formats-snapshots-filtering.md](plans/02-formats-snapshots-filtering.md)

Deliver:

- one private format-descriptor catalog with semantic ownership, list profile,
  decoder, empty policy, and version data;
- private q row framing and typed snapshot parsing on raw-q transport;
- public byte-preserving `TmuxText` and crate-private explicit
  `Availability<T>`;
- crate-private intrinsic `SessionInfo`, `WindowInfo`, `PaneInfo`, and
  `ClientInfo`;
- crate-private first-class `WindowLink`, `WindowLinkIdentity`,
  `WindowProjection`, and `PaneProjection` values;
- native borrowed iteration over user-owned ordered `Vec<T>` and slices, with
  ordered `Vec<T>` as the floor for future public listings;
- `Matcher<T>`, `QueryIteratorExt`, exact cardinality, and opaque
  `FilterExpr<T>`;
- pre-generated typed scalar fields and explicit relation quantifiers;
- optional versioned serde and downstream `#[derive(Filterable)]` support.

Current status: the public filtering values and crate-private format, snapshot,
and projection kernels are implemented. Public snapshot consumers and
listings remain in discovery, and Task 17 owns the final independent review
and delivery gate.

Acceptance:

- q-framed raw transport on pinned tmux 3.2a and 3.6 and current 3.7b survives
  delimiter-like bytes, embedded newlines, and invalid UTF-8 values;
- the VIS transport window is bounded by upstream commits `7e497c7f` and
  `93b1b781` before tag 3.4 and `5fd45b38` before tag 3.6, not by observed
  behavior alone;
- live tmux 3.4 and 3.5a runs recover arbitrary original non-NUL callback
  bytes through the VIS dialect, and every nonzero byte value round-trips
  through its grammar;
- a plan whose dialect disagrees with the daemon that produced the output
  fails loudly at the first divergent escape rather than decoding it;
- exact tmux 3.2a and 3.6 runs exercise both sides of version-dependent
  linked-window aggregate semantics;
- release-unsupported, development-unproven, semantically absent, empty, and
  available values remain distinct only where tmux and the field policy can
  prove the distinction;
- server-wide window and pane parsing preserves every winlink edge;
- native `.filter()` remains the closure path and `.matching()` is lazy over
  `Iterator<Item = &T>`;
- scalar operators, boolean composition, and exact cardinality match the
  documented Rust parity boundary without collecting;
- field-operation mismatches fail to compile;
- empty to-many relations satisfy `all` and `none`, but not `any`;
- duplicate members and unknown wire versions, fields, operators, and
  quantifiers fail loudly from raw input;
- Rust regex syntax differences are recorded and tested;
- serde output matches stable versioned JSON fixtures on Rust 1.85 and the
  development toolchain.

Native tmux pushdown, residual partitioning, ordering plans, limits,
`server.query_*`, and the dynamic `field__operator` edge parser remain a later
execution slice. Local `matching()` never pushes down.

## Discovery, traversal, refresh, and environment resolution

Write a detailed plan after typed parsing and query semantics pass their gate.

Deliver:

- `Server`, `Session`, `Window`, `Pane`, and `Client` discovery APIs;
- hierarchy collections with lenient and loud variants;
- live parent and active-object resolution;
- immutable snapshot getters, `refresh(&mut self)`, and `refreshed(&self)`;
- default, named, explicit-path, and `TMUX`-environment connections;
- loud server and session environment reads with explicit set/unset values;
- server liveness and loud dead-server checks;
- async scoped server, session, window, and pane operations.

The version-aware transport this slice once blocked on is delivered: byte
recovery is proved on live tmux 3.4, 3.5a, 3.6, and 3.7b, so byte-bearing
public snapshots are no longer gated on it.

Acceptance:

- stale clones remain unchanged when another handle refreshes;
- refresh clears fields when live state changes from nonempty to empty;
- unset or cross-server IDs never compare equal accidentally;
- linked-window traversal resolves both object and edge context correctly;
- each collection's lenient path returns empty for list-operation failures and
  each loud path preserves the cause;
- scoped cleanup runs after success, error, cancellation, and panic.

## Object mutations and interactions

Write plans per object so each API family remains independently reviewable.

Deliver:

- server and session lifecycle operations;
- server and session environment set, unset, and remove operations;
- window create, link, move, swap, rename, layout, and selection operations;
- pane split, move, resize, capture, input, mode, title, and process operations;
- client attach, detach, suspend, switch, and refresh operations;
- consuming `#[must_use]` builders for operations with several optional
  clauses;
- typed flags, directions, targets, indices, sizes, and environment values.

Acceptance:

- every mutation has a real-tmux effect assertion rather than command-string
  inspection alone;
- sensitive pane input and environment values remain redacted;
- target disappearance is distinct from connection failure;
- version-gated flags have supported and unsupported behavior tests;
- builders cannot emit conflicting or meaningless tmux flags.

## Options, hooks, and advanced command families

Split this work by public capability rather than internal helper shape.

Deliver:

- typed global, server, session, window, and pane option scopes;
- option get, set, append, unset, and typed parsing;
- hook get, set, append, unset, and run operations;
- keys, buffers, commands, menus, prompts, popups, and related APIs present in
  the pinned baseline;
- raw escape hatches where tmux intentionally accepts open-ended syntax.

Acceptance:

- scope-invalid options and hooks are unrepresentable or rejected locally;
- array options and repeated hooks preserve ordering and indices;
- raw values remain available when no typed decoder exists;
- prompts, buffers, environment values, and pane input pass the redaction
  suite;
- public methods, errors, and return cardinality match their ledger rows.

## Documentation, compatibility, and parity closure

Deliver:

- crate-level documentation and focused object, query, testing, and migration
  guides;
- compiling doctests for every public operation family;
- examples using isolated tmux servers;
- Linux coverage across tmux 3.2a and maintained stable releases;
- latest-stable macOS coverage;
- feature-powerset, MSRV, dependency audit, semver, and package-content checks;
- an adversarial Rust API review and a final Python parity audit.

Acceptance:

- every pinned-baseline public capability is implemented or recorded as an
  intentional Rust syntax or verified bug-correction delta;
- every ledger row is `verified` or `excluded` with focused evidence;
- all format, lint, test, doctest, docs, feature, MSRV, package, and real-tmux
  gates pass from a clean checkout;
- the final adversarial review has no unresolved critical or important finding;
- shipped files contain no spike code, local paths, private data, unstable
  source links, AI signatures, or ownerless scaffolding.

## Authoritative gate

Cargo remains authoritative. The justfile is a convenience facade that invokes
the same commands:

```console
$ just check
```

The aggregate gate covers formatting, Clippy with warnings denied, all targets
and features, doctests, documentation with warnings denied, no-default-features,
the supported feature powerset, MSRV, and package verification. The exact
commands live in the `justfile` and CI so local and hosted checks cannot drift.

Repository commits and publication are separate user-authorized actions; the
delivery gate does not imply either one.
