
# Examples

Read the recorded viewport dimensions after the pane has exited. This recording and its nonce came from a tmux 3.4 client.

```rust
use libtmux_core::{CreationNonce, PaneSnapshot, SnapshotList, TmuxVersion};

let version = TmuxVersion::parse_output(b"tmux 3.4\n")?;
let nonce = CreationNonce::from_bytes([0; 16]);
let listing = SnapshotList::<PaneSnapshot>::new(&version, nonce);
let recording = concat!(
    "libtmux-row:00000000000000000000000000000000:",
    r#"\%0=4294967295=4294967295=\ =1=0=0=960=2000=0=0=0=0=0=0=0=0=0="#,
    r#"0=1=1=1=1=1=default=23=sleep=/tmp=0====default=24=0=0=0=0=0=0==="#,
    r#"341995=0=79==\"sleep\ 30\"=/tmp=0=8,16,24,32,40,48,56,64,72=nv="#,
    r#"0=/dev/pts/4=0=80==23=0=1="#,
    "\n",
);
let rows = listing.decode(Some(0), recording.as_bytes(), 0, false)?;
assert_eq!((*rows[0].pane_width(), *rows[0].pane_height()), (80, 24));
# Ok::<(), Box<dyn std::error::Error>>(())
```
