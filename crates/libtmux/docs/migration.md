# Move from 0.1.2 to the native drivers

The facade defaults to `libtmux::blocking::Server`. For async code, enable the
`tokio` feature and import `libtmux::tokio::Server`. A program using both
imports can pass a `libtmux::PaneKey` between them. Offline tools can depend on
`libtmux-core` alone.

| In 0.1.2 | In the native clients |
| --- | --- |
| Root `Server` with async methods | `libtmux::tokio::Server` |
| `blocking::Runtime::run` around a future | Direct calls on `libtmux::blocking::Server` |
| `NewSessionOptions` or `plan::NewSession` | Shared `NewSession` |
| Session handle returned by creation | `CreatedSession` with the three IDs and daemon generation |
| `plan.run(&server, planner)` | `server.run_plan(&plan, planner)` |
| `plan.run_over_control_mode(&sender)` | `sender.run_plan(&plan)` |

Use the pane key on the receipt for a capture. To read session metadata,
pass the receipt to the Tokio client's `inspect_session`. If a creation
returns an error after tmux acknowledged it, inspect the receipt there
before retrying. [Architecture decision](adr/0001-native-blocking-and-tokio.md)

Earlier migrations accompany the source version they describe.
[0.1.2 migration notes](https://github.com/libtmux/libtmux-rs/blob/1027012583abdc8b07ea0fd071e0c6cacb051278/crates/libtmux/docs/migration.md)
