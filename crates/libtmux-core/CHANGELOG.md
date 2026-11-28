# Changelog

Notable changes to `libtmux-core`.

## Unreleased

### Added

- `options` exposes the shared option catalogue, typed values, validation
  and later-scope version requirements without a runtime dependency. Tokio
  re-exports the same types for existing callers.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
- Native capture codecs can return the pane-dead observation preceding a
  capture. Ordinary codecs support bounded history ranges and pending bytes.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
- `Error::JoinedCaptureHook` reports an ordinary joined pane capture whose
  stdout can include a configured hook's reply. The caller receives no
  partial pane bytes. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- `Error::creation_receipt` borrows verified creation identities through
  cleanup, admission and observation wrappers.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
- `ControlCommandRefusal` classifies commands that cannot share a managed
  control connection. Both drivers use `require_shared_control` and the same
  native argument parser before admitting public commands.
- Describe a session once with `NewSession`. Plans and both clients share its command arguments, daemon-bound IDs and creation receipt. The core builds without a runtime. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))
- Build a queue guard with `WindowLinkSnapshot::guard_command`. It checks the daemon generation, session, index and window before a placement mutation; unlinking uses the same guard. ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

### Changed

- **Behaviour change.** A creation without a verified identity receipt has
  `Effect::Unknown`, including malformed, late and failed follow-up replies.
  Its error describes the uncertainty and asks callers to inspect the server
  before repeating, without including diagnostic or receipt payloads.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

### Fixed

- An ordinary pane probe can receive an attached client's current pane after
  its target disappears. A valid reply naming that other pane returns
  `Error::StalePane`. Malformed metadata still fails decoding.
  ([#185](https://github.com/libtmux/libtmux-rs/issues/185))

- One-shot captures validate complete, successful and unambiguous hook tails
  before returning screen bytes. Duplicate closes inside later hook output
  and incomplete notices are refused. Creation replies retain their verified
  receipt when this validation fails.
- `FilterSchema` links its expression type with `query` enabled and `schema`
  disabled. `PaneProgressState` links `PaneFields` when `query` is enabled.
