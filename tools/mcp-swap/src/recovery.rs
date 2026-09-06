//! Versioned, checksummed, private recovery state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Scope;
use crate::fs::{
    DirectorySnapshot, FileIdentity, FileSnapshot, FsError, LinkSnapshot, digest, stable_snapshot,
};
use crate::jsonc;

/// Recovery schema version written by this native tool.
pub const STATE_VERSION: u32 = 1;
/// Maximum accepted recovery file size.
pub const STATE_MAX_BYTES: usize = 256 * 1024;

/// One configuration layer tracked for exact restoration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEntry {
    /// Canonical client name.
    pub client: String,
    /// Normalized configuration scope.
    pub scope: Scope,
    /// Strictly increasing transaction order.
    pub sequence: u64,
    /// MCP server key changed by the swap.
    pub server: String,
    /// Absolute logical client configuration path.
    pub config_path: PathBuf,
    /// Absolute physical configuration target.
    pub target_path: PathBuf,
    /// Every final-path symlink authenticated when the first backup was made.
    pub route_links: Vec<LinkSnapshot>,
    /// Authenticated parent directories for the logical symlink route.
    pub route_anchors: Vec<DirectorySnapshot>,
    /// Authenticated physical parent directory.
    pub route_parent: DirectorySnapshot,
    /// Mode restored with the original configuration bytes.
    pub original_mode: u32,
    /// Absolute first-backup path.
    pub backup_path: PathBuf,
    /// Exact original backup identity.
    pub backup: FileIdentity,
    /// Exact configuration identity expected after the latest use.
    pub expected_config: FileIdentity,
}

/// All outstanding swaps in canonical key order.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    /// Sequence number assigned to the next first-time layer swap.
    pub next_sequence: u64,
    /// Entries keyed as `client:scope`.
    pub entries: BTreeMap<String, RecoveryEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerFile {
    version: u32,
    checksum: String,
    payload: Ledger,
}

/// Read and authenticate one private recovery ledger.
///
/// # Errors
///
/// Returns [`FsError`] for symlinks, wrong ownership or mode, hard links,
/// oversize input, malformed schemas, noncanonical entries, or checksum drift.
pub fn load_ledger(path: &Path) -> Result<Ledger, FsError> {
    load_ledger_snapshot(path).map(|(ledger, _)| ledger)
}

pub(crate) fn load_ledger_snapshot(path: &Path) -> Result<(Ledger, FileSnapshot), FsError> {
    load_ledger_snapshot_with_hook(path, &mut || Ok(()))
}

pub(crate) fn load_ledger_snapshot_with_hook(
    path: &Path,
    hook: &mut dyn FnMut() -> Result<(), FsError>,
) -> Result<(Ledger, FileSnapshot), FsError> {
    let snapshot = stable_snapshot(path, STATE_MAX_BYTES)?;
    hook()?;
    validate_private(&snapshot.identity, "recovery state")?;
    let text = std::str::from_utf8(&snapshot.bytes)
        .map_err(|error| FsError::new(format!("recovery state is not UTF-8: {error}")))?;
    let value = jsonc::parse_json(text)
        .map_err(|error| FsError::new(format!("parse recovery state: {error}")))?;
    let file: LedgerFile = serde_json::from_value(value)
        .map_err(|error| FsError::new(format!("parse recovery state: {error}")))?;
    if file.version != STATE_VERSION {
        return Err(FsError::new(format!(
            "unsupported recovery state version {}",
            file.version
        )));
    }
    validate_ledger(&file.payload)?;
    let expected = ledger_checksum(&file.payload)?;
    if file.checksum != expected {
        return Err(FsError::new("recovery state checksum mismatch"));
    }
    Ok((file.payload, snapshot))
}

/// Serialize a ledger under the size ceiling.
///
/// # Errors
///
/// Returns [`FsError`] for an invalid ledger or oversized representation.
pub fn ledger_bytes(ledger: &Ledger) -> Result<Vec<u8>, FsError> {
    validate_ledger(ledger)?;
    let file = LedgerFile {
        version: STATE_VERSION,
        checksum: ledger_checksum(ledger)?,
        payload: ledger.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| FsError::new(format!("serialize recovery state: {error}")))?;
    bytes.push(b'\n');
    if bytes.len() > STATE_MAX_BYTES {
        return Err(FsError::new(format!(
            "recovery state exceeds {STATE_MAX_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

/// Return the canonical state key for a client layer.
#[must_use]
pub fn state_key(client: &str, scope: Scope) -> String {
    let scope = match scope {
        Scope::User => "user",
        Scope::Project => "project",
    };
    format!("{client}:{scope}")
}

fn ledger_checksum(ledger: &Ledger) -> Result<String, FsError> {
    let payload = serde_json::to_vec(ledger)
        .map_err(|error| FsError::new(format!("serialize recovery checksum: {error}")))?;
    Ok(digest(&payload))
}

fn validate_ledger(ledger: &Ledger) -> Result<(), FsError> {
    let mut sequences = BTreeMap::new();
    for (key, entry) in &ledger.entries {
        if key != &state_key(&entry.client, entry.scope) {
            return Err(FsError::new(format!("noncanonical recovery key {key:?}")));
        }
        if !matches!(
            entry.client.as_str(),
            "claude" | "codex" | "cursor" | "gemini" | "grok" | "agy" | "opencode" | "pi"
        ) {
            return Err(FsError::new(format!(
                "unknown recovery client {:?}",
                entry.client
            )));
        }
        for (label, path) in [
            ("config_path", &entry.config_path),
            ("target_path", &entry.target_path),
            ("backup_path", &entry.backup_path),
        ] {
            if !path.is_absolute() {
                return Err(FsError::new(format!("recovery {label} must be absolute")));
            }
        }
        if sequences.insert(entry.sequence, key).is_some() {
            return Err(FsError::new("recovery sequence numbers must be unique"));
        }
        if entry.sequence >= ledger.next_sequence {
            return Err(FsError::new("recovery sequence exceeds next_sequence"));
        }
    }
    Ok(())
}

fn validate_private(identity: &FileIdentity, label: &str) -> Result<(), FsError> {
    if identity.mode != 0o600 {
        return Err(FsError::new(format!("{label} must have mode 0600")));
    }
    if identity.uid != rustix::process::getuid().as_raw() {
        return Err(FsError::new(format!("{label} has a different owner")));
    }
    if identity.links != 1 {
        return Err(FsError::new(format!(
            "{label} must have exactly one hard link"
        )));
    }
    Ok(())
}
