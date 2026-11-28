
# Examples

Identify a control client from a recorded listing. This recording and its nonce came from a tmux 3.4 client.

```rust
use libtmux_core::{CreationNonce, ClientSnapshot, SnapshotList, TmuxVersion};

let version = TmuxVersion::parse_output(b"tmux 3.4\n")?;
let nonce = CreationNonce::from_bytes([0; 16]);
let listing = SnapshotList::<ClientSnapshot>::new(&version, nonce);
let recording = concat!(
    "libtmux-row:00000000000000000000000000000000:",
    r#"client-342000=1795880365===1=1795880365=0=attached,focused,control-mode,UTF-8="#,
    r#"=root=342000=0=0==dumb===1000=nv=1=80=0="#,
    "\n",
);
let rows = listing.decode(Some(0), recording.as_bytes(), 0, false)?;
assert!(rows[0].client_control_mode());
# Ok::<(), Box<dyn std::error::Error>>(())
```
