
# Examples

Read the name as bytes from an archived session listing. This recording and its nonce came from a tmux 3.4 client.

```rust
use libtmux_core::{CreationNonce, SessionSnapshot, SnapshotList, TmuxVersion};

let version = TmuxVersion::parse_output(b"tmux 3.4\n")?;
let nonce = CreationNonce::from_bytes([0; 16]);
let listing = SnapshotList::<SessionSnapshot>::new(&version, nonce);
let recording = concat!(
    "libtmux-row:00000000000000000000000000000000:",
    r#"\$0=1795880365=1=1795880365=1795880365=0=work=/tmp=1="#,
    "\n",
);
let rows = listing.decode(Some(0), recording.as_bytes(), 0, false)?;
assert_eq!(rows[0].session_name().as_bytes(), b"work");
# Ok::<(), Box<dyn std::error::Error>>(())
```
