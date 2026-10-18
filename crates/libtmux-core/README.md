# libtmux-core

`libtmux-core` provides byte-preserving tmux text, typed object IDs, literal
arguments, validated session names, ordered release values, and control-mode
notification decoding. It uses only the Rust standard library, including in
its tests. It does not start tmux or require an async runtime.

```toml
[dependencies]
libtmux-core = "0.1.0-alpha.15"
```

```rust
use libtmux_core::{PaneId, TmuxArg, TmuxText};

let pane: PaneId = "%0007".parse()?;
assert_eq!(pane.as_ref(), "%7");

let title = TmuxArg::from("release#1");
assert_eq!(title.as_os_str(), "release##1");

let output = TmuxText::from_bytes([b'a', 0xff]);
assert_eq!(output.as_bytes(), [b'a', 0xff]);
assert!(output.as_str().is_err());
# Ok::<(), libtmux_core::IdParseError>(())
```

IDs preserve their object scope and compare numerically. `TmuxText` preserves
arbitrary bytes; UTF-8 validation and lossy decoding are explicit choices.
`TmuxArg::from` escapes tmux format expansion. Use `TmuxArg::format` only when
the caller intends that expansion.

`control::Line` decodes every notification modeled by the client. Pass the
current block number to `parse_within_block` while reading command output;
text that resembles a notification remains output inside that block.

The argument types use Unix `OsStr` bytes and support Linux and WSL. They do
not provide process execution, deadlines, cancellation, or control-mode I/O.
