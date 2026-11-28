# Tokio driver design

The canonical architecture and design are in the
[`libtmux` design notes](https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/docs/design.md).
The released Tokio object model and native process and control drivers moved
into `libtmux-tokio` without changing their execution contracts.
