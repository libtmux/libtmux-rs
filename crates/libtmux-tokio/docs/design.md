# Tokio driver design

The canonical architecture and design are in the
[`libtmux` design notes](https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/design.md).
Session creation returns the same typed receipt as the blocking driver. A handle
obtained from that receipt carries the expected daemon generation into later
commands.
