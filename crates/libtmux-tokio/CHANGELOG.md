# Changelog

Notable changes to `libtmux-tokio`.

## Unreleased

### Added

- Run tmux operations with Tokio. `Server::new_session` returns the shared creation receipt. Use `Server::inspect_session` to read the session and `Server::capture` to capture the pane identified by either driver. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

### Changed

- **Breaking.** Select, move and swap an observed placement through `WindowLink::{select, move_to, swap_with}`. Obtain it with `window.link()`. Reindexed links are refused, and successful moves and swaps refresh the returned placement. `Window::kill` still kills the intrinsic window, and `Window::link_to` can link it after its discovery session has gone. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
