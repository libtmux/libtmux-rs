//! Repository-internal MCP client configuration switcher.

#![forbid(unsafe_code)]

/// Supported client catalog and selector normalization.
pub mod catalog;
/// Format-aware client configuration transforms.
pub mod config;
/// Authenticated filesystem routes and no-replace file operations.
pub mod fs;
/// JSON-with-comments parsing and byte-preserving edits.
pub mod jsonc;
/// Process and filesystem lock for configuration transactions.
pub mod lock;
/// Bounded MCP initialize handshake.
pub mod preflight;
/// Versioned, checksummed recovery state.
pub mod recovery;
/// Local-build, cargo-run, path, and published source resolution.
pub mod source;
/// All-selected use and revert transaction coordinator.
pub mod transaction;
