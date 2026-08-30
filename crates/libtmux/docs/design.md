# libtmux Rust crate design

## Outcome

`rs/` is a Cargo workspace whose members all live under `rs/crates/`:

| Crate            | Published | What it is                                    |
| ---------------- | --------- | --------------------------------------------- |
| `libtmux`        | yes       | The async tmux client and object model         |
| `libtmux-macros` | yes       | `#[derive(Filterable)]`, for downstream structs |
| `tmux-mcp`       | no        | A Model Context Protocol server over `libtmux` |
| `tmux-workspace` | no        | Builds tmux workspaces from tmuxp-style YAML   |

The two unpublished crates exist to exercise the public API from outside, the
way a real consumer would. The dependency runs one way: `libtmux` knows
nothing about either.

`libtmux` presents `Server`, `Session`, `Window`, `Pane`, and `Client`
handles, hierarchy listings, refresh, options, hooks, buffers, key bindings,
and the local typed filtering kernel, keeping Python libtmux's object
vocabulary and the same tmux floor of 3.2a. The format catalog, intrinsic
snapshots, and winlink projections back those handles and stay crate-private.
Optional features add control mode, a blocking runtime, portable expression
serialization, tracing, and the real-tmux test guard.

Cargo is authoritative for compilation, dependency resolution, testing,
documentation, and publishing. A `justfile` groups those commands so `just`
alone lists them; it adds no build step and does not participate in the
published crate.

The first transport uses Tokio subprocesses. Command, target, result,
capability, snapshot, and query values do not depend on that transport, so a
future control-mode or engine-ops crate can reuse them without changing normal
object APIs.

## Goals

- Reach behavioral feature parity with the pinned Python public API baseline.
- Keep familiar names and hierarchy while using Rust ownership, errors,
  builders, iterators, and async methods naturally.
- Preserve tmux 3.2a compatibility and gate newer format fields and flags by
  detected capabilities.
- Make IDs, targets, fields, option scopes, directions, flags, and query
  cardinality statically meaningful.
- Preserve the bytes emitted by tmux and expose decoding explicitly without
  assuming every tmux release emits untransformed callback bytes.
- Provide isolated real-tmux tests with explicit socket paths and fail-closed,
  platform-scoped cleanup.
- Leave a coherent extension seam for immutable plans, query lowering,
  control mode, serialization, and alternate executors.
- Keep the default crate small enough to audit and pleasant to embed.

## Non-goals

- A JavaScript, N-API, Python, WASM, or C binding.
- A second synchronous API.
- Control mode in the default transport. Normal commands stay one process per
  command whatever the optional `control-mode` feature does.
- A literal port of Python inheritance, arbitrary keyword arguments, mutable
  dictionaries, or exceptions.
- Compatibility methods whose only current behavior is raising
  `DeprecatedError`.
- Reproducing Python's `QueryList` type, its equality and lookup-suffix
  defects, stale fields, or cross-server identity defects.
- A process-global engine or operation registry.

## Compatibility contract

The target compatibility surface is tmux 3.2a and newer on Linux, macOS, and
WSL. Native Windows is unsupported because tmux is unavailable there. The
initial Rust MSRV is 1.85, the first stable compiler supporting Edition 2024.
The Foundation gate verifies Linux with both Rust 1.85 and the pinned
development toolchain; the release slice adds the remaining platform matrix.

The compatibility baseline for the first Rust release is the Python package at
commit
[`c4a980b`](https://github.com/tmux-python/libtmux/tree/c4a980b). The parity
ledger treats every module in its API reference as public, including
`Client`. Internal Python implementation types are ported only when their
behavior is visible through public APIs.

That baseline does not move during implementation. Later Python changes enter
the Rust scope only through an explicit baseline update and parity-ledger diff.

Behavioral parity means equivalent tmux effects, return information,
cardinality, ordering, version gating, and documented failure behavior. Rust
syntax intentionally differs where Python constructs have no sound or
idiomatic equivalent.

## Evaluated architectures

### Literal Python-shaped port

This approach would expose string IDs, dynamic `field__operator` lookups,
mutable property bags, broad target strings, and methods with many optional
arguments. It offers superficial familiarity but loses Rust's type checker,
cannot make live properties async, and preserves accidental behavior. It is
rejected.

### Runtime-generic core and adapter workspace

This approach would begin with separate core and Tokio crates and make every
handle generic over a transport. It keeps dispatch allocation-free, but
`Server<T>` propagates through `Session<T>`, `Window<T>`, `Pane<T>`, queries,
errors, and downstream signatures. The crate/version/feature coordination has
no initial consumer. It is rejected as premature.

### Tokio-first crate with an internal executor boundary

This is the selected design. Public handles are concrete and cheap to clone.
They share an `Arc<Core>`, while `Core` owns a private object-safe executor.
The subprocess executor is Tokio-native. Public values above execution remain
transport-independent.

The design grafts three proven ideas without importing their competing plan
models:

- immutable, inspectable query and command values as the semantic source;
- the familiar `Server -> Session -> Window -> Pane` authoring facade;
- typed fields, portable predicates, traversal, and explicit cardinality.

A later engine layer can embed the same query and command values in one
immutable statement graph. It must not introduce a second semantic model.

## Spike findings

The disposable spike compiled and exercised the selected seams against real
tmux. Its source is not copied into the crate.

- Native async methods in a public transport trait are not object-safe. An
  internal boxed adapter kept the public transport implementation ergonomic
  and every object handle non-generic.
- Direct transport generics compiled but spread through all child handle
  types. A closed transport enum compiled but prevented downstream engines.
- A single predicate trait accepting both closures and typed expressions
  defeated inline closure parameter inference on Rust 1.85. Native
  `.filter(|item| ...)` therefore remains the closure path, while
  `.matching(expr)` accepts portable expressions and named matchers. No
  collection wrapper or duplicate closure method is needed.
- Single-evaluation `q`-quoted tmux formats round-tripped delimiter bytes,
  embedded newlines, and invalid UTF-8 on pinned tmux 3.2a. Delimiter
  splitting and separately sampled length prefixes did not work.
- tmux 3.4 and 3.5 wrap that output in `VIS_OCTAL | VIS_CSTYLE | VIS_NOSLASH`,
  so the transport carries two escaping layers rather than one. The layers do
  not collide: `#{q:}` runs first and doubles every backslash, `VIS_NOSLASH`
  stops `vis` adding more, and no member of tmux's `format_quote_shell` set is
  an octal digit or a `VIS_CSTYLE` letter. Each dialect is therefore a separate
  injective grammar, and a decoder that accepts only one of them rejects the
  other's output instead of returning altered bytes.
- Owned snapshots refreshed predictably. Refreshing one clone did not mutate
  another clone, avoiding locks and hidden shared state.
- A real-tmux guard created unique short socket paths, exposed the exact path,
  and removed the daemon and socket on drop.
- Loud list access and explicit `*_or_empty` access can coexist without making
  raw command execution swallow failures.
- A failed command at the start of a tmux semicolon chain prevents later
  commands from executing. A control-mode implementation that predicts one
  result block per separator can wait forever for blocks tmux will never send.
- A bare `;` argv element is always a separator and never an argument, and a
  `\;` element is always a literal semicolon and never a separator. The two are
  distinguishable only at the argv layer, so a separator has to be produced by
  the type that owns the sequence rather than by any argument a caller supplies.
- A subprocess chain cannot attribute its own failure. Failing at the first,
  middle, or last position of a chain returns the same exit code and the same
  stderr; only the count of stdout lines differs, and mutating commands print
  nothing, so a chain of them carries no positional evidence at all. Reporting
  the first command as the failed one is therefore a guess that is wrong
  whenever the failure was later. Without per-command evidence the terminal
  state of every member is `unknown`.
- A creating command's `-P -F` output survives a later failure in the same
  chain, so a fold that captures an id still binds it when a decorate fails.
- A fixture that leaves `default-shell` unset does not get a neutral shell, it
  gets `$SHELL`. Every fixture pane then sources the developer's interactive
  startup files, which makes the suite's timing a property of their dotfiles.
  Measured here: an interactive `zsh` reading a 395-line `.zshrc` drew a prompt
  in ~1 s idle and 9.7 s while other tmux servers on the machine were starting
  shells, against ~10 ms for a pinned shell reading nothing. Startup files that
  enable shared shell history compound it, because each shell then takes one
  machine-wide lock on one history file. Pinning `default-shell` *and*
  `default-command` is what removes it: with only the former tmux still runs
  the shell as a login shell and reads `/etc/profile` and `~/.profile`.
- Control mode does not deliver `split-window`'s `-P` output. Measured on tmux
  3.7b, `new-session -P -F` returns `$1 @1 %1` and `new-window -P -F` returns
  `@2 %2` inside their protocol blocks, while `split-window -P -F` closes its
  block successfully with no lines at all, whether the target window is in the
  attached session or another one. A plan that binds a created id from printed
  output therefore cannot create panes by splitting over control mode, and the
  `{marked}` fold is what a subprocess transport offers instead.
- Control mode is the only transport that separates how many commands from how
  many processes. Measured over one workload: one invocation per operation
  costs six processes and names a failure exactly; folding costs three and
  cannot; control mode costs one process, still reports six outcomes, and was
  the fastest of the five. Folding is therefore the answer for a subprocess
  transport and not an improvement over control mode.
- tmux reports hooks differently depending on how it is asked. `show-hooks`
  lists every hook name it knows, bare when the hook holds nothing and
  `name[index] value` when it holds something -- but only at server and
  session scope. For a window or a pane it reports nothing at all, and
  `show-options` omits them there too while still listing ordinary options.
  Asked about one hook *by name*, tmux lists its slots at every scope. So
  reading one hook by name works everywhere and enumerating them does not,
  which is why `Window` and `Pane` offer `hook` and no listing: a listing
  there could answer nothing but empty, which reads as "none set" rather than
  "not reported".
- Hook indices are sparse and tmux keeps the gaps: setting slots 0 and 3
  leaves 1 and 2 absent rather than closing up. A hook is therefore a map from
  index to command, not a list.
- Whether `<shell> -c "cmd"` becomes `cmd` or stays a shell with a child is an
  optimization POSIX does not require: `zsh` and `bash` exec, `dash` forks. A
  test that reads `#{pane_current_command}` therefore has to say `exec` rather
  than inherit whichever answer the ambient shell gives, and one that wants
  escape sequences in a pane's output has to emit them rather than rely on a
  prompt being colourful.

## Architecture

```text
Server / Session / Window / Pane / Client
                 |
                 +--> operation builders --> Command
                 |
                 +--> Vec snapshots --> native iterators
                                      |
                                      +--> Matcher / FilterExpr
                 |
                 +--> immutable Info snapshots
                                   |
                              Arc<Core>
                                   |
                         private Executor trait
                                   |
                        Tokio subprocess executor
                                   |
                                  tmux
```

### Core and executor

`Core` owns the configured tmux executable, captured launch context, server
identity, default timeout, capability snapshot, and executor. Handles contain
`Arc<Core>` plus owned identity and snapshot data. No public handle is generic
over a runtime or executor.

The private executor is stored as
`Arc<dyn Executor + Send + Sync + 'static>`. It consumes an owned
`CommandRequest` and returns a `Send` future producing a raw result. It never
receives domain objects. Adding a public alternate executor later is additive
because the boundary already exists, but the first release does not freeze an
unused public async trait.

Each `Core` owns its dispatch-request counter. Cloned `Server` values share
that Core and counter; separately constructed servers have separate scopes,
even when they identify the same tmux endpoint. The Core allocates an ID before
executor validation, so a rejected request can carry an ID without spawning a
process. A caller retry is another Core dispatch request and receives another
ID. The ID is not globally unique or a process ID.

The Tokio executor uses argv directly and never invokes a shell. Each spawned
child is transferred to an independently owned supervisor task. That task
captures stdout and stderr concurrently, applies a deadline, and remains
responsible for terminating the child's isolated process group and awaiting the
direct child after deadline expiry, caller cancellation, or explicit shutdown.
Pipe draining precedes `wait`, so the unreaped direct child anchors the numeric
process-group identity until inherited pipes close or the overall deadline
expires. Dropping the caller future signals cancellation; `kill_on_drop` is
only a final safety net and is never the reaping strategy.

Deterministic async reaping requires the Tokio runtime to remain alive until
the operation or explicit executor shutdown completes. Runtime teardown cannot
make that async guarantee; it synchronously signals the process group and uses
`kill_on_drop` for the direct-child fallback, without claiming a completed
wait. Callers that own a runtime shut the `Server` down before the runtime.
`TestServer` separately owns its foreground tmux daemon as a synchronous child
so its `Drop` cleanup does not depend on a live Tokio runtime.

Concurrent subprocess requests have no ordering guarantee beyond the atomic
execution of each process. Callers that require ordering await requests in
sequence. A future persistent executor serializes protocol writes internally
while preserving correlation between concurrent callers. Every public handle
is `Send + Sync`; compile-time assertions make that contract executable.

### Commands and results

`Command` stores typed tokens rather than one shell string. Each token carries
both its executable value and a public-or-sensitive diagnostic classification.
Custom `Debug` renderers, executor errors, and tracing use only the redacted
diagnostic view. Every Foundation command token is literal, so a token ending
in `;` gains one escape byte during argv lowering. Existing backslashes do not
suppress that lowering. Shell-free execution alone does not make semicolons
literal because tmux reparses every argv token after process launch.

Foundation Core dispatch accepts one logical command per request.
`CommandResult` stores the Core-scoped dispatch-request identity, a sanitized
command summary, exit status, and raw stdout and stderr bytes. It never copies
executable argv.
Borrowed strict UTF-8 views return
`std::str::Utf8Error`; named lossy views are explicit. Owned decoding errors
are not retained because their diagnostics could expose raw output. A non-zero
status and non-empty stderr remain data at the raw `cmd()` boundary; domain
wrappers decide which tmux responses are failures.

Batching and structural semicolon chains are deferred. When they are added,
independent batches return one result per request. A direct subprocess chain is
one Core dispatch request and one result because subprocess output cannot prove
per-command attribution. A planner that requires individual terminal states
must dispatch independently unless the executor advertises per-command
correlation. Control-mode block identities remain separate from the Core
dispatch-request identity and are retained rather than merged; any command
without evidence is `unknown`, never guessed to be failed or skipped.

Later builders for pane input, environment values, prompts, and buffers must
mark those arguments sensitive at construction. Each owning slice passes
sentinel secrets through its command families and proves that `Debug`, errors,
and tracing omit them.

### IDs, targets, and relationships

`SessionId`, `WindowId`, and `PaneId` validate `$`, `@`, and `%` prefixes.
Targets are scope-specific: pane operations cannot accept window targets.
`ServerIdentity` structurally normalizes the effective socket endpoint,
including the resolved socket root for named and default sockets and the
absolute path captured for explicit sockets. Symlink-sensitive parent
components are preserved rather than collapsed, and dispatch uses the same
captured path as identity. It never uses `Arc` pointer identity. Independently
constructed servers for the same structural endpoint therefore compare
equally, while equal-looking object IDs on different endpoints do not.

Handle equality and hashing use `(ServerIdentity, object ID)` for sessions,
windows, and panes, and `(ServerIdentity, client_name)` for clients. Window and
pane handle identity follows the underlying tmux object, not the discovery
edge. `WindowLinkIdentity` separately represents
`(ServerIdentity, SessionId, window index, WindowId)`.

A strict ownership tree is insufficient because tmux windows are linked.
`WindowLink` is a first-class edge containing session ID, window index, and
window ID. Server-wide window and pane enumeration retains one row per
winlink. Underlying object identity and edge identity remain distinct.

`Pane` retains the winlink context from which it was discovered. Live parent
resolution re-queries tmux so move and link operations do not leave traversal
permanently attached to stale parents.

### Private snapshots and future refresh

The current `SessionInfo`, `WindowInfo`, `PaneInfo`, `ClientInfo`,
`Availability<T>`, projections, format plans, and built-in fields are
crate-private. No public hierarchy handle or listing can return them yet. The
discovery slice will promote only the values needed by a public consumer and
define handle refresh around complete owned snapshots.

Inside that private kernel, known IDs, indices, flags, sizes, timestamps, and
enums use typed fields. `TmuxText` retains stored bytes exactly and exposes
byte, strict UTF-8, and explicitly lossy views without an implicit string
conversion. `Availability<T>` distinguishes release-gated `Unsupported`,
development-build `Unproven`, semantic absence, and available values where
tmux makes those states observable. Both unavailable states are omitted from
the format plan:
`Unsupported` requires a detected release below the field floor, while
`Unproven` records that a release-less development build is admitted only to
the conservative baseline catalog. For a supported field, the descriptor
decides whether zero bytes mean `Absent` or an available empty `TmuxText`;
tmux does not reveal whether its callback returned `NULL` or an empty C
string. A scope-inapplicable field is a plan error rather than `Absent`.

`WindowInfo` describes an underlying window. Session ID, index, active state,
and link flags belong to `WindowLink`. Server-wide window rows become owned
`WindowProjection` values containing one link and one intrinsic window.
`PaneProjection` similarly retains the link identity for each pane row. The
same window or pane may therefore appear more than once without conflating
object identity with discovery-edge identity.

Comma-joined tmux callbacks that contain names remain opaque `TmuxText`:
tmux permits commas in those names and provides no escaping. Typed
relationships come from projection rows and IDs, never by splitting those
display values.

Future handles expose synchronous snapshot getters while live relationships
and commands remain async. The planned `refresh(&mut self)` replaces the
receiver's complete snapshot and returns `&mut Self`; `refreshed(&self)`
returns a new handle. Clones remain independent snapshots sharing only
`Core`.

The private descriptor catalog records stable field name, semantic owner,
required context, admitted list profiles, minimum tmux version, decoder, and
empty-value policy. A `FormatPlan` owns the selected descriptors, rendered
template, and parser order together. Each requested field is rendered once as
`#{q:field}%`, and transport bytes are parsed before any newline split or
UTF-8 decode. The decoder removes each quoting backslash and treats only the
template's unescaped `%` as a field terminator; the LF after the final
terminator ends the row. Value newlines are ordinary payload because rows end
at a counted terminator rather than at the next LF.

`CommandResult` always preserves the exact stdout transport emitted by the
selected tmux, and the plan decodes it through the dialect its version emits.
`TransportDialect::RawQ` covers releases before 3.4 and from 3.6 onward, where
`#{q:}` escaping reaches stdout verbatim. `TransportDialect::Vis` covers 3.4
and 3.5, where `server_client_print` additionally applied
`VIS_OCTAL | VIS_CSTYLE | VIS_NOSLASH`. Both recover arbitrary original non-NUL
callback bytes.

The window boundaries come from upstream commits rather than from observed
behavior: `7e497c7f` and `93b1b781` introduced the transform before tag 3.4,
and `5fd45b38`, "Do not strvis output to terminal from commands", restored
verbatim output before tag 3.6. A `next-X.Y` identifier resolves through the
release it precedes; `master` names no release and selects `RawQ`.

Dialect selection is evidence, not trust. The version probe reads the
configured client executable, but `server_client_print` runs in the daemon that
owns the socket, so the two can disagree. Each dialect therefore rejects the
escapes only the other produces: a mismatched pairing fails loudly at the first
divergent escape instead of decoding into different bytes.

Each decoded field is therefore one callback sample. Dangling escapes,
missing field terminators, missing row LF, and trailing bytes are framing
errors. The parser never splices or retries individual fields or rows.
Tmux still expands fields and rows sequentially, so a byte-exact decoded
listing is not a transactional snapshot of server state.

There is no aggregate `ServerSnapshot`. Future Session, Window, Pane, and
Client listings come from separate tmux commands and cannot form an atomic
capture. Their return floor is an ordered `Vec<T>`, but no public hierarchy
listing exists at the current milestone.

### Native local iteration

The implemented query kernel operates on user-owned `Vec<T>` and slices.
Future hierarchy listings use the same collection floor and preserve tmux
order. Native iterators provide the ordinary local API:

```rust
let tasks = vec![("build", false), ("test", true)];
let pending = tasks.iter().filter(|task| !task.1);
let first = pending.clone().next();
let collected = pending.collect::<Vec<_>>();
```

`QueryIteratorExt` is implemented only for iterators whose item is `&T`.
`matching()` is lazy and preserves order; `vec.iter().matching(expr)` works,
while `vec.into_iter().matching(expr)` intentionally does not. `Matcher<T>`
has a blanket implementation for `Fn(&T) -> bool`, but inline closures use
native `.filter()` because the blanket bound cannot infer an untyped closure
parameter on the MSRV.

`exactly_one()` inspects at most two items and returns `ExactlyOneError` with
distinct zero and multiple variants. `one_or_none()` returns `None`, one
borrowed item, or `MultipleItemsError`. Neither method counts, collects, or
exhausts a potentially infinite iterator. Importing both this extension trait
and `itertools::Itertools` makes the shared `exactly_one` method name
ambiguous; callers in that uncommon case use trait-qualified syntax.

### Portable filters

`FilterExpr<T>` is the only portable predicate value. It is an opaque typed
wrapper over an inert tree of scalar comparisons, ordered `and` and `or`
junctions, `not`, and explicit relation quantifiers. It stores stable field
names and owned literals, never reader closures or executor state. Structural
equality flattens adjacent junctions while retaining operand order; it does
not claim logical equivalence for reordered predicates.

Generated field handles make invalid operations fail to compile on downstream
data through the current public API:

```rust
# // The derive is behind `derive`, which is not a default feature.
# #[cfg(not(feature = "derive"))]
# fn main() -> Result<(), Box<dyn std::error::Error>> { Ok(()) }
# #[cfg(feature = "derive")]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use libtmux::query::{Filterable as _, QueryIteratorExt as _};

#[derive(libtmux::Filterable)]
#[filterable(target = "task", crate = "::libtmux")]
struct Task {
    name: String,
    done: bool,
}

let tasks = vec![Task { name: "build".into(), done: false }];
let fields = Task::filter_fields();
let expression = fields.name.starts_with("build").and(fields.done.eq(false));
let task = tasks.iter().matching(&expression).exactly_one()?;
# let _ = task;
# Ok(())
# }
```

String, boolean, integer, enum, to-one, and to-many handles expose only their
valid operations. To-many relations use `any`, `all`, and `none`; an empty
relation satisfies `all` and `none` but not `any`. To-one relations use `is`
and an absent value does not match. Relation matching is synchronous and may
inspect only relationships already present in the candidate value. Built-in
relation handles therefore land with an explicit hydrated projection rather
than performing hidden tmux I/O or treating unloaded relationships as empty.

Rust membership authoring accepts any `IntoIterator`. The version 1 wire uses
arrays only; membership in an empty array is false and exclusion from it is
true after the candidate meets its field precondition. Python's string-RHS
`in` behavior is not preserved; substring matching uses `contains`. Portable
booleans are JSON booleans, text and enum values are strings, and every
fixed-width integer is a canonical base-10 string. This avoids TypeScript
number precision loss. `isize` and `usize` are excluded because their range is
target-dependent.

Wire decoding stops at 64 expression levels, 4,096 expression nodes, or 4,096
membership values. The limits bound recursive validation and retained input
independently of the deserializer. The schema states the array limits; its
recursive grammar cannot express one budget shared across the whole tree.

`lt`, `lte`, `gt`, and `gte` take one bound rather than a set, and appear in
the schema beside the text operators because an integer rides as text. Which
fields accept them is the crate's business rather than the schema's: a bound
on a text field is refused when it is decoded, the same way an unknown field
is. Ordering compares the decoded integer, not the string, so `"10"` is above
`"9"`. A port reading this grammar has to do the same.

Scalar case-insensitive text operators use Unicode default case folding
without normalization, so `Straße` equals `STRASSE` and a composed `é` does
not equal a decomposed one. Case-insensitive regex uses the Rust `regex`
grammar and its own folding instead, because folding regex syntax as ordinary
text would change the pattern language. The two therefore disagree: `Straße`
equals `STRASSE` as text and does not match `^strasse$` as a regex.

Wire schema version 1 implies that dialect. It is asserted by
[`tests/filter_dialect.rs`](../tests/filter_dialect.rs) against
[`tests/fixtures/dialect-v1/text.json`](../tests/fixtures/dialect-v1/text.json),
which records the answers rather than the library version that happens to
produce them — a port in another language can be held to the same file.
`filter_serde.rs` and the `filter-v1` fixtures lock the wire shape; they say
nothing about what an operator matches, which is why the dialect needs its
own fixture. Pinning dependencies to exact versions is not a substitute: it
freezes a version without checking any of this, and makes the crate
uninstallable alongside a consumer that needs a newer patch.

The optional `derive` feature re-exports `#[derive(Filterable)]` from a
sibling proc-macro package for downstream structs. Built-in libtmux fields
are generated inside the core crate and do not require that feature. The
derive generates typed handles and evaluation for scalar fields and explicit
`Vec<T>` and `Option<T>` relations.

Custom enum fields implement `FilterEnum`, which supplies a closed list of
stable variant names and the current value's name. Generated companion field
types have the same visibility as the source struct and default to
`<Struct>Fields`, with an explicit collision override.

Generated code calls a small `query::__private` runtime ABI. The module is
hidden from ordinary documentation but is compatibility-sensitive because
already-expanded downstream code names it. Core and macro versions are exact,
and compatibility fixtures exercise those signatures; `#[doc(hidden)]` does
not make the boundary private to Rust's linker or type system.

The optional `serde` feature serializes expressions through a private wire
adapter using a versioned tagged grammar with stable field names. Version 1 is
the numeric value `1` delivered by the host serde deserializer. Every signed
and unsigned integer visitor width and an `f64` equal to one are accepted;
accepted values serialize as integer `1`. With `serde_json`, `1`, `1.0`, and
`1e0` are equivalent, and a nearby source such as `1.0000000000000001` can
round to `1.0` before the library sees it. The schema retains exact numeric
`const: 1`, so an exact-decimal validator can reject a source that the host
rounds to one. Other host-decoded integral values are unsupported versions;
non-integral or non-finite floats and nonnumeric values are invalid structure.

The grammar, not Rust method names, is shared with the future TypeScript port.
Unknown versions, fields, operators, quantifiers, and incompatible literal
types are rejected during decoding so `FilterExpr::matches(&T)` remains
infallible. Every object is closed to unknown members. Hosts embed the complete
expression envelope under their own `where` member; `where` is not part of the
expression AST. Any new node, member, or literal representation increments the
schema version.

Duplicate JSON member names are invalid. Rust rejects them while deserializing
the raw text; the future TypeScript and external-input edges must perform the
same duplicate-aware parse before constructing an object because
`JSON.parse` alone collapses duplicates to the last value.

Regular-expression lookups use Rust `regex` syntax deliberately. Python's
`re` syntax includes constructs such as look-around and backreferences that
Rust's linear-time engine rejects. The operator and search semantics remain,
while unsupported pattern syntax is a validated, documented parity-ledger
delta rather than silently changing meaning.

### What an expression can name

An expression names one object's own fields. A `FilterExpr<Pane>` reaches the
`pane_*` catalog and nothing else, so "panes in the session named work" is not
a thing it can say. That is deliberate: the grammar stays per-object so it can
later compile to tmux's own `-f`, which evaluates against one row.

Cross-object questions are asked by narrowing first, which is what tmux does
with a target: `Session::panes` and `Window::panes` choose the rows,
and the expression chooses among them. `tmux-mcp`'s `find_panes` takes both
for exactly this reason.

Relations are how a parent is asked about, and they need a parent that holds
its children. A `Session` handle does not: it fetches its windows. The shape
that does is `Server::hierarchy`'s, so `SessionTree` and `WindowTree` are
`Filterable`, with a `windows` relation and a `panes` relation respectively.
Their targets are `session_tree` and `window_tree`.

Their field companions are hand-written, because libtmux does not use its own
derive. Each keeps the owned handle's fields under a named field -- `session`
and `window` -- and puts the relation beside it, so a session's own fields and
a question about its contents compose in one expression:

```no_run
# fn expression() {
use libtmux::query::Filterable as _;
use libtmux::{SessionTree, WindowTree};

let sessions = SessionTree::filter_fields();
let windows = WindowTree::filter_fields();

let expression = sessions
    .session
    .session_name
    .starts_with("build")
    .and(sessions.windows.any(windows.window.window_name.eq("editor")));
# let _ = expression;
# }
```

Matching delegates: a predicate naming the relation resolves against the
children, and anything else is handed to the owned handle, which already knows
its own catalog. Validation delegates the same way, so an expression naming a
field the inner type does not have is rejected when it is decoded rather than
evaluating to false.

Local `matching()` never pushes down. A later
`server.query_sessions(expression)` family may compile supported predicates
to tmux `-f` before materialization and evaluate the remainder locally. That
slice must prove equivalence with pure local evaluation before exposing a plan
type. The Python `field__operator` syntax is only an edge parser for CLI, MCP,
or configuration input and never becomes the Rust authoring API.

### Object API

Public modules and object names mirror Python. Properties that execute tmux
become async methods; snapshot properties remain ordinary getters. Simple
operations remain methods with ordinary arguments. Operations with several
optional clauses accept a consuming `#[must_use]` options builder through
`impl Into<Options>`.

```no_run
# async fn walk() -> Result<(), Box<dyn std::error::Error>> {
use libtmux::Server;

let server = Server::new()?;
let session = server.new_session("work").await?;
let window = session.new_window("editor").await?;
let Some(pane) = window.active_pane().await? else {
    return Ok(());
};
pane.send_keys("cargo test").await?;
# Ok(())
# }
```

The same method accepts configured options without adding a second operation:

```no_run
# async fn build(server: &libtmux::Server) -> Result<(), libtmux::Error> {
use libtmux::NewSessionOptions;

let options = NewSessionOptions::new("work")
    .window_name("editor")
    .start_directory("workspace");
let session = server.new_session(options).await?;
# let _ = session;
# Ok(())
# }
```

Options and hooks are exposed as inherent methods on each applicable handle,
not extension traits users must import. Private macros may remove repetitive
wrapper code while keeping generated public methods documented and tested.

Python context-manager cleanup maps to `Server::with_session`,
`Session::with_window`, and `Window::with_pane`. Each accepts an async closure
and waits for cleanup after success or error. An owned task completes creation,
arms cleanup before handing the object to the caller, and keeps cleanup running
after cancellation. Cleanup needs the Tokio runtime to remain active; ordinary
cloneable handles remain non-destructive.

If tmux creates an object but the command fails before yielding a decodable
handle, the scope has no identity to target and cannot compensate for it.

### Errors and collapsed collections

The current public `#[non_exhaustive]` `Error` enum covers Foundation server
configuration, command input, process execution, timeouts, shutdown, version
probing, and sanitized source context. The format kernel uses a private
`FormatCodecError`; future discovery converts its safe metadata into the
public domain error surface only when a public operation needs it. Target
disappearance, missing relationships, and unsupported operation capabilities
also remain future variants.

Local iterator cardinality uses the source-less `ExactlyOneError` and
`MultipleItemsError` values directly; it does not pass through `Error`.
Invalid regexes and portable wire data use the focused, source-less
`FilterExpressionError`. Public errors never retain executable arguments,
raw process output, row bytes, snapshot text, regex patterns, or serialized
filter values.

List-shaped object access offers both shapes, and the short name is the loud one. What follows describes the collapsing
contract:

```no_run
# async fn both(server: &libtmux::Server) -> Result<(), libtmux::Error> {
let lenient = server.sessions_or_empty().await;
let loud = server.sessions().await?;
# let _ = (lenient, loud);
# Ok(())
# }
```

The first returns an empty `Vec` when the underlying tmux list operation fails
and records the failure through tracing when enabled. The short form returns
`Result<Vec<_>, Error>` and preserves command, framing, and decoding failures.
This pair applies consistently to hierarchy collections. Expression
construction, future explicit remote query execution, and mutations remain
loud.

### Capabilities and future engines

The first `EngineCapabilities` is an immutable wrapper around the exact
detected tmux version. That is the only capability state consumed by the
one-shot Foundation and the private version-gated format rendering. Transport
bytes remain a `CommandResult` contract; original callback-byte recovery is a
separate version-sensitive format concern. The version is reported by the
configured executable and does not assert the build of a daemon already
listening at the selected endpoint.

The initial crate does not expose Statement, Program, planner, registry,
approval, or retry policy APIs. The public query and command values can later
be embedded into one immutable graph. When prepared physical work lands, it
must bind to the exact capability value and distinguish logical operation and
node identity, the current Core-scoped dispatch-request identity,
executor-internal attempt identity, and physical transport correlation. A
caller retry creates a new dispatch request. A future executor-internal retry
keeps the accepted dispatch-request identity and assigns separate attempt
identity; a subprocess PID or control-mode block number is correlation evidence
for an attempt, not the public request ID. Result shape must match the
executor's proven correlation granularity.

A future control-mode engine must issue independent requests separately from
semicolon chains, keep draining protocol frames after caller cancellation,
and give each logical node exactly one terminal state: complete, failed,
skipped, or unknown. It may report per-command states only when retained
protocol evidence supports that attribution.

## Control mode

The `control-mode` feature opens one tmux connection and keeps it. A task owns
the pipes; callers hold a `ControlSender` and a `ControlEvents`, which is a
`Stream`.

That task is an actor, and this document previously argued against one on the
grounds that a caller-driven connection buffers and drops nothing out of sight.
The argument was backwards. Control mode exists to act on what the server
reports, and a single object holding both directions needs `&mut` for each, so
a task awaiting an event cannot send the command that event implies. One task
multiplexing the connection is what makes the feature usable at all.

What the earlier framing was right about is kept:

- events are handed over with backpressure, never dropped -- a consumer that
  stops reading stops the connection reading from tmux, which is the
  backpressure tmux already applies to a slow client. That pause is available
  only while nothing is waiting for a reply. A reply arrives on the connection
  the events arrive on, so pausing with one outstanding would be waiting for
  something this end had stopped listening for, and it deadlocked: measured
  identically on 3.2a through 3.7c, with no error and `is_closed` reporting
  false. While a reply is outstanding the connection keeps reading and holds
  a fixed maximum of what the consumer has not taken. At that ceiling it
  refuses live replies but retains their correlation slots; an absolute reply
  deadline bounds the write through the complete block. Pausing resumes the
  moment no live reply remains;
- the connection lives while either handle is in use and ends when both are
  gone, so a caller who only watches and a caller who only sends are both
  ordinary;
- `attach` returns only once tmux has the client attached, so a change made
  immediately afterwards is reported rather than racing the attach.

Correlation is by arrival order, which is sound because tmux answers commands
in order and blocks do not nest. The block number tmux assigns is carried on
the result rather than used to match, because a command that fails early can
leave a caller waiting for a block tmux will never send.

The protocol is parsed as bytes. tmux escapes only what would break the line
protocol -- bytes below `0x20`, and backslash -- so `%output` carries a pane's
bytes literally and a line is not necessarily UTF-8.

## Module and component map

This table spans implemented and planned components. A listed target file is
not a current public module or export until its owning roadmap slice closes.

| Component       | Responsibility                                         | Primary Rust files                      | Python source baseline                    |
| --------------- | ------------------------------------------------------ | --------------------------------------- | ----------------------------------------- |
| Crate facade    | Re-exports, feature gates, package docs                | `src/lib.rs`                            | `src/libtmux/__init__.py`                 |
| Commands        | Redacted arguments, grouping, raw results              | `src/command.rs`                        | `src/libtmux/common.py`                   |
| Versions        | Tmux releases, minimum checks, capabilities            | `src/version.rs`, `src/capabilities.rs` | `src/libtmux/common.py`                   |
| Errors          | Public Foundation/query errors; future domain errors   | `src/error.rs`, `src/query.rs`          | `src/libtmux/exc.py`                      |
| Constants       | Scopes, directions, flags, compatibility constants     | `src/constants.rs`                      | `src/libtmux/constants.py`                |
| Formats         | Typed format descriptors and row parsing               | `src/formats.rs`                        | `src/libtmux/formats.py`, `neo.py`        |
| Snapshots       | Typed object information and winlink projections       | `src/snapshot.rs`                       | `src/libtmux/neo.py`                      |
| Targets         | Validated IDs, typed targets, winlink edges            | `src/target.rs`                         | object modules and `neo.py`               |
| Queries         | Iterator extensions, typed expression AST, cardinality | `src/query.rs`                          | `_internal/query_list.py`, filtering docs |
| Runtime core    | Configuration, capabilities, private executor          | `src/internal/core.rs`                  | command and fetch paths                   |
| Tokio transport | Spawn, capture, timeout, cancellation, reaping         | `src/internal/subprocess.rs`            | `tmux_cmd`                                |
| Server          | Connection and server-wide operations                  | `src/server.rs`                         | `src/libtmux/server.py`                   |
| Session         | Session traversal and operations                       | `src/session.rs`                        | `src/libtmux/session.py`                  |
| Window          | Winlink-aware traversal and operations                 | `src/window.rs`                         | `src/libtmux/window.py`                   |
| Pane            | Pane I/O, layout, popup, mode, and movement operations | `src/pane.rs`                           | `src/libtmux/pane.py`                     |
| Client          | Attached client snapshots and traversal                | `src/client.rs`                         | `src/libtmux/client.py`                   |
| Options         | Typed option scopes, parsing, get/set/unset            | `src/options.rs`                        | `src/libtmux/options.py`                  |
| Hooks           | Hook values and get/set/unset/run operations           | `src/hooks.rs`                          | `src/libtmux/hooks.py`                    |
| Test support    | Isolated sockets, names, retries, temporary objects    | `src/test.rs`, `tests/support/`         | `pytest_plugin.py`, `src/libtmux/test/`   |
| Parity ledger   | Public surface mapping and intentional deltas          | `docs/parity.md`                        | API reference and tests                   |
| Tooling         | Cargo metadata, just recipes, format/lint/CI gates     | crate configuration                     | `pyproject.toml`, workflows               |

The public file names stay close to Python. Runtime-only implementation files
live under private `internal`; no `_internal` module is exported.

## Test architecture

The optional `test-support` feature exports `libtmux::test::TestServer` for
downstream crates. Repository integration tests use the same implementation.

Each guard:

- creates a unique short temporary directory and explicit `-S` socket path;
- exposes the exact socket path and a configured `Server`;
- starts tmux with `-D`, a controlled config, and command-local environment,
  isolates its process group, and retains the foreground daemon child;
- configures every command through the exposed `Server` with tmux's global
  no-start flag so it cannot bootstrap an unowned replacement daemon;
- provides consuming async shutdown that awaits the owned daemon;
- caps graceful observation at five seconds on targets without a safe
  non-reaping child-exit observer, then forces group cleanup and waits;
- reobserves child ownership before each numeric signal phase and permanently
  retires numeric PID, process-group, `Child` signal, and `Child` wait
  operations after `ECHILD` or another untrusted observation failure;
- performs synchronous forced best-effort cleanup in `Drop` without depending
  on a Tokio runtime or claiming that an unreportable failure succeeded;
- retains an opened owned-directory descriptor and removes only the fixed
  socket, configuration, and lock basenames with descriptor-relative cleanup;
- disables lexical recursive temporary-directory cleanup and removes the
  original directory entry through its retained parent descriptor;
- never launches a path-based cleanup client after exposing the socket path.

On Linux, process-group cleanup is followed by an exact-marker sweep. A process
is admitted only when its real UID matches the guard, its live non-zombie
`(pid, uid, start time)` identity is readable, its NUL-delimited environment
contains the exact marker entry, a pidfd opens, and both identity and marker
survive revalidation. Every admitted process is frozen and signaled through
that pidfd. The admitted-pidfd collection owns a best-effort `SIGKILL` fallback:
a later scan, timeout, revalidation, or signal error kills every process already
frozen before the sweep returns failure. The root remains in place because a
best-effort signal is not proof that every target became terminal.

The Linux marker is guard-selection metadata, not authentication or ancestry
proof. A same-UID process that copies the marker is admitted. A genuine
descendant that clears the marker or makes it unreadable before initial
admission is outside the sweep. Initial opacity is skipped because it cannot be
distinguished from another guard's process; opacity after an initial identity
and marker match fails the sweep.

On other Unix targets, successful cleanup proves only process-group signaling
and direct-child waiting. It does not contain descendants that detach with
`setsid` or otherwise leave that group. On every target, observing lost child
ownership makes lifecycle cleanup fail closed: no later numeric process or
group signal and no later `Child` signal or wait is issued, fixed files are
retained, and consuming startup or shutdown reports `ShutdownFailed`. Linux
still attempts its pidfd marker sweep. A successful direct-child wait likewise
permanently retires numeric signaling, so a later containment failure cannot
rearm a stale PID or process group.

The non-reaping observation and the following numeric signal are not one
atomic operation. An uncoordinated external waiter can still reap the leader
between them; this design detects ownership loss at the next observation but
cannot close that final race portably. It also cannot synchronously report a
failure from `Drop` or prove termination of a process stuck indefinitely in
the kernel. Stronger Linux isolation would require an explicit facility such
as a cgroup, PID namespace, or subreaper rather than a stronger claim about the
marker.

Tests use observable polling with deadlines, never fixed sleeps. Later parser
and query tests use hand-written byte fixtures. Behavioral tests execute real
tmux. Foundation transport tests cover timeout cancellation, child reaping, invalid
UTF-8, non-zero status with stdout, stderr without failure, absent daemons,
task abort during a command, cancellation during scoped shutdown, redaction on
every diagnostic surface, daemon PID disappearance, and cleanup after panics.

The Foundation workflow runs the pinned tmux 3.2a floor on Linux. The format
slice adds a dedicated Linux harness that builds pinned 3.2a and 3.6 commits,
supplies each binary directly to `TestServer`, and exercises both sides of the
3.6 linked-window callback transition. An unpinned distro tmux cannot
substitute for either endpoint. The harness also builds pinned 3.4 and 3.5a,
the two releases that visually encoded command output, so the `Vis` dialect is
exercised in CI rather than only on a developer's own machine.

The adversarial transport fixture sets one server option to a value carrying a
field terminator, a row terminator, a `#{q:}` special, a multibyte character,
and two bytes that are invalid UTF-8. It then asserts the exact stdout its
dialect emits and decodes that live transport back to the original bytes. It
runs on both sides of both boundaries: 3.4 and 3.5a on the `Vis` lane, 3.6 and
current 3.7b on the `RawQ` lane. A unit fixture additionally round-trips every
nonzero byte value through the `Vis` grammar, and mismatched dialect pairings
assert a loud framing error rather than altered bytes. The release
compatibility matrix will add maintained
tmux releases on Linux and the latest stable release on macOS. Tests for newer
flags assert both supported behavior and the version-gated error or warning
path when their owning slices land.

A parity ledger maps every pinned-baseline Python public method and property to
its Rust method, intentional syntax delta, tmux version gate, and evidence.
The crate is not feature-complete while any baseline capability remains
`planned` or `in progress`; reviewed omissions are recorded as `excluded`.

## Lint and dependency gates

`clippy.toml` carries what the lint levels alone cannot express. `expect`,
`unwrap`, and `panic` are denied workspace-wide, because a library returns
errors rather than deciding to end the caller's process -- but test code
asserts, so the three `allow-*-in-tests` settings exempt it without every
test file repeating an allow attribute. Files with helpers outside a test
function still need one, since clippy's exemption follows test functions
rather than file paths.

`await_holding_lock` and `await_holding_invalid_type` are denied because the
whole public surface is async and the transport supervisor holds a
`std::sync::Mutex`. Nothing holds a guard across an await today; the lints
keep it that way.

`cargo semver-checks` compares the public API against the previous release
and reports which lints a version bump would violate. It is a release gate
rather than a per-PR one, for two reasons. It needs a published baseline, and
until one exists it has to be pointed at a git revision with
`--baseline-rev`. And every version transition inside `0.x.y-prerelease` space
counts as major, which permits everything -- a run today reports `0 checks,
254 skip`. Forcing the comparison with `--release-type minor` across the
commit that closed the hierarchy shapes reports all six correctly, so the tool
does understand this crate; it simply has nothing to say until there is a
release to compare against. There is no `semver` recipe while the crates are
prerelease: `cargo-semver-checks` treats a prerelease-to-prerelease step as a
major change and skips every lint, so it reported `0 checks` and then `no
semver update required`, which reads like a clean bill of health and is not
one.

`deny.toml` gates the dependency tree through `cargo deny`: an allowlist of
the permissive licences the tree actually carries, denial of yanked crates
and unknown registries, and a wildcard ban that exempts intra-workspace path
dependencies. Those carry a version as well as a path, because every crate
here publishes: a path-only dependency cannot be published at all.

## Compatibility lanes

`scripts/test-tmux-format-compat.sh` builds each pinned tmux from source and
runs the whole workspace against it, all targets and all features. The lanes
are 3.2a, 3.4, 3.5a, 3.6b, and 3.7b: the floor, the ceiling, and the releases
in between that are known to differ. 3.4 and 3.5a are the two that wrapped
command output in `VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`, and are the reason the
codec has a second dialect at all.

Running only the codec tests on the middle lanes was tempting and wrong. The
crate's version sensitivity is not confined to the codec: command flags,
control mode, and the stderr wording that separates a missing target from a
refusal all vary by release in principle. Asserting them against one tmux
proves nothing about the others, and the first full run of the floor lane
found a real difference -- `new-session -x -y` sets `default-size` on every
release, but 3.2a still draws a client-less window at 80x23 where 3.6 uses
the default. The crate behaves identically on both; only tmux's rendering
differs, which is why the test asserts the option rather than the geometry.

A ceiling lane matters as much as the floor. Without one, the newest tmux --
the version most people actually run -- would be covered only by whatever the
runner image happens to package, which lags releases by a long way.

## What downstream can construct

Handles and snapshots keep private fields and expose accessors, so adding a
field is not a breaking change. The two shapes `Server::hierarchy` returns are
the exception, because a caller reads `branch.session` and `branch.windows`
directly and an accessor would only be in the way. They are `#[non_exhaustive]`
instead: field access stays, construction and exhaustive destructuring do not,
and a later `clients` relation costs nobody a major version.

`Chooser` and `PaneProgressState` are `#[non_exhaustive]` for the same reason
in the other direction: both enumerate something tmux owns and can extend, so
a downstream `match` needs a `_` arm.

The geometric enums -- `SplitDirection`, `ResizeDirection`, `WindowPlacement`,
`PaneSize` -- are left open deliberately. Their variants are complete by
construction, and exhaustive matching over them is worth more than room to grow
that will not be used.

## Classifying a failure

`Error` has a variant per failure mode; `Error::kind` reduces those to the
decisions a caller actually makes, in the shape of `std::io::Error::kind`.
`ObjectGone`, `Refused`, `Timeout`, `Unreachable`, `UnsupportedVersion`,
`InvalidInput`, `Transport`, and `Decode` each imply a different next step.

Separating `ObjectGone` from `Refused` needs tmux's stderr. tmux exits 1 both
for a target it cannot find and for an argument it does not like, and reports
the first as `can't find <kind>: <target>`. That message is not localized --
tmux has no message catalogue -- and `cmd-find.c` has carried the same four
wordings, plus `no current target`, unchanged from 3.2 through the current
development branch.

Stability of someone else's strings is not something to take on faith, so
`real_tmux_compat_error_missing_target_wording_is_recognized` asserts the
classification against whichever tmux is running, and every compatibility
lane requires that test family to exist. A release that rewords these fails
there rather than silently returning the wrong answer from
`Error::is_object_gone`. A wording the crate does not recognize stays a
refusal, so the cost is the distinction rather than correctness.

One case needs more than the message. A server holding no sessions reports
`no current target` for any command needing one, including `-a` listings that
ask for everything. What that means depends on the request: a server-wide
listing has nothing to list, while a listing or mutation under a target could
not resolve it. The scope, and the request's own `-t`, are what tell them
apart.

How much a miss proves depends on what tmux echoes back, and the echo is
self-describing. tmux returns the part of the target it could not resolve, so
an identity comes back carrying its sigil and a coordinate comes back without
the session it belonged to:

| sent | echoed | what it establishes |
| --- | --- | --- |
| `-t home:@99` | `can't find window: @99` | no window `@99` is reachable |
| `-t home:9` | `can't find window: 9` | `home` holds nothing at index 9 |
| `-t home:nosuch` | `can't find window: nosuch` | `home` holds no window of that name |
| `-t nosuch` | `can't find session: nosuch` | no session of that name |

A coordinate is scoped to one session and is not unique on the server, so its
absence never establishes that an object died. Reading one as an identity
reported index 3 as window `@3` -- a different object, often a live one -- and
`Error::is_object_gone` answered `true`, which is the single predicate a caller
consults before discarding a handle. Those misses are `Error::LinkGone`, which
answers `false`. A session is the exception, because `-t` takes a session's
name and tmux keeps those unique, so a bare word there is still an identity.

The sigil does not settle everything. `unlink-window -t home:@3` answers
`can't find window: @3` whether `@3` is dead or merely linked into some other
session, so the two produce the same string and no reading of it can separate
them. `Window::unlink` and `Window::swap_with` buy that distinction with a
lookup, on the failure path only and only where the answer changes what a
caller should do. They ask the server rather than refreshing the handle: a
window refreshes within its own session, and a window whose link to that
session is gone is the case being told apart, so refreshing would answer the
question with its own premise.

`real_tmux_compat_a_coordinate_miss_is_not_an_object_miss` pins both halves
against whichever tmux the lane runs, for the same reason the wording test
exists, and has passed on every one: 3.2a, 3.4, 3.5a, 3.6b and 3.7b. The split
needs no version gate, and 3.2a -- the oldest supported, and the release most
likely to answer differently -- echoes exactly as the current one does. If a future release echoed the whole target, or dropped the sigil, the
unrecognized form classifies as `LinkGone` -- the reading that does not license
discarding a live handle -- so the cost of being wrong is a distinction rather
than a destroyed handle.

Listing accessors come in pairs, and the split is load-bearing here. The
`*_or_empty` form returns an empty `Vec` for any failure, which suits a status
line. The short form propagates, which is the whole reason it exists -- a
short form that quietly returned no rows for an unreachable daemon would make
the pair meaningless.

## Cost of gathering the hierarchy

`Server::hierarchy` issues three tmux commands whatever the server holds --
`list-sessions`, `list-windows -a`, `list-panes -a`, run concurrently -- and
stitches the rows locally. Walking down instead costs one command per session
and one per window, which is the shape a caller reaches for first.

`tests/command_budget.rs` asserts the counts by observing what the crate
actually ran, so a change that reintroduces per-object commands fails rather
than merely getting slower. `benches/hierarchy.rs` reports the time, measured
on one developer machine against tmux 3.7b:

| server | `hierarchy()` | walking down | ratio |
| --- | --- | --- | --- |
| 1 session, 1 window, 2 panes | 6.7 ms | 22.3 ms | 3.3x |
| 2 sessions, 8 windows, 16 panes | 15.0 ms | 78.1 ms | 5.2x |
| 4 sessions, 32 windows, 64 panes | 26.6 ms | 282 ms | 10.6x |

The gathered column is not flat, and saying it is would be wrong: the command
count is constant, but tmux still has to produce and the crate still has to
parse a row per object. What is constant is the per-object process spawn, which
is what dominates the walking column.

Stitching the three listings groups by the numeric part of each tmux ID, which
is `Copy`, so joining them allocates nothing per object.

## Toolchain and packaging

MSRV is a promise about the published crates. `libtmux` and `libtmux-macros`
build and test on 1.85. The consumer crates, `tmux-mcp` and `tmux-workspace`,
exist to exercise the public API from outside. `tmux-mcp` carries its own MSRV
of 1.88, because its dependencies need a compiler the libraries do not, and is
tested on its own lane. All four crates publish; being published is part of
the consumers' job rather than beside it, since a crate built only inside its
own workspace never proves its dependency requirements resolve.

The consumers also prove the dependency edge runs one way: `libtmux` names
neither of them, and `cargo tree -p libtmux` lists only caseless, regex,
rustix, thiserror, and tokio.

A declared dependency version is the minimum this crate supports, not the
newest published one. Cargo's version-aware resolver picks the newest release
each toolchain can build, so a floor below the latest is the normal state
rather than staleness. A floor is raised only for a fix the crate needs, and
never above a version whose own `rust-version` exceeds the MSRV -- which is
what rules out criterion 0.8 and trybuild 1.0.120 today.

The crate uses Edition 2024, Cargo resolver 3, `rust-version = "1.85"`, and
the repository's MIT license. `rust-toolchain.toml` pins the development
compiler and installs Rustfmt, Clippy, and rust-src. Rustfmt uses stable
settings only.

The Foundation dependency set stays narrow:

- Tokio with only process, I/O, time, synchronization, runtime, and macro
  features;
- thiserror for the public error enum;
- optional tracing instrumentation without subscriber configuration;
- optional tempfile support for the public test guard.

The query slice adds regex support plus independent optional `serde` and
`derive` features. The derive macro lives in the sibling
`libtmux-macros` proc-macro package; normal snapshot querying does not compile
it. Current Cargo verifies the unpublished workspace packages in dependency
order, while Rust 1.85 remains a build-and-test gate rather than a packaging
toolchain.

The manifest includes complete description, repository, documentation,
readme, license, keywords, categories, docs.rs feature metadata, and an
explicit include list. `Cargo.lock` is committed for repository CI
reproducibility.

Rust lints forbid unsafe code and warn on missing docs, unreachable public
items, unused lifetimes, and unused qualifications. Clippy enables `all` and
`pedantic` with narrow documented exceptions; unwrap, expect, and panic are
denied in library code and permitted in tests. CI promotes warnings to errors.

The justfile recipes call the same direct Cargo gates documented for Rust
users, and `just` alone lists them:

```console
$ just check
```

The aggregate gate runs formatting, Clippy, all-target/all-feature tests,
doctests, documentation with warnings denied, the no-default-features build,
bounded feature-powerset checks, and the MSRV lane. `cargo hack` verifies each
public feature independently and the supported combinations on both the
development toolchain and MSRV. Dependency audit and semver checks remain part
of the final compatibility slice; they are not required to build the crate.

## Delivery sequence

The work is split into independently reviewable subprojects:

1. Crate foundation, raw transport, errors, IDs, capabilities, and real-tmux
   test guard.
2. Format catalog, typed snapshots, parsing, winlinks, native iterator
   extensions, portable filters, and downstream derive support.
3. Server, Session, Window, Pane, and Client discovery, traversal, refresh,
   and environment resolution.
4. Full mutation and interaction parity across the object hierarchy.
5. Options, hooks, keys, buffers, menus, prompts, popups, and control-related
   command families.
6. Documentation, examples, compatibility matrix, parity closure, fuzzing,
   semver checks, and final adversarial review.

Each subproject begins from failing behavior tests, ends with the complete
crate gate, and receives an independent API and Rust-quality review. Spike
code is never promoted; implementation is written afresh from the approved
contracts.

## Acceptance criteria

The crate is complete only when:

- the parity ledger accounts for every pinned-baseline Python public
  capability;
- all intentional differences are documented as Rust syntax or verified bug
  corrections rather than missing behavior;
- the full Rust format, Clippy, test, doctest, docs, feature, and MSRV gates
  pass;
- real-tmux tests pass on tmux 3.2a and the maintained compatibility matrix;
- socket isolation and cleanup pass under parallel tests and panic paths;
- timeout, dropped-future, and task-abort tests prove process-group termination
  and direct-child reaping;
- portable filter serialization matches the versioned grammar and local
  evaluation remains the reference semantics for any later pushdown;
- server-wide linked-window and pane enumeration preserves every winlink;
- an adversarial Rust API review has no unresolved critical or important
  findings;
- no shipped file contains spike code, local paths, private data, unstable
  source links, AI signatures, or unowned scaffolding.

### An option refusal has three answers, not four

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

### The server and session environments are two stores, merged late

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
told from the next variable, so every name is read back on its own.

### `run-shell` output goes nowhere on tmux 3.3 through 3.4

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

### Muting a control-mode pane kills the server before tmux 3.7

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

### Two shapes that make a test flaky under load

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

### An idle fixture process must idle the right way

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

### A client's attachment is a name, so it is read as an id instead

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

### `list-clients` collapses three ways of being absent

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

### `display-message` answers about a pane you did not ask for

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

### A socket path does not identify a tmux server

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

### Budgets, because tmux is an unbounded producer

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

### Control mode needs its own budgets

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

### Which tmux releases the lanes build

The final patch of each series rather than its first: 3.2a, 3.5a, 3.6b, and
3.7b are what a distribution ships and what a user runs. 3.4 is the exception,
because that series has no later patch and is one of the two releases that
wrapped command output in `VIS_OCTAL|VIS_CSTYLE|VIS_NOSLASH`.

The lane was `3.6` and is now `3.6b` for that reason. Note that `3.6` still
appears in the source as a *behaviour boundary* -- the dialect restore landed
in 3.6 itself -- which is a different statement from which build CI runs.

### macOS is tested, not assumed

The platform contract names macOS, and for a long time the evidence was
Linux-only. What differs there is exactly what this crate leans on: process
groups, Unix sockets, `waitid`, and temporary paths. The lane runs the test
suite rather than the whole gate, and on master rather than every push,
because a macOS runner bills at ten times a Linux one while the lints it would
re-run are platform-independent.

### What the first macOS lane found

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

### The short name belongs to the honest form

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
the old behaviour writes `sessions_or_empty()`, which says what it does. The
breaking change is cheap now and would not be later, which is the argument for
doing it during an alpha rather than after one.

### Fuzzing the parsers that read from outside

Three surfaces take bytes this crate did not write, and each is fuzzed:

- the control-mode line parser, which reads from a tmux that keeps running, so
  a malformed line is not a command that failed but bytes it has to survive;
- the versioned filter-expression wire format, which can arrive from a config
  file, a CLI argument, or an MCP tool call;
- the tmuxp-style workspace loader, which walks a hand-written nested document
  deciding what each value means.

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

`__fuzz_parse_control_line` exists because the parser is private and should
stay private. It is behind `unstable-fuzzing`, which is not in `full` and
which nothing but `fuzz/` turns on.

CI runs them weekly rather than per-push. This kind of testing finds things by
running for a long time, so a schedule is worth more than a gate nobody can
wait for, and a crash is uploaded as an artifact rather than left in a log.

### The public surface is recorded, because nothing else reports drift

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

### The MCP server bounds the tmux side, not just its own answers

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

### Example coverage is measured, because the gap is invisible

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

### Waiting was missing from the production surface

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

Both numbers that would justify a doorbell have now been taken, and neither
argues for one.

Latency is a capture round-trip rather than a fraction of the poll interval,
because the loop looks before it sleeps: twelve waits for a marker printed into
a pane answered in 12ms at the fastest, 22ms median, 31ms at the slowest,
measured from dispatching the key that produces the text. A doorbell removes
the round trip, not an interval, so it is worth tens of milliseconds rather
than the hundreds the interval suggests.

A flood is where the two paths diverge, and not in the doorbell's favour.
`seq 1 200000` into a pane, waiting for the last line: 460ms, found, nothing
lost. Polling costs one capture per interval whatever the pane is doing, so a
flood does not reach it. A doorbell rings per notification, which is where the
Swift port's coalescing comes from -- machinery this path does not need
because it does not have the problem.

So the doorbell stays unbuilt, and this is the reason rather than the absence
of one. It buys tens of milliseconds and brings a failure mode the floor does
not have.

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
rather than an alternative to it. What remains to measure is what the doorbell
saves, and what a flood does to a wait that rings on every byte.
