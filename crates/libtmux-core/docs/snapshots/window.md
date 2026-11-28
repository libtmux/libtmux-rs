
# Examples

Read the name and pane count of a saved window. This recording and its nonce came from a tmux 3.4 client.

```rust
use libtmux_core::{CreationNonce, WindowSnapshot, SnapshotList, TmuxVersion};

let version = TmuxVersion::parse_output(b"tmux 3.4\n")?;
let nonce = CreationNonce::from_bytes([0; 16]);
let listing = SnapshotList::<WindowSnapshot>::new(&version, nonce);
let recording = concat!(
    "libtmux-row:00000000000000000000000000000000:",
    r#"@0=1795880365=32=16=24=b25d,80x24,0,0,0=editor=1="#,
    r#"b25d,80x24,0,0,0=80=0="#,
    "\n",
);
let rows = listing.decode(Some(0), recording.as_bytes(), 0, false)?;
assert_eq!(rows[0].window_name().as_bytes(), b"editor");
assert_eq!(*rows[0].window_panes(), 1);
# Ok::<(), Box<dyn std::error::Error>>(())
```
