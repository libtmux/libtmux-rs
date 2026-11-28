# libtmux-core

`libtmux-core` describes tmux operations and decodes their replies without
performing I/O or choosing an async runtime. Applications normally use a
transport crate; use the core when recording operations or writing a driver.

## Describe a session

Names and directories are literal by default. `TmuxArg::format` requests tmux
format expansion. Shell commands and environment assignments keep their exact
operating-system strings and carry a sensitive diagnostic classification.

```rust
use libtmux_core::{NewSession, OperandKind, TmuxArg};

let request = NewSession::new("work#1")
    .window_name("editor")
    .start_directory(TmuxArg::format("#{pane_current_path}"))
    .environment("TOKEN", "private-value")
    .size(120, 40);
let operands = request.operands(NewSession::RECEIPT_FORMAT);
assert_eq!(operands[5].as_os_str(), "work##1");
assert!(operands.iter().any(|arg| arg.kind() == OperandKind::Sensitive));
```

`operands` returns logical values for a transport to encode. `arguments`
produces process argv for a one-shot control client, including `-C`, and
escapes trailing semicolons so a value cannot become a command boundary.

`ordinary_arguments` uses a fresh driver-supplied `CreationNonce` to request a
receipt without `-C`. Only the first complete stamped record is accepted;
prefaced or duplicate-stamped output leaves creation unconfirmed. The
[ordinary receipt contract](docs/ordinary-creation.md) records its provenance
and the daemon trust boundary.

## Keep creation evidence

A creation receipt binds the session, first window and first pane IDs to the
daemon generation and endpoint reported by one action. A successful action
followed by a failed hook, missing `%exit` or late reply retains that evidence
in the error. An ambiguous action frame leaves the requested effect unknown.

The observation and capture decoders return data only from complete, unique
action frames. Hook output and partial capture bytes never become a result.

## Read a saved observation

Both native clients return the same owned snapshots. You can keep one after
its session exits, compare successive listings, or read the original bytes
of a session name without contacting tmux.

```rust
use libtmux_core::SessionSnapshot;

fn names(sessions: &[SessionSnapshot]) -> impl Iterator<Item = &[u8]> {
    sessions.iter().map(|session| session.session_name().as_bytes())
}
```

[Changelog](CHANGELOG.md)
