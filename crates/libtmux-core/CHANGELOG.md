# Changelog

Notable changes to `libtmux-core`.

## Unreleased

### Added

- Describe a session once with `NewSession`. Plans and both clients share its command arguments, daemon-bound IDs and creation receipt. The core builds without a runtime. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
- Build a queue guard with `WindowLinkSnapshot::guard_command`. It checks the daemon generation, session, index and window before a placement mutation; unlinking uses the same guard. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
