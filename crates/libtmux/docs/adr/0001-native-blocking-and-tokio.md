# ADR 0001: Share tmux operations between native blocking and Tokio clients

**Proposed, 28 November 2026.** The next breaking Rust release should offer
a native blocking client by default and a native Tokio client as an additive
feature. Both clients use the same owned observations and operation types.
This record covers the Rust workspace. [Implementation issue][issue] ·
[Dated decision and tested reader][snapshot]

## Context

A CLI should be able to inspect an existing pane without starting an async
runtime. An application should be able to pass that same pane to a Tokio
watcher. In 0.1.2 an async handle combines its executor with the fields from
the last read. The blocking bridge runs the same futures on a current-thread
runtime. In the idle-delivery experiment, a ready reply waited until the
caller entered the runtime again. Immediate and planned session creation
also have separate builders, encoders and sets of supported arguments.
[Released design][released] · [Immediate creation][immediate] ·
[Planned creation][planned]

## Decision

| Package | Responsibility |
| --- | --- |
| `libtmux-core` | Byte-preserving arguments and text, IDs, daemon-bound keys, immutable snapshots, command preparation, reply decoding, version rules and plan steps. Pure operations can use `std`. They perform no I/O. |
| `libtmux-blocking` | Native blocking subprocesses, monotonic deadlines, cleanup and a persistent control reader that progresses between caller requests. Its normal dependency graph excludes Tokio. |
| `libtmux-tokio` | Tokio subprocess supervision, cancellation, persistent control I/O and fallible streams on the caller's runtime. |
| `libtmux` | Re-exports the shared values and concrete `blocking::Server` and `tokio::Server`. The blocking feature is the default. Enabling both adds both namespaces. |

A driver owns a live handle. An immutable observation can outlive that
handle. In the prototype, `blocking.observe_pane(id)?` returned a
`PaneKey` that `tokio_server.capture(&key).await?` accepted directly.
A configure-only tool can depend on core alone. Keep all four packages on
one release train with exact sibling dependencies so mixed callers resolve
one copy of the shared types. [Complete reader and dependency cases][snapshot] ·
[Cargo feature unification][cargo]

### Operations and identity

Define each operation once, including argument intent, version and target
checks, eligible transports, output decoder and effect on failure. Keep
byte-preserving `TmuxText` and distinguish literal and format arguments.
Query pushdown requires agreement with the in-memory predicate. The adapters
drive the same steps when more I/O is needed.

Begin with `new-session -d -P -F`. The action's reply identifies the session,
first window, first pane, daemon PID and start time. Use that receipt to
return the created objects and bind plan slots. If hydration fails later,
return the receipt and the failure. Acknowledged creation with an unreadable
row can establish creation while leaving the IDs unknown.

An endpoint locates a server. An object key additionally carries its observed
daemon generation and object ID. Guard an effect, including a deferred unlock,
in the same tmux queue as the effect. A key-bound operation must never start a
replacement daemon. PID and whole-second start time have a collision limit.
Require fresh discovery to rebind. A window key identifies the underlying
window. A window link identifies a placement in a session. Recheck the
expected window before acting on a link: an index can move, and tmux can
resolve a missing link to the current window. These contracts assume native
semantics for the configured executable and the operation and guard commands.
[Generation and replacement][generation] · [Built-in command contract][builtins]

### Effects, cancellation and control

| Evidence | Caller-visible outcome |
| --- | --- |
| Admission refused or a guard independently rejected the request | Report the refusal before the action. |
| Bytes submitted, no verified action acknowledgement | Report unknown effect. Never automatically replay a mutation. |
| Action acknowledged, followed by hook, decode, hydration or cleanup failure | Keep the receipt, failing stage and cause. Preserve both operation and cleanup failures. |
| Part of a batch acknowledged | Return the proven steps. Leave the remainder uncertain where framing cannot identify them. |

tmux can emit valid creation IDs and then exit unsuccessfully because an
after-hook failed. It can also send the complete action frame while a hook
is parked. Preserve verified receipts through deadlines, signals and EOF.
An ambiguous frame supplies no such proof. Keep unrelated hook output and
unverified pane bytes out of results and diagnostics. A Tokio start error
can follow OS process creation when registration of a pipe or reaper fails.
An undifferentiated start error therefore leaves the effect unknown.
[Failure experiments][snapshot] · [Tokio process registration][registration]

Give each request one deadline from admission through follow-up reads.
Bound requests across clones and bytes on each pipe. Hold the admission slot
through reaping, including after cancellation. Bound cleanup and expose
cleanup failure to an explicit close. Test process-group ownership and child
reclamation on startup failure and runtime shutdown.

Acquire persistent control explicitly against a checked daemon at or above
tmux 3.7 final with a compatible client. Use a separate subprocess for
client-blocking waits and executable flags. Only use persistent control when
the operation's encoding distinguishes payload bytes from its active reply
terminator. Qualify one-shot framed commands separately on older releases.
Low-level observations on 3.4 do not qualify managed control on that release.
[Release floors and client compatibility][control-floor]

One reader owns each connection. Once any bytes of a command have been
written, finish the line or close. Drain every expected reply block before
assigning a later response, even if the caller has gone away. A conditional
can emit two blocks, and a wait acknowledgement can precede queue progress.
If correlation is lost, retain proven receipts, mark outstanding submitted
actions uncertain and identify requests still waiting to be written.

Bound subscriber queues by bytes and item count. Both blocking receivers and
async streams expose data, gaps, terminal failure and clean completion. A slow
subscriber must not stop the command reader. Resume age cannot establish
continuity. A new control client on the same pane begins a new raw-output
segment. A grid capture supplies current screen state and cannot recover
the missing bytes. [tmux control delivery][upstream-control]

## Alternatives and consequences

A dedicated Tokio worker delivered replies while the blocking caller was
idle and reused the same live handle. Its blocking-only binary still depended
on Tokio. A native blocking mirror inside one package can meet the runtime
requirement. Separate packages additionally let the CLI and MCP select and
audit their driver dependencies independently.

The [native blocking root proposal][pr66] puts the blocking API in the
facade and supplies a Tokio sibling. The [async-io proposal][pr82] shares
async operations between runtimes and offers a blocking wait. The four-crate
decision keeps driver-specific lifecycle code in each adapter and shares the
operation semantics. The cost is two supervisors and a coordinated release.
Qualify both against the same fixtures. The project would revisit the split
if its only Tokio-free consumers needed offline data.

## Migration and release criteria

1. Extract shared values and complete creation semantics. Preserve the library
   Rust 1.85 and MCP Rust 1.88 floors. Test core alone, each driver alone and
   both together. Build an external consumer from the staged packages and
   check that a key created by one driver works in the other.
2. Migrate `tmux-mcp` to Tokio and `tmux-workspace` to the shared plan
   operations. Verify MCP discovery, scoped refusals, link loss, unknown
   effects and output gaps over stdio. Exercise cancellation, reaping,
   replacement and linked windows on the supported tmux matrix.
3. Remove root async handles, `blocking::Runtime`, duplicated immediate and
   planned encoders and superseded control entry points. Update every caller,
   diff the public inventories, require retired imports to fail, and document
   replacement calls in the breaking changelog.
4. Update the Rust extractor and pinned docs workflow together. Give each
   symbol a crate, module and public path. Run programs copied from rendered
   HTML and Markdown against staged packages. Verify rustdoc, versioned
   reference links, search and `llms.txt`. Preserve 0.1.2 documentation.
5. Require independent review of the implementation. The architecture
   experiment covered two reads and a creation operation. Production
   acceptance also requires the complete control drivers, process containment,
   migrated consumers and release checks. The issue records the evidence,
   outstanding work and review thresholds. [Implementation gates][issue] ·
   [Port review rubric][rubric]

[issue]: https://github.com/libtmux/libtmux-rs/issues/185
[snapshot]: https://github.com/libtmux/libtmux-rs/issues/185#issuecomment-7239797783
[released]: https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/docs/design.md
[immediate]: https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/src/server.rs#L2407
[planned]: https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/src/plan/ops/sessions.rs#L27
[cargo]: https://doc.rust-lang.org/cargo/reference/features.html#feature-unification
[generation]: https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/src/server.rs#L1777
[builtins]: https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/docs/contracts.md#L37-L40
[registration]: https://github.com/tokio-rs/tokio/blob/b53b2001ed2ccfd9b87e32a0c6da1a1eebdc80b8/tokio/src/process/unix/mod.rs#L122-L145
[control-floor]: https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/docs/contracts.md#L60-L80
[upstream-control]: https://github.com/tmux/tmux/blob/e476c1230b958df0cb12977517d24b3dc931375b/control.c#L235-L391
[pr66]: https://github.com/libtmux/libtmux-rs/pull/66
[pr82]: https://github.com/libtmux/libtmux-rs/pull/82
[rubric]: https://github.com/libtmux/libtmux-rs/issues/166
