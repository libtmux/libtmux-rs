# Move from 0.1.2 to the native drivers

The facade defaults to `libtmux::blocking::Server`. For async code, enable the
`tokio` feature and import `libtmux::tokio::Server`. A program using both
imports can pass a `libtmux::PaneKey` between them. Offline tools can depend on
`libtmux-core` alone.

| In 0.1.2 | In the native clients |
| --- | --- |
| Root `Server` with async methods | `libtmux::tokio::Server` |
| `blocking::Runtime::run` around session creation, listing or pane capture | `blocking::Server::new_session`, `snapshots`, `observe_pane` and `capture` |
| `blocking::Runtime::run` around pane input, a window split or a plan | Use the Tokio client's pane, window or plan API on your application's runtime |
| `NewSessionOptions` or `plan::NewSession` | Shared `NewSession` |
| Session handle returned by creation | `CreatedSession` with the three IDs and daemon generation |
| `plan.run(&server, planner)` | `server.run_plan(&plan, planner).await` on a Tokio server |
| `plan.run_over_control_mode(&sender)` | `sender.run_plan(&plan).await` on a Tokio control connection |

Use the pane key on the receipt for a capture. To read session metadata,
pass the receipt to the Tokio client's `inspect_session`. If session creation
fails, `error.creation_receipt()` can still identify the session a hook
left behind. Use that receipt for inspection or cleanup. An unknown effect
means the session may already exist, so replaying creation can duplicate
the action. [Architecture decision](adr/0001-native-blocking-and-tokio.md)

Earlier migrations accompany the source version they describe.
[0.1.2 migration notes](https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/docs/migration.md)
