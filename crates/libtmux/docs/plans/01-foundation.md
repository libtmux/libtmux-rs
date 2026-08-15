# Crate foundation implementation plan

## Goal

Create the publishable Rust crate foundation, typed command and connection
values, supervised Tokio subprocess transport, minimal raw `Server` API, and
isolated real-tmux test support described in the approved design.

This slice deliberately stops before format-row parsing, hierarchy discovery,
`QueryList`, and object mutations. It must make those later layers possible
without exposing an executor generic or a second semantic command model.

## Fixed decisions

- Package name: `libtmux`
- Initial crate version: `0.1.0`
- Edition: 2024
- MSRV: Rust 1.85
- Pinned development compiler: Rust 1.97.1
- Runtime: Tokio
- Minimum tmux: 3.2a
- Platforms: Linux, macOS, and WSL
- Default features: none
- Optional foundation features: `tracing`, `test-support`
- Cargo owns builds and publication; the justfile only invokes Cargo commands
- No commit, tag, push, or publication is part of this plan

The initial dependency requirements are compatible with the MSRV and locked by
`Cargo.lock`:

- `tokio = "1.53.1"`
- `thiserror = "2.0.20"`
- `rustix = "1.1.4"` with process support
- optional `tracing = "0.1.44"`
- optional and development-only `tempfile = "3.27.0"`
- development-only `static_assertions = "1.1.0"`
- development-only `tracing-subscriber = "0.3.23"`

Before implementation, resolve the manifest once and inspect the selected
packages' `rust-version` metadata. Resolver 3 must not substitute for an actual
Rust 1.85 build.

## Execution discipline

For each behavior task:

1. Add the smallest public-facing or internal test that proves the next
   contract.
2. Run only that test and record the expected failure.
3. Implement the smallest production surface that makes it pass.
4. Run the focused test again.
5. Run formatting and Clippy on the touched targets.
6. Run the complete foundation gate before moving to the next task family.

Do not copy code from disposable spikes. Do not add a public symbol unless its
current slice has a caller, documentation, and an executable example.

## Task 1: Create the Cargo and just shell

Files:

- Create `rs/Cargo.toml`
- Create `rs/Cargo.lock`
- Create `rs/LICENSE`
- Create `rs/rust-toolchain.toml`
- Create `rs/rustfmt.toml`
- Create `rs/justfile`
- Create `rs/README.md`
- Create `rs/src/lib.rs`
- Create `rs/docs/parity.md`
- Create `.github/workflows/rust.yml`

Create a library-only manifest with `resolver = "3"`, `rust-version = "1.85"`,
complete crates.io metadata, an explicit package include list, and no default
features. Tokio's normal dependency enables only `io-util`, `process`, `rt`,
`sync`, and `time`; the development dependency additionally enables `macros`
and `rt-multi-thread`.

Configure Rust lints in the manifest:

```toml
[lints.rust]
missing_docs = "warn"
unreachable_pub = "warn"
unsafe_code = "forbid"
unused_lifetimes = "warn"
unused_qualifications = "warn"

[lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
expect_used = "deny"
panic = "deny"
unwrap_used = "deny"
```

Use narrow crate- or item-level exceptions only after Clippy reports a concrete
false positive. Test targets may explicitly allow `expect_used`, `panic`, and
`unwrap_used` at their roots.

The `justfile` groups the recipes so `just` alone lists them: `test`, `lint`,
`docs`, `bench`, `release`, and an aggregate `check`. Each recipe invokes
Cargo directly; there is no build step and no binding target.

Seed `docs/parity.md` from the complete pinned Python API inventory before
implementing public Rust behavior. Every row records the delivery slice and
starts as `planned`; a row becomes implemented only with its Rust symbol and
test evidence.

Add a Rust workflow with separate development-toolchain, Rust 1.85, and tmux
3.2a jobs. The minimum-tmux job builds the pinned 3.2a tag, places that binary
first on `PATH`, asserts `tmux -V`, and runs all real-tmux foundation tests. The
workflow is required evidence; the installed local tmux 3.7b cannot establish
the compatibility floor.

Add crate-level documentation explaining the async-only API, tmux 3.2a floor,
raw-byte result boundary, and feature flags. Add a platform compile error for
non-Unix targets because native Windows has no tmux transport.

Resolve and verify the empty crate:

```console
$ cargo check --locked --manifest-path rs/Cargo.toml
```

Inspect the dependency graph and selected MSRVs:

```console
$ cargo metadata --locked --manifest-path rs/Cargo.toml --format-version 1
```

Verify package contents before later source files expand the include list:

```console
$ cargo package --locked --manifest-path rs/Cargo.toml --allow-dirty --list
```

Expected result: the crate builds without warnings, contains no binary target,
and the runner is absent from Cargo metadata.

## Task 2: Implement tmux-specific versions and foundation errors

Files:

- Create `rs/src/error.rs`
- Create `rs/src/version.rs`
- Create `rs/tests/version.rs`
- Modify `rs/src/lib.rs`

Start with table-driven tests for raw `tmux -V` output:

```rust
#[test]
fn parses_release_suffixes_without_losing_order() {
    let rc = TmuxVersion::parse_output(b"tmux 3.7-rc\n").unwrap();
    let rc2 = TmuxVersion::parse_output(b"tmux 3.7-rc2\n").unwrap();
    let release = TmuxVersion::parse_output(b"tmux 3.7\n").unwrap();
    let patch = TmuxVersion::parse_output(b"tmux 3.7a\n").unwrap();

    assert!(rc < rc2);
    assert!(rc < release);
    assert!(release < patch);
}
```

Add cases for `1.10`, `3.2`, `3.2a`, `3.1c`, `3.7b`, `3.7-rc2`, `master`,
and `next-3.3`. Cover a renamed tmux 3.2a executable because supported tmux
versions used `getprogname()` as the output prefix. Reject missing or extra
tokens, extra lines, invalid UTF-8, controls, numeric overflow, uppercase or
repeated suffixes, release candidate zero, noncanonical leading zeroes, missing
newlines, CRLF, and trailing garbage. The minimum-version test must prove that
3.2 is below 3.2a, 3.2a meets the floor, `next-3.2` does not meet the floor,
and `next-3.3` does.

The framing contract follows tmux 3.2a's pinned
[`-V` implementation](https://github.com/tmux/tmux/blob/3.2a/tmux.c#L387-L389).
Numbered release-candidate support follows the upstream
[`3.7-rc2` version bump](https://github.com/tmux/tmux/blob/aa1f065/configure.ac#L1-L4).

Run the test before creating the modules:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --test version
```

Expected failure: unresolved imports for `TmuxVersion` and version errors.

Implement these public values:

```rust
pub struct TmuxVersion {
    raw: Box<str>,
    release: Option<ReleaseVersion>,
}

pub struct ReleaseVersion {
    major: u16,
    minor: u16,
    suffix: ReleaseSuffix,
}
```

`ReleaseSuffix` orders unnumbered and numbered release candidates before the
final release and patch letters after it. Treat unnumbered `-rc` as the first
candidate, accept explicit `-rc1`, reject `-rc0`, and preserve the raw version
token separately from canonical structured display. Development identifiers
preserve their raw value and never compare as an invented numeric release.
`master` is conservatively capable only of the minimum release. A `next-X.Y`
identifier meets the minimum only when its target release line is newer than
3.2; development identifiers satisfy no requirement above the minimum.
`TmuxVersion::meets(&ReleaseVersion)` handles capability checks explicitly.

Start a `#[non_exhaustive] Error` with only the version-parse and
unsupported-version variants reached in this task. Invalid-version errors may
retain a non-sensitive reason and byte count, but never the raw process output;
the parsing caller already owns the input. Later tasks add validation, UTF-8
view, builder, spawn, executable-not-found, wait, timeout, and shutdown variants
only when their context types and callers exist. Every process-related variant
added later contains request identity and a sanitized command summary, never
executable argv or raw output.

Document and doctest every public constructor and getter. Re-run the focused
test, then:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --doc
```

Expected result: suffix ordering, minimum enforcement, malformed-input errors,
and public examples pass without warnings.

## Task 3: Implement typed IDs, targets, and server identity

Files:

- Create `rs/src/target.rs`
- Create `rs/tests/target.rs`
- Modify `rs/src/error.rs`
- Modify `rs/src/lib.rs`

Write failing tests for `SessionId`, `WindowId`, and `PaneId`. Each accepts its
own tmux sigil followed by at least one ASCII digit in tmux's `u32` range and
rejects empty values, wrong scopes, signs, whitespace, overflow, and trailing
text. Canonicalize leading zeroes so alternate spellings of one native ID have
the same equality and hash identity, and order IDs by their numeric value rather
than their rendered string. This matches tmux 3.2a's bounded native
[`u_int` ID parsing](https://github.com/tmux/tmux/blob/3.2a/session.c#L85-L96).
Add compile-fail doctests showing that a
`PaneTarget` cannot be passed where a `WindowTarget` is needed.

Write structural identity tests before implementing `ServerIdentity`:

```rust
#[test]
fn normalized_socket_paths_define_server_identity() {
    let left = capture_endpoint("relative/socket", "/tmp/work").unwrap();
    let right = capture_endpoint("/tmp/work/relative/socket", "/unused").unwrap();

    assert_eq!(left, right);
    assert_eq!(hash(left), hash(right));
}
```

Keep this normalization test beside the private helper; use a local hash helper
and do not add either helper to the public API. Add cases proving distinct
explicit paths remain distinct and that named/default sockets include the
resolved socket root. Cover an existing, symlinked, relative, missing, and empty
`TMUX_TMPDIR`; tmux 3.2a resolves an existing candidate with `realpath` and
otherwise falls back to `/tmp`. Cover a valid `TMUX` triple, a comma inside its
socket path, and malformed environment values. Tests inject the working
directory, socket-root candidate, real user ID, and environment through private
helpers rather than mutating process-global state.

Run the target test in the red state:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --test target
```

Implement scope-specific target enums and ID newtypes. Keep their inner values
private. IDs implement `Display`, `FromStr`, `AsRef<str>`, `Eq`, numeric `Ord`,
and `Hash`. In this slice, each `#[non_exhaustive]` target contains only its ID
variant and implements `From<Id>`, `Clone`, `Debug`, `Eq`, `Ord`, and `Hash`.
Defer target `Display`, `FromStr`, `AsRef<str>`, and argv lowering until the
complete name, index, and contextual variants have consuming operations.
`WindowTarget::Current`, for example, lowers to command omission rather than a
round-trippable borrowed token. Return a dedicated sanitized `IdParseError`
from ID parsing rather than the crate's broad operational `Error`.

`ServerIdentity` stores a captured absolute socket endpoint. The resolver joins
relative explicit paths to the builder's captured working directory without
requiring the socket to exist. It preserves symlink-sensitive `..` components,
trailing separators, and leading double separators. Equality and hashing use
the captured path's raw Unix bytes rather than `Path` component equality, and
dispatch uses those same bytes, so equality never claims two potentially
different explicit endpoints are identical. Named and default
endpoints follow tmux 3.2a's `-L` behavior: resolve an existing
`TMUX_TMPDIR` candidate through `realpath`, fall back to `/tmp` when it is
unset, empty, or unresolvable, then append
`tmux-<real-user-id>/<socket-name>`. The exact root selection follows the
pinned [`make_label` path](https://github.com/tmux/tmux/blob/3.2a/tmux.c#L142-L224).
Obtain the real user ID through safe
`rustix` APIs. Reject a named socket that is not one nonempty normal path
component so `-L` dispatch cannot escape or disagree with its captured
identity. Named-root resolution may canonicalize symlinks because tmux does;
explicit socket paths must not. Never use `Arc` pointer identity. When no
selector is explicit, a valid inherited `TMUX` value supplies the effective
endpoint by splitting its path from the right; this matches the pinned Python
[`socket_path_from_env`](https://github.com/tmux-python/libtmux/blob/c4a980b/src/libtmux/_internal/env.py#L91-L122)
contract. Malformed values fall back to
the default endpoint in the ordinary constructor and remain loud in the later
explicit `from_env` API. Task 7 freezes the captured connection environment on
each subprocess: it converts valid inherited `TMUX` to `-S`, removes malformed
or absent `TMUX`, and applies the resolved named/default socket root
command-locally so live environment changes cannot move dispatch away from the
captured identity.

Only export identities needed by the current Server slice. Do not add
test-only aliases or ownerless private structs for future handles. The owning
later slices must introduce and test session, window, pane, client, and winlink
identity composition against their actual handle implementations. In
particular, the Formats slice proves that underlying window identity remains
distinct from winlink-edge identity.

## Task 4: Implement redaction-aware commands and byte results

Files:

- Create `rs/src/command.rs`
- Create `rs/tests/command.rs`
- Modify `rs/src/lib.rs`

Write failing integration tests for the public `Command` builder and sanitized
`CommandSummary`. Put lowering, request/status, and result tests in
`src/command.rs` so constructors remain crate-private; do not publish factories
solely for tests. Cover public and sensitive arguments, control and non-UTF-8
diagnostic bytes, literal semicolons in every argv position, request IDs, exact
raw output, borrowed UTF-8 views, named lossy views, and custom `Debug` output.

The sentinel-redaction test must inspect every foundation diagnostic surface:

```rust
#[test]
fn sensitive_arguments_and_output_are_absent_from_debug() {
    let command = Command::new("set-environment")
        .arg("TOKEN")
        .sensitive_arg("sentinel-secret");

    assert!(!format!("{command:?}").contains("sentinel-secret"));
}
```

Extend the private unit test to `CommandArg`, `CommandSummary`,
`CommandResult`, and each error variant that owns request context. A result
containing the sentinel in stdout and stderr must expose it through explicit
byte getters but not through `Debug`; `CommandResult` has no `Display`
implementation. Walk each constructed error's `source()` chain and apply the
same sentinel assertion. Task 6 extends this check to tracing and live executor
errors once those surfaces exist.

Run the test before implementation:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --test command
```

Implement:

```rust
pub struct Command {
    subcommand: CommandArg,
    arguments: Vec<CommandArg>,
}

pub(crate) struct CommandResult {
    request_id: RequestId,
    command: CommandSummary,
    status: ProcessStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
```

`CommandArg` is private and contains an `OsString` plus public or
sensitive classification. It has no raw public conversion or getter.
`Command::new`, `arg`, and `sensitive_arg` are explicit consuming,
`#[must_use]` builders accepting `Into<OsString>`; do not add a generic
`From<OsString>` classification path. `Command`, `CommandArg`, and
`CommandResult` use manual `Debug` implementations. `CommandSummary` has safe
`Debug` and `Display` and contains only the logical subcommand, bounded escaped
public diagnostic tokens, redaction markers that disclose no value or length,
and argument counts. Diagnostic rendering is ASCII-only: non-ASCII bytes,
including valid Unicode direction controls, are escaped rather than emitted to
the terminal. It never stores lowered argv.

Keep `CommandResult`, `RequestId`, and `ProcessStatus` construction
crate-private and promote/re-export their user-facing surface with
`Server::cmd` in Task 7. `CommandRequest::new` accepts a crate-private
`RequestId`; Task 7's `Core` counter assigns it per physical dispatch. Never
allocate request IDs while constructing a cloneable logical `Command` or from a
process-global counter. Construct `ProcessStatus` privately from `ExitStatus`;
expose only success, numeric code, and terminating signal.

The crate-internal `CommandResult` implements `stdout()` and `stderr()` byte slices,
`into_streams()` for consuming both byte vectors, `stdout_utf8()` and
`stderr_utf8()` returning borrowed `std::str::Utf8Error`, and
`stdout_lossy()` and `stderr_lossy()` returning `Cow<str>`. Never retain or
chain `FromUtf8Error`, whose debug representation owns the rejected output.
Preserve all bytes exactly: do not trim trailing newlines, filter empty stderr
lines, or mirror `has-session` stderr into stdout. A non-zero status remains
data rather than a domain error.

The first slice exposes only one logical `Command`. During argv lowering,
inspect every logical token and insert exactly one backslash before its final
byte when that byte is `;`, even when one or more backslashes already precede
it. Tmux 3.2a's pinned
[`cmd_parse_from_arguments`](https://github.com/tmux/tmux/blob/3.2a/cmd-parse.y#L922-L995)
path reparses each argv token for separators after process launch.
Lower bytewise so non-UTF-8 arguments are preserved; summaries continue to
describe logical tokens. Unit tests cover `;` to `\;`, `value;` to
`value\;`, unchanged `a;b`, one and two existing backslashes before the final
semicolon, `;;`, a semicolon-bearing token followed by another token, and a
non-UTF-8 prefix. A sensitive non-UTF-8 token ending in `;` must reach lowered
argv exactly while every diagnostic retains only the redaction marker. Task 7
repeats those cases as real-tmux effect tests.
Independent batches and explicit chains remain deferred until their physical
result contracts have dedicated tests.

Task 6 validates executable, subcommand, public-argument, and
sensitive-argument NUL bytes before spawning. Detect NUL bytewise and return a
sanitized error with no `NulError` source because `NulError` owns and debugs the
rejected argument bytes.

## Task 5: Implement immutable executor capabilities

Files:

- Create `rs/src/capabilities.rs`
- Modify `rs/src/lib.rs`

Write unit tests first because no public caller exists until Task 7:

```rust
#[test]
fn stores_detected_version_without_transformation() {
    let version = TmuxVersion::parse_output(b"tmux 3.7b\n")?;
    let capabilities = EngineCapabilities::from_tmux_version(version.clone());

    assert_eq!(capabilities.tmux_version(), &version);
    assert_eq!(capabilities.tmux_version().raw(), "3.7b");
}
```

Keep the module, type, constructor, and getter crate-private in this task. The
value contains only the exact detected `TmuxVersion`, which is the capability
state consumed by the Foundation and later version-gated rendering. Cover
release and development versions, equality across equal and unequal versions,
and `Clone + Debug + Eq + Send + Sync` trait contracts. This is the configured
tmux executable's version; it does not claim that an existing daemon at the
selected endpoint runs the same build.

Task 7 promotes `EngineCapabilities` and `tmux_version()` when
`Server::capabilities()` becomes the first public caller. Keep fields and
construction private. Add transport behavior only with its first consumer and
observable behavior tests: raw byte fidelity belongs to `CommandResult`, while
dispatch grouping, chain attribution, notifications, and prepared-work cache
identity remain deferred with their owning APIs.

Run the focused test:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml capabilities
```

Expected result: the immutable value preserves exact release and development
versions without inventing transport capabilities.

## Task 6: Build the cancellation-safe subprocess supervisor

Files:

- Create `rs/src/internal/mod.rs`
- Create `rs/src/internal/executor.rs`
- Create `rs/src/internal/subprocess.rs`
- Modify `rs/Cargo.toml`
- Modify `rs/src/command.rs`
- Modify `rs/src/error.rs`
- Modify `rs/src/lib.rs`

Keep the executor private and object-safe:

```rust
pub(crate) trait Executor: Send + Sync + 'static {
    fn execute(&self, request: CommandRequest) -> DispatchFuture;
    fn shutdown(&self) -> ShutdownFuture;
}
```

`DispatchFuture` and `ShutdownFuture` are boxed `Send + 'static` futures behind
crate-private newtypes. Public handles later store
`Arc<dyn Executor + Send + Sync + 'static>`.

Before implementation, add internal tests using the current Rust test binary
as a real child helper. A specially selected helper test writes its PID to an
owned temporary path and then blocks. It performs no helper behavior unless a
private sentinel environment key is present, so an ordinary test run returns
immediately.

Add these red tests in order:

1. stdout and stderr are drained concurrently as bytes;
2. non-zero exit plus stdout returns `Ok(CommandResult)`;
3. child stdin is null and reaches EOF;
4. a deadline kills, awaits, and unregisters the child;
5. dropping the dispatch future signals cancellation and unregisters the
   child;
6. aborting a Tokio task that awaits dispatch does the same;
7. a descendant that inherits stdout is killed with the isolated process group
   and cannot strand pipe draining, both while the leader runs and after the
   leader exits unreaped;
8. concurrent executor shutdown callers cancel all active children, await an
   empty registry, reject new requests, and remain idempotent;
9. a duplicate active `RequestId` is rejected before spawn;
10. cancellation between spawn and supervisor registration cannot orphan the
    child;
11. request start racing executor shutdown either registers before shutdown or
    fails without spawning;
12. invalid executable; NUL in the executable, subcommand, public argument, or
    sensitive argument; reader failure; and supervisor loss map to sanitized
    typed errors without byte-owning `NulError` sources;
13. shell metacharacters and command-substitution text reach the child helper as
    one exact argv value and create no sentinel side effect;
14. scoped tracing covers successful and failed terminal events while fields,
    error displays, debug output, and complete source chains contain neither
    sensitive argv, raw sentinel output, nor reader or supervisor panic payloads;
15. runtime teardown synchronously signals the process group without claiming
    that async reaping can continue after the runtime is gone.

Mark each new struct-like public `Error` variant `#[non_exhaustive]` so outside
callers cannot construct a source-bearing executor error. NUL validation errors
have no source. Reader-task cancellation, panic, and supervisor loss become
source-less sanitized variants; never retain a `JoinError` or panic payload.
System I/O sources may be retained only where they cannot own executable argv
or process output.

Run only the internal supervisor tests in the red state:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml internal::subprocess::tests
```

Implementation structure:

- validate and lower the request without a shell;
- keep acceptance state and request entries under one synchronous mutex;
- atomically reject shutdown or a duplicate active `RequestId`, reserve the
  entry with its cloneable cancellation sender, and roll the reservation back
  on every validation, spawn, or setup failure;
- spawn with null stdin, piped stdout and stderr, a new process group, and
  `kill_on_drop(true)`;
- install armed process-group and registry-removal guards, transfer the child
  and both pipes to an independent Tokio supervisor, and update the reserved
  entry before the caller reaches its first await;
- have the caller future own an armed cancellation guard;
- read both pipes concurrently while selecting the overall deadline and
  cancellation, without polling or reaping the direct child yet;
- after both pipes reach EOF, await the direct child while continuing to select
  the same deadline and cancellation;
- on deadline or cancellation, terminate the isolated process group, stop pipe
  readers so inherited write ends cannot strand cleanup, kill the direct child
  as a fallback, and then await it;
- remove the request from the registry only after direct-child `wait` and pipe
  cleanup complete;
- drop process-group, child, reader, and registry ownership in that order on an
  unexpected wait failure, so shutdown cannot observe an empty registry before
  best-effort termination has run;
- notify shutdown waiters after removal;
- disarm the caller guard only after receiving the terminal result.

Create the shutdown notification before checking whether the registry is empty
and repeat that sequence in a loop so a completion between check and await
cannot strand concurrent shutdown callers. Never hold the registry mutex
across `.await`.

The process-group guard synchronously sends `SIGKILL` from `Drop`; Tokio's
`kill_on_drop` remains the direct-child fallback. The unreaped direct child
anchors the numeric process-group identity while pipe reads are pending. Disarm
the group guard immediately in the same poll continuation in which `wait`
returns, before any other await or fallible work. Never signal a stored PGID
after reaping its leader because Unix may reuse the number for an unrelated
group. A descendant-held pipe is therefore bounded by the overall command
deadline rather than a shorter post-exit timer. A registry guard removes
abnormal supervisor exits so a lost task cannot strand shutdown.

Tracing emits a sanitized request event before admission, when no registry
entry or child exists. Terminal events run only after process ownership has
been cleaned up and the registry entry removed, so a blocking subscriber cannot
strand a live child or shutdown.

The timeout path returns a typed timeout to a live caller. Caller-drop and
task-abort paths have no receiver, but the independent supervisor still reaps
the child. Internal tests assert the registry reaches zero only after the wait
path completes and use safe `rustix` existence probes to show that recorded
child and descendant PIDs are gone. Live-runtime paths assert direct-child
absence immediately when dispatch or shutdown returns rather than relying on
eventual orphan reaping.

The deterministic reaping guarantee holds while the Tokio runtime remains
alive through dispatch completion or explicit executor shutdown. Add a
fresh-runtime teardown test that documents and exercises the `kill_on_drop`
fallback, but do not claim timely async reaping after the runtime itself is
gone. Runtime-owning callers must invoke Server shutdown before dropping it.

Tokio's normal dependency enables `macros` because production uses
`tokio::select!`; the development dependency must not be the accidental source
of that feature. Include a library-only MSRV check and packaged-crate check so
development feature unification cannot mask a missing runtime feature.

Run the focused tests under both runtimes used by the crate tests:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml internal::subprocess::tests -- --test-threads=1
```

The `--test-threads=1` diagnostic run is additional evidence, not a substitute
for the normal parallel run.

## Task 7: Add the concrete Server and raw command boundary

Files:

- Create `rs/src/internal/core.rs`
- Create `rs/src/server.rs`
- Create `rs/tests/server_command.rs`
- Modify `rs/src/capabilities.rs`
- Modify `rs/src/command.rs`
- Modify `rs/src/error.rs`
- Modify `rs/src/internal/mod.rs`
- Modify `rs/src/internal/subprocess.rs`
- Modify `rs/src/lib.rs`
- Modify `rs/src/target.rs`

Write failing compile and behavior tests for:

- fallible `Server::new()` and `Server::builder().build()`;
- default, named, and explicit-path socket selection;
- config-file selection and valid color-mode lowering;
- eager rejection of conflicting socket selectors and invalid colors;
- structural `Eq` and `Hash` based on `ServerIdentity`;
- configurable tmux executable and default timeout;
- captured working-directory, executable-search, `TMUX`, `TMUX_PANE`, and
  `TMUX_TMPDIR` inputs that cannot drift after construction;
- byte-exact global socket, config, and color arguments, including values that
  end in `;`;
- lazy, shared, immutable capability detection;
- exact release and development versions through the public capability getter;
- one successful version probe across concurrent calls on cloned servers;
- retry after a failed or non-zero capability initialization;
- raw single command execution;
- concurrent and repeated `Server::shutdown()` across clones, including active
  child cleanup and rejection of later commands;
- distinct request IDs for sequential and concurrent dispatches of one cloned
  logical `Command`;
- strict and lossy output helpers;
- a non-zero tmux status returned as data;
- literal `;`, `value;`, `a;b`, and backslash-semicolon effects against real
  tmux;
- minimum-version rejection through capabilities without disabling raw
  execution;
- sanitized `Debug` output for public Server values;
- every public Server value being `Send + Sync`.

The compile contract uses `static_assertions`:

```rust
assert_impl_all!(
    Server,
    ServerBuilder,
    ServerIdentity,
    Command,
    CommandResult,
    EngineCapabilities: Send,
    Sync
);
assert_impl_all!(Server: std::fmt::Debug);
```

Run the red test:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --test server_command
```

`ServerBuilder::build()` captures the working directory and relevant connection
environment once. Socket configuration is one enum value, so explicit path and
name cannot conflict. Preserve an explicit `-S` selection and its noncanonical
path components, but join a relative path to the captured working directory
once. Dispatch the resulting absolute raw bytes used by `ServerIdentity`, so
identity and actual dispatch cannot diverge. Capture relative config paths the
same way. A valid inherited `TMUX` endpoint is converted to an explicit socket
path.

`Core` contains the tmux executable, selector, config path, color mode, identity,
default timeout, atomic request counter, Tokio
`OnceCell<EngineCapabilities>`, and private executor. Public configuration
getters live on `Server`; `Core` stays private. Capability initialization
executes `tmux -V` without a shell, parses exact bytes, enforces 3.2a, and
shares one successful immutable value. A failed or non-zero initialization may
be retried; it is not cached as permanent state. The detected version describes
the configured executable, not a daemon already listening at the selected
endpoint.

Global tmux options are a physical prefix separate from the logical
`CommandSummary`. Pass `-S`, `-L`, `-f`, `-2`, `-8`, and each captured value
exactly;
tmux consumes them before command parsing, so Task 4's terminal-semicolon
escaping applies only to logical command tokens. A valid inherited `TMUX`
endpoint becomes `-S`; preserve a captured `TMUX_PANE` only with that inherited
endpoint. Remove live `TMUX` always and remove `TMUX_PANE` for every other
selector. Named and default selectors apply the captured canonical socket root
command-locally. Capture `PATH` and the working directory without eagerly
requiring the executable to exist, so launch failures remain dispatch errors.

Promote `EngineCapabilities` and expose the capability and one-shot raw APIs in
this slice:

```rust
impl Server {
    pub async fn capabilities(&self) -> Result<&EngineCapabilities, Error>;
    pub async fn cmd(&self, command: Command) -> Result<CommandResult, Error>;
    pub async fn shutdown(&self) -> Result<(), Error>;
}
```

`shutdown()` idempotently closes the executor shared by every clone, waits for
active cleanup, and makes later `cmd()` calls fail without spawning. Runtime
owners call it before dropping Tokio so deterministic async reaping remains
available.

`Server::cmd()` is the raw escape hatch and does not initialize capabilities.
It remains available when the configured executable reports an unsupported
version. Capability queries and later typed domain operations enforce the
minimum; add the before-side-effect gate with the first domain operation.

Do not add independent batches, semicolon chains, or domain success conversion
until a later slice gives each one real-tmux ordering, early-abort, and result
attribution tests.

Use real tmux for raw command behavior. An absent explicit socket is valid
setup; commands that need a daemon may return non-zero results, while process
spawn and timeout remain `Error` values.

## Task 8: Add public isolated real-tmux test support

Files:

- Create `rs/src/test.rs`
- Create `rs/tests/test_server.rs`
- Modify `rs/Cargo.toml`
- Modify `rs/src/lib.rs`
- Modify `rs/src/server.rs`
- Modify `rs/src/internal/core.rs`

Gate the public module behind `test-support`. The repository integration tests
enable the same feature and do not maintain a second fixture implementation.

Write failing functional tests for:

- a short unique explicit socket path inside an owned temporary directory;
- exact mode 0700 on that directory after the caller's umask is applied;
- exact socket-path exposure;
- tmux startup with `-D`, an owned empty config, and no bootstrap session;
- equality between the retained foreground child PID and tmux's server PID;
- daemon process-group identity equal to that PID and distinct from the test
  runner's process group;
- independent guards running in parallel;
- consuming async shutdown awaiting the daemon and removing the socket;
- synchronous `Drop` cleanup after normal early return;
- synchronous `Drop` cleanup while unwinding;
- cleanup when construction fails after the daemon starts;
- cleanup when the startup future is aborted;
- cleanup when the shutdown future is aborted;
- parent-directory rename and symlink substitution without signaling an
  outside daemon or unlinking an outside file;
- descriptor-relative removal of only the fixed socket basename from the
  directory that was actually created;
- bounded cleanup when tmux is missing or unresponsive;
- no inherited `TMUX` or `TMUX_PANE` contamination;
- no-start `-N` on every command issued through the exposed `Server`;
- direct daemon PID disappearance after every cleanup path;
- same-process-group helper descendant disappearance on forced cleanup;
- ordinary real pane exit after graceful shutdown;
- no fixed sleeps in readiness assertions.

Run the red integration test with the feature enabled:

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --features test-support --test test_server
```

Implement `TestServer` with a consuming `#[must_use] TestServerBuilder` and
`TestServer::new().await` convenience. Prefer a short system temporary root and
fail with a clear test-support error if the resulting Unix socket path exceeds
tmux's platform limit. Validate the C-string limit exactly rather than using a
Linux-only constant. Create the directory through `tempfile`, explicitly set
and verify mode 0700 after umask processing, then retain an opened directory
descriptor plus its parent descriptor for cleanup. Disable `TempDir`'s lexical
recursive cleanup after those descriptors are retained. Remove only the fixed
owned entries through the directory descriptor, then remove the original
directory basename through its parent descriptor; a substituted parent entry
must be refused rather than traversed.

Keep the public surface to `TestServer::{new, builder, server, socket_path,
daemon_pid, shutdown}` and consuming `TestServerBuilder::{tmux_executable,
lifecycle_timeout, start}`. The guard is uniquely owned: do not implement
`Clone`, `Deref`, `AsRef`, or a raw-child escape hatch. Report fixture failures
through source-less, path-free `TestServerError` and the non-exhaustive kinds
`FilesystemSetupFailed`, `SocketPathTooLong`, `ExecutableNotFound`,
`DaemonSpawnFailed`, `DaemonExited`, `ReadinessProbeFailed`,
`DaemonPidMismatch`, `StartupTimedOut`, `ShutdownFailed`, and `CleanupFailed`.
`kind()` is the only public error accessor.

Start `tmux -D -S <socket> -f <owned-empty-config>` through
`std::process::Command`, set an isolated Unix process group with
`CommandExt::process_group(0)`, and retain that foreground `Child`; `-D` accepts
no tmux command and disables `exit-empty`, so startup creates no session. A
temporary construction guard owns the child from the instant `spawn` succeeds,
ensuring every later `?` or future cancellation kills and waits it. Never call
`killpg` unless this isolation was installed before spawn.

Startup inherits the ordinary process environment except that command-local
execution removes `TMUX` and `TMUX_PANE` and sets a deterministic terminal
type, `TERM=xterm-256color`. Do not set `HOME`, do not mutate process-global
environment, and do not serialize environment values into tmux `-e` arguments.

Socket existence alone is not readiness. Probe with a no-start
`display-message -p '#{pid}'` client and require its exact PID to equal the
retained foreground child. A successful first command also proves that initial
configuration loading has completed without allowing the probe to bootstrap a
replacement daemon. Observe early exit without reaping where the platform
offers `waitid(..., WNOWAIT)`; on other Unix targets, classify the retained
child's status only after terminal group cleanup. Never call `Child::try_wait`
or otherwise reap the leader before the final possible process-group signal.
Configure the `Server` returned by `server()` with the same global no-start
flag for every client invocation. Keep that switch crate-private: if the
retained foreground daemon exits, even a start-capable raw command must fail
instead of forking an unowned replacement server at the exposed socket.

After `socket_path()` is exposed, never launch a path-based cleanup client: the
owned directory can be renamed and the old path replaced between any identity
check and `connect`, redirecting `kill-server` to an outside daemon. Consuming
async shutdown instead transfers the foreground child to a blocking waiter,
sends SIGTERM to that retained unreaped PID, polls to a deadline, then signals
the isolated process group with SIGKILL if needed and always calls `wait` on
the direct child. It separately shuts down the shared client executor. If the
shutdown future is dropped before transfer, `TestServer::Drop` owns cleanup; if
it is dropped afterward, the unabortable blocking waiter owns cleanup.
An executor or process-lifecycle error never returns early: retain it, finish
daemon termination, direct-child wait, and descriptor cleanup, then report
`ShutdownFailed`. Report `CleanupFailed` only when lifecycle cleanup succeeds
but fixed-entry unlinking or owned-directory removal cannot be proved.
The configured lifecycle timeout bounds startup and graceful-exit observation.
On targets without a safe non-reaping child-exit observer, cap graceful
observation at five seconds before forced group cleanup; startup keeps its full
configured bound because readiness remains observable. The final kernel `wait`
after SIGKILL is mandatory and does not claim a hard wall-clock bound for an
uninterruptible process.

`Drop` never launches a cleanup client. It signals the isolated daemon process
group before reaping its retained leader, kills and waits the direct child
synchronously, and then uses descriptor-relative `unlinkat` for only the fixed
socket basename. The successful `wait` is the reaping proof; production never
probes the numeric PID after reaping because it may already have been reused.
It never resolves the exposed parent path during cleanup. The foreground child
makes this cleanup independent of a live Tokio runtime. Tests assert direct
daemon PID absence, same-group helper cleanup, and socket absence; bookkeeping
and unlinking alone are insufficient. tmux pane leaders create separate
sessions and process groups, so the fixture does not claim portable containment
of arbitrary signal-resistant pane process trees. Graceful-shutdown tests cover
an ordinary real pane.

Use observable polling with a deadline for daemon and socket state. A short
poll interval is allowed only inside that bounded observation loop.

## Task 9: Close foundation documentation and gates

Files:

- Modify `rs/README.md`
- Modify `rs/docs/design.md` if implementation evidence changes a contract
- Modify `rs/docs/parity.md`
- Modify `rs/justfile`
- Modify `.github/workflows/rust.yml`

Document:

- installation and runtime requirements;
- default, named, and explicit socket configuration;
- raw command and byte-result usage;
- sensitive argument construction;
- cancellation and timeout guarantees;
- `test-support` lifecycle and cleanup;
- the Cargo-first, just-facade tool workflow.

Every public function and method must have a compiling focused doctest. Keep
complex cancellation and cleanup demonstrations in integration tests rather
than oversized doc examples.

Mark foundation ledger rows implemented only when a matching Rust test exists;
leave future rows as `planned` with their delivery slice, never as an unowned
placeholder.

Run each direct gate independently:

```console
$ mise exec actionlint@1.7.12 -- actionlint .github/workflows/rust.yml
```

```console
$ cargo fmt --manifest-path rs/Cargo.toml --all -- --check
```

```console
$ cargo clippy --locked --manifest-path rs/Cargo.toml --all-targets --all-features -- -D warnings
```

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --all-targets --all-features
```

```console
$ cargo test --locked --manifest-path rs/Cargo.toml --doc --all-features
```

```console
$ env RUSTDOCFLAGS='-D warnings' cargo doc --locked --manifest-path rs/Cargo.toml --all-features --no-deps
```

```console
$ cargo check --locked --manifest-path rs/Cargo.toml --no-default-features
```

```console
$ cargo hack check --locked --manifest-path rs/Cargo.toml --each-feature --all-targets
```

```console
$ rustup run 1.85.0 cargo test --locked --manifest-path rs/Cargo.toml --all-targets --all-features
```

```console
$ rustup run 1.85.0 cargo hack check --locked --manifest-path rs/Cargo.toml --each-feature --all-targets
```

```console
$ cargo package --locked --manifest-path rs/Cargo.toml --allow-dirty
```

Finally run the aggregate recipe:

```console
$ just --justfile rs/justfile check
```

The recipe result is evidence that the facade matches Cargo, not an independent
test implementation.

## Task 10: Adversarial foundation review

After every gate passes, request independent reviews of:

- Rust public API idioms and SemVer hazards;
- process ownership, cancellation, reaping, and shutdown races;
- redaction and raw-output diagnostic leakage;
- socket identity, path normalization, and cleanup safety;
- tmux 3.2a behavior and real-test isolation;
- foundation parity-ledger accuracy.

Verify each finding against code and real behavior before changing anything.
The slice is complete only when no critical or important finding remains and
the full gate still passes after all accepted corrections.
