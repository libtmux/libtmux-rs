# Discovery, traversal, refresh, and environment plan

## Outcome

Promote the private snapshot kernel into public `Session`, `Window`, `Pane`,
and `Client` handles reachable from `Server`, with listings, traversal,
refresh, and environment resolution. This slice adds no mutations: every
operation here reads.

The transport gate this slice previously carried is closed. Byte recovery is
proved on live tmux 3.4, 3.5a, 3.6, and 3.7b, so byte-bearing public snapshots
are unblocked.

## Fixed decisions

These follow [design.md](../design.md) and are not reopened here.

- Handles are concrete, cheap to clone, `Send + Sync`, and share one
  `Arc<Core>` while owning independent snapshots.
- Equality and hashing use `(ServerIdentity, object id)`; clients use
  `(ServerIdentity, client_name)`.
- `WindowLink` is a first-class edge. Server-wide window and pane listings
  return one row per winlink and never collapse linked windows.
- Snapshot getters are synchronous. Anything that reaches tmux is `async`.
- Every listing has a lenient accessor returning `Vec<T>` and a loud `try_*`
  form returning `Result<Vec<T>, Error>`. Singular relationships, environment
  reads, and liveness checks are loud.
- Listings return ordered `Vec<T>`. No collection wrapper is introduced.

## Open questions

Resolve by spike before the affected task, not by argument.

1. **Accessor return shape for optional fields.** Info fields are
   `Availability<T>`, which distinguishes release-unsupported, development-
   unproven, semantically absent, and present. Exposing `Availability<T>`
   publicly is honest but pushes a four-way match onto every caller for
   fields that are almost always present. Candidates: expose `Availability<T>`
   directly; expose `Option<&T>` plus a separate `availability()` probe; or
   expose `Option<&T>` only and keep the distinction internal. Spike all three
   against the WorkspaceBuilder consumer and pick by call-site weight.
2. **`Pane::window()` and stale winlinks.** A pane retains the winlink it was
   discovered through. After `move-window` or `link-window` that edge can be
   stale. Live re-resolution is correct but costs a command per call. Decide
   whether the parent accessor is async-and-live, or snapshot-and-explicit
   with a separate `refreshed_parent()`.
3. **Lenient accessor observability.** The lenient form swallows the cause. It
   must record through `tracing` when that feature is on, but decide whether a
   swallowed failure is also recoverable without re-running the loud form.

## Open question 4: filtering over public handles (resolved)

Resolved by candidate 1, with one refinement: rather than generating a second
companion, the existing one became generic over the type it filters. The
private snapshot uses `SessionFields<SessionInfo>` and the public handle uses
`SessionFields<Session>`, both built from one catalog. `Filterable` on the
handle delegates matching and validation to its snapshot, so only catalogued
fields are expressible and `Window`'s link fields simply have no handle yet.

The original text follows for the record.


The goal requires `sessions.iter().matching(expr)` and generated field handles
such as `pane::command().starts_with("nv")`. The query kernel already provides
all of it, but only for the crate-private `Info` types: `filter_fields`
produces `TextField<SessionInfo>`, so a `FilterExpr<SessionInfo>` cannot match
a `&Session`. Public listings return handles, so today the two halves do not
meet.

Candidates:

1. **Generate a second companion for each handle.** Extend
   `define_snapshot_info!` to emit a public `SessionFields` alongside the
   private one, and implement `Filterable for Session` by delegating
   `__filter_matches` to the inner snapshot. Best call-site ergonomics and
   keeps one catalog. Costs macro surgery across four entities, and `Window`
   and `Pane` wrap projections rather than plain snapshots, so their field sets
   must span the snapshot and the link.
2. **Promote the `Info` types and filter those.** Cheapest, but forces callers
   to reach through a handle to a snapshot to filter, which reads badly and
   leaks a second vocabulary for the same objects.
3. **Blanket-implement `Filterable` for any handle that can borrow a
   `Filterable` snapshot.** Avoids duplicate companions, but the field handles
   stay parameterized on the snapshot type, so the expression type still does
   not name the handle.

Candidate 1 is the likely answer because it is the only one where the type a
listing returns is the type an expression filters. Resolve it with a spike on
`Session` alone before touching `Window`, whose link fields make it the hard
case.

## Tasks

Each task lands with failing tests first, real-tmux evidence where behavior is
observable, a parity-ledger row, and the full crate gate.

1. ~~**Snapshot accessors.**~~ Done. Getters are generated alongside the
   struct, `Filterable`, and hydration output. Open question 1 resolved: one
   accessor per field returning `Availability<&T>`, with `available`,
   `is_available`, and `copied` helpers so callers who do not care why a value
   is missing pay one call.
2. ~~**Listing pipeline.**~~ Done, as `internal::listing`, with an explicit
   `Scope` for how tmux is asked. `list-panes` needed a separate session scope
   because tmux resolves a session target to that session's current window.
3. **Promote snapshots.** Deferred, and possibly unnecessary. The handles
   expose `Option<T>` getters instead, so nothing public needs the `Info`
   types yet. Revisit only when open question 4 or a caller demands the
   four-way availability evidence.
4. ~~**`Session` handle and `Server` listings.**~~ Done, apart from
   `attached_sessions`/`try_attached_sessions`.
5. ~~**`Window`, `Pane`, and `Client` handles.**~~ Done. The winlink contract
   is under real-tmux test: a window linked into two sessions yields two rows
   with equal ids and unequal handles.
6. ~~**Traversal.**~~ Done. Open question 2 resolved: parent resolution
   re-reads tmux, so a rename since discovery is visible while identity stays
   stable. A snapshot-only parent accessor was not added; nothing needed it.
7. ~~**Refresh.**~~ Done for `Session`, `Window`, and `Pane`, with clone
   independence under test. Still owed: a case where a field goes from
   nonempty to empty, and refresh for `Client`.
8. **Connections and environment.** Liveness and `has_session` are done.
   Still owed: `Server::from_env`, and loud server and session environment
   reads with explicit set and unset values.
9. **Scoped operations.** `with_server`, `Server::with_session`,
   `Session::with_window`, `Window::with_pane`, each awaiting cleanup after
   success, error, and cancellation.
10. ~~**Filtering over handles.**~~ Done. `Session`, `Window`, `Pane`, and
    `Client` implement `Filterable`; mismatched field operations are proved to
    fail at compile time by `compile_fail` doctests. Still owed: handles for
    `Window`'s link fields, and the `field__operator` edge parser.
11. **Slice gate.** Parity rows, docs, examples, and an independent review.

## Acceptance

- stale clones remain unchanged when another handle refreshes;
- refresh clears fields when live state changes from nonempty to empty;
- unset or cross-server ids never compare equal accidentally;
- linked-window traversal resolves both object and edge context correctly;
- each lenient path returns empty for list-operation failures while each loud
  path preserves the cause;
- scoped cleanup runs after success, error, cancellation, and panic;
- listings preserve tmux order and one row per winlink.

## Flattened getters (done)

Twenty-five handle getters returned `Option<T>` for cases that could not
happen. Four remain, and each is a field the catalog says can genuinely be
absent: `client_height`, `session_last_attached`, `pane_current_command`, and
`pane_current_path`. `Window` has none left.

The rule was derived rather than chosen. A field cannot be absent when its
floor is at or below the supported minimum and its empty policy is `Required`
or `Available`: the codec already fails hydration on an empty `Required`
field, an empty `Available` field is a value, and `classify_field` marks
anything at the floor `Selected` on numbered and development builds alike.
135 of the catalog's fields meet it.

Four macro arms carry the branch, all keyed the same way, so the shapes cannot
drift apart: `stored_type` for the struct, `borrowed_type` and `borrow_stored`
for the accessor, `decode_stored` for hydration, and `match_stored` for the
`Filterable` arm. Applying it turned up two fields my hand-written list had
wrong, which is the argument for deriving it: `client_height` is `Absent` and
had to stay optional, while `pane_title`, `pane_tty`, `window_layout`,
`pane_active`, `pane_dead`, `pane_in_mode`, `client_readonly`, and
`client_control_mode` were flat and I had missed them.

The snapshot test that asserted every supplement was `Availability<T>` now
asserts each field's shape against the same rule, split by the catalog rather
than by hand, so a field whose policy changes fails to compile until the test
says so.

## Notes

- **Containment sweep, readmission bug: fixed.** The termination loop removed a
  terminal process from `frozen` and rescanned in the same iteration, so a
  process the sweep had just killed returned as a new candidate. A freshly
  killed process is briefly a zombie whose identity still reads but whose
  environment does not, which the scanner reports as opacity following an
  earlier match. Terminal identities are now retired into a set the rescan
  skips, and
  `a_process_this_sweep_killed_is_not_readmitted_as_a_new_candidate` covers it
  with a scanner that keeps reporting the same identity.

- **Containment sweep, second cause: fixed.** Giving
  `terminate_all_with_scanner` a typed `SweepFailure` instead of a bare
  `false` identified it in one run: `TerminationScan`, the scan during
  teardown. Retirement could not help, because the failure happens inside the
  scan while classifying, not in what the sweep does with the result.

  Every process still frozen in that loop has already been killed, so a scan
  that cannot classify a candidate is not evidence one escaped. A process
  being torn down turns opaque, and the scan cannot tell that apart from one
  hiding. That pass now treats a scan error as "nothing new", because the loop
  only returns once every frozen process is terminal and the deadline still
  fails closed if that never happens.

  The instrumentation also corrected an earlier misreading: the `DiscoveryScan`
  first observed came from an over-tight assertion added alongside it in
  `later_scan_error_kills_all_previously_frozen_pidfds`, not from the flake.
  How many discovery passes run before that test's scanner starts failing is a
  race, so it asserts only that the sweep fails.

  `cargo test --lib test::containment` now passes 30 of 30, and the workspace
  suite 4 of 4.

  Reproduce with `cargo test --lib test::containment`, which fails about half
  its runs; the test passes alone and the full workspace suite passes.

  Prime suspect is the fixture rather than the sweep. That test's first
  candidate is `(41, <tempdir>)`: a hand-written `/proc` view claiming pid 41
  and this user's uid, whose `environ` is deliberately a directory so the
  candidate reads as opaque. The metadata is read from the tempdir, but pid 41
  is a real number on the host, so any step that opens or revalidates a pidfd
  for it touches a real process whose existence and ownership this test does
  not control. Initial opacity is meant to be skipped, not to fail the sweep,
  so either that skip is not reached or the pidfd path runs first.

  Next: assert which candidate `scan_process_paths` rejects, then give the
  fixture a pid that cannot exist rather than a low real one.
