# Changelog

Notable changes to `libtmux-tokio`.

## Unreleased

### Added

- Run tmux operations with Tokio. `Server::new_session` returns the shared creation receipt. Use `Server::inspect_session` to read the session and `Server::capture` to capture the pane identified by either driver. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
