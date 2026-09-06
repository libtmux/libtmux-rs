//! Authenticated filesystem routes and no-replace operations.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{CWD, Mode, OFlags, RenameFlags, open, renameat_with};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lock::retain_if_active_lock;

static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static RETAIN_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A filesystem route changed or violated the private-artifact contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsError(String);

impl FsError {
    /// Construct an error for a failed authenticated filesystem operation.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for FsError {}

/// Stable identity and contents digest for one regular file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    /// Filesystem device number.
    pub device: u64,
    /// Filesystem inode number.
    pub inode: u64,
    /// Permission and special mode bits.
    pub mode: u32,
    /// Owning user ID.
    pub uid: u32,
    /// Hard-link count.
    pub links: u64,
    /// File length in bytes.
    pub size: u64,
    /// Lowercase SHA-256 digest.
    pub sha256: String,
}

/// Stable bytes and identity read from one regular file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileSnapshot {
    /// Absolute physical path opened without following a final symlink.
    pub path: PathBuf,
    /// File identity and content digest.
    pub identity: FileIdentity,
    /// Exact file bytes.
    pub bytes: Vec<u8>,
}

pub(crate) struct StagedFile {
    path: PathBuf,
    identity: FileIdentity,
    limit: usize,
    armed: bool,
    complete: bool,
}

impl StagedFile {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn identity(&self) -> &FileIdentity {
        &self.identity
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(crate) fn cleanup(&mut self) -> Result<(), FsError> {
        if !self.armed {
            return Ok(());
        }
        match fs::symlink_metadata(&self.path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.armed = false;
                return Ok(());
            }
            Ok(_) => {}
            Err(error) => {
                return Err(FsError::new(format!(
                    "inspect stage {}: {error}",
                    self.path.display()
                )));
            }
        }
        let current = stable_snapshot(&self.path, self.limit)?;
        let matches = if self.complete {
            current.identity == self.identity
        } else {
            current.identity.device == self.identity.device
                && current.identity.inode == self.identity.inode
        };
        if !matches {
            return Err(FsError::new(format!(
                "stage changed; retained {}",
                self.path.display()
            )));
        }
        remove_exact(&self.path, &current.identity, self.limit)?;
        self.armed = false;
        Ok(())
    }

    fn cleanup_error(&mut self, error: FsError) -> FsError {
        match self.cleanup() {
            Ok(()) => error,
            Err(cleanup) => FsError::new(format!("{error}; stage cleanup incomplete: {cleanup}")),
        }
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

/// One symlink in a logical configuration route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkSnapshot {
    /// Absolute symlink path.
    pub path: PathBuf,
    /// Link text as stored in the directory entry.
    pub target: PathBuf,
    /// Device number of the link itself.
    pub device: u64,
    /// Inode number of the link itself.
    pub inode: u64,
    /// Link mode bits.
    pub mode: u32,
    /// Owning user ID.
    pub uid: u32,
}

/// Logical and physical identity of the target's parent directory.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectorySnapshot {
    /// Logical parent named by the client configuration path.
    pub logical: PathBuf,
    /// Canonical parent used for the final file operation.
    pub physical: PathBuf,
    /// Device number of the canonical directory.
    pub device: u64,
    /// Inode number of the canonical directory.
    pub inode: u64,
    /// Directory mode bits.
    pub mode: u32,
    /// Owning user ID.
    pub uid: u32,
}

/// Authenticated logical route and physical configuration file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigRoute {
    /// Absolute logical path stored for the client.
    pub logical: PathBuf,
    /// Absolute physical file reached by the link chain.
    pub target: PathBuf,
    /// Every final-path symlink traversed in order.
    pub links: Vec<LinkSnapshot>,
    /// Parent-directory identity for every symlink path in the logical route.
    pub anchors: Vec<DirectorySnapshot>,
    /// Parent-directory identity for the physical file.
    pub parent: DirectorySnapshot,
    /// Stable file bytes and identity.
    pub file: FileSnapshot,
    limit: usize,
}

impl ConfigRoute {
    /// Re-read the route and require every stored identity and byte to match.
    ///
    /// # Errors
    ///
    /// Returns [`FsError`] when a link, directory, file, mode, or digest changed.
    pub fn verify(&self) -> Result<(), FsError> {
        let current = resolve_config_route(&self.logical, self.limit)?;
        if current != *self {
            return Err(FsError::new(format!(
                "configuration route changed: {}",
                self.logical.display()
            )));
        }
        Ok(())
    }

    /// Require the logical links and physical parent to remain unchanged.
    ///
    /// Unlike [`Self::verify`], this accepts an absent final target while a
    /// transaction has taken the file aside.
    ///
    /// # Errors
    ///
    /// Returns [`FsError`] when a link, target route, or parent changed.
    pub fn verify_topology(&self) -> Result<(), FsError> {
        let (target, links, anchors, parent) = route_topology(&self.logical)?;
        if target != self.target
            || links != self.links
            || anchors != self.anchors
            || parent != self.parent
        {
            return Err(FsError::new(format!(
                "configuration route changed: {}",
                self.logical.display()
            )));
        }
        Ok(())
    }

    /// Require the authenticated route to be unchanged and its target absent.
    ///
    /// # Errors
    ///
    /// Returns [`FsError`] when topology changed or a file appeared before
    /// create-new publication.
    pub fn verify_target_absent(&self) -> Result<(), FsError> {
        self.verify_topology()?;
        match fs::symlink_metadata(&self.target) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(FsError::new(format!(
                "configuration target appeared: {}",
                self.target.display()
            ))),
            Err(error) => Err(FsError::new(format!(
                "inspect absent configuration target: {error}"
            ))),
        }
    }
}

fn route_topology(
    logical: &Path,
) -> Result<
    (
        PathBuf,
        Vec<LinkSnapshot>,
        Vec<DirectorySnapshot>,
        DirectorySnapshot,
    ),
    FsError,
> {
    let logical = normalize_absolute(logical)?;
    let mut current = logical.clone();
    let mut links = Vec::new();
    let mut anchors = Vec::new();
    let mut visited = BTreeSet::new();
    for _ in 0..40 {
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if !visited.insert((metadata.dev(), metadata.ino())) {
                    return Err(FsError::new("configuration symlink loop"));
                }
                let target = fs::read_link(&current).map_err(|error| {
                    FsError::new(format!("read configuration symlink: {error}"))
                })?;
                links.push(LinkSnapshot {
                    path: current.clone(),
                    target: target.clone(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    mode: metadata.mode() & 0o7777,
                    uid: metadata.uid(),
                });
                let parent = current
                    .parent()
                    .ok_or_else(|| FsError::new("configuration symlink has no parent"))?;
                let anchor = snapshot_directory(parent)?;
                let next = if target.is_absolute() {
                    target
                } else {
                    anchor.physical.join(target)
                };
                anchors.push(anchor);
                current = normalize_absolute(&next)?;
            }
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(FsError::new(format!(
                    "inspect configuration route {}: {error}",
                    current.display()
                )));
            }
        }
    }
    let logical_parent = current
        .parent()
        .ok_or_else(|| FsError::new("configuration has no parent"))?;
    let parent = snapshot_directory(logical_parent)?;
    let target = parent.physical.join(
        current
            .file_name()
            .ok_or_else(|| FsError::new("configuration has no file name"))?,
    );
    Ok((target, links, anchors, parent))
}

fn snapshot_directory(logical: &Path) -> Result<DirectorySnapshot, FsError> {
    let physical = logical
        .canonicalize()
        .map_err(|error| FsError::new(format!("resolve configuration parent: {error}")))?;
    let metadata = fs::metadata(&physical)
        .map_err(|error| FsError::new(format!("inspect configuration parent: {error}")))?;
    if !metadata.is_dir() {
        return Err(FsError::new("configuration parent is not a directory"));
    }
    Ok(DirectorySnapshot {
        logical: logical.to_path_buf(),
        physical,
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
    })
}

/// One logical, resolved, and optional physical artifact reservation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactClaim {
    /// Human-readable owner of the reservation.
    pub label: String,
    /// Normalized logical path.
    pub logical: PathBuf,
    /// Prospective physical path after parent resolution.
    pub resolved: PathBuf,
    /// Existing regular-file device and inode, if present.
    pub physical: Option<(u64, u64)>,
}

impl ArtifactClaim {
    /// Build a claim from an authenticated configuration route.
    #[must_use]
    pub fn from_route(label: impl Into<String>, route: &ConfigRoute) -> Self {
        Self {
            label: label.into(),
            logical: route.logical.clone(),
            resolved: route.target.clone(),
            physical: Some((route.file.identity.device, route.file.identity.inode)),
        }
    }

    /// Resolve a present or prospective artifact path into an alias claim.
    ///
    /// # Errors
    ///
    /// Returns [`FsError`] for a relative path, missing parent, or unsupported
    /// existing file type.
    pub fn for_path(label: impl Into<String>, path: &Path) -> Result<Self, FsError> {
        let logical = normalize_absolute(path)?;
        let parent = logical
            .parent()
            .ok_or_else(|| FsError::new("artifact path has no parent"))?;
        let parent = prospective_parent(parent)?;
        let name = logical
            .file_name()
            .ok_or_else(|| FsError::new("artifact path has no file name"))?;
        let resolved = parent.join(name);
        let physical = match fs::symlink_metadata(&logical) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(FsError::new(format!(
                    "artifact path is a symlink: {}",
                    logical.display()
                )));
            }
            Ok(metadata) if metadata.is_file() => Some((metadata.dev(), metadata.ino())),
            Ok(_) => {
                return Err(FsError::new(format!(
                    "artifact path is not a regular file: {}",
                    logical.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(FsError::new(format!(
                    "inspect artifact {}: {error}",
                    logical.display()
                )));
            }
        };
        Ok(Self {
            label: label.into(),
            logical,
            resolved,
            physical,
        })
    }
}

fn prospective_parent(path: &Path) -> Result<PathBuf, FsError> {
    let mut cursor = path;
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = cursor
                    .file_name()
                    .ok_or_else(|| FsError::new("artifact has no existing ancestor"))?;
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| FsError::new("artifact has no existing ancestor"))?;
            }
            Err(error) => {
                return Err(FsError::new(format!("resolve artifact parent: {error}")));
            }
        }
    }
}

/// Resolve and snapshot a configuration file through a bounded symlink chain.
///
/// # Errors
///
/// Returns [`FsError`] for relative paths, loops, missing files, non-regular
/// targets, oversized files, or unstable reads.
pub fn resolve_config_route(path: &Path, limit: usize) -> Result<ConfigRoute, FsError> {
    let logical = normalize_absolute(path)?;
    let mut current = logical.clone();
    let mut links = Vec::new();
    let mut anchors = Vec::new();
    let mut visited = BTreeSet::new();
    for _ in 0..40 {
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            FsError::new(format!(
                "inspect configuration {}: {error}",
                current.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            if !visited.insert((metadata.dev(), metadata.ino())) {
                return Err(FsError::new("configuration symlink loop"));
            }
            let target = fs::read_link(&current)
                .map_err(|error| FsError::new(format!("read configuration symlink: {error}")))?;
            links.push(LinkSnapshot {
                path: current.clone(),
                target: target.clone(),
                device: metadata.dev(),
                inode: metadata.ino(),
                mode: metadata.mode() & 0o7777,
                uid: metadata.uid(),
            });
            let parent = current
                .parent()
                .ok_or_else(|| FsError::new("configuration symlink has no parent"))?;
            let anchor = snapshot_directory(parent)?;
            let next = if target.is_absolute() {
                target
            } else {
                anchor.physical.join(target)
            };
            anchors.push(anchor);
            current = normalize_absolute(&next)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(FsError::new(format!(
                "configuration is not a regular file: {}",
                current.display()
            )));
        }
        let parent_path = current
            .parent()
            .ok_or_else(|| FsError::new("configuration has no parent"))?;
        let parent = snapshot_directory(parent_path)?;
        let target = parent.physical.join(
            current
                .file_name()
                .ok_or_else(|| FsError::new("configuration has no file name"))?,
        );
        let file = stable_snapshot(&target, limit)?;
        return Ok(ConfigRoute {
            logical,
            target,
            links,
            anchors,
            parent,
            file,
            limit,
        });
    }
    Err(FsError::new("configuration symlink chain exceeds 40 links"))
}

/// Read an exact regular file through an `O_NOFOLLOW` descriptor.
///
/// # Errors
///
/// Returns [`FsError`] when the file is a symlink, non-regular, too large, or
/// changes while being read.
pub fn stable_snapshot(path: &Path, limit: usize) -> Result<FileSnapshot, FsError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| FsError::new(format!("open {} without symlinks: {error}", path.display())))?;
    let file = File::from(descriptor);
    let before = file
        .metadata()
        .map_err(|error| FsError::new(format!("inspect {}: {error}", path.display())))?;
    let mut file = retain_if_active_lock(file, &before, path)?;
    if !before.is_file() {
        return Err(FsError::new(format!(
            "not a regular file: {}",
            path.display()
        )));
    }
    if before.len() > limit as u64 {
        return Err(FsError::new(format!(
            "{} exceeds {limit} bytes",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| FsError::new(format!("read {}: {error}", path.display())))?;
    if bytes.len() > limit {
        return Err(FsError::new(format!(
            "{} exceeds {limit} bytes",
            path.display()
        )));
    }
    let after = file
        .metadata()
        .map_err(|error| FsError::new(format!("reinspect {}: {error}", path.display())))?;
    let file_identity = identity(&before, &bytes);
    if identity(&after, &bytes) != file_identity {
        return Err(FsError::new(format!(
            "file changed while reading: {}",
            path.display()
        )));
    }
    Ok(FileSnapshot {
        path: path.to_path_buf(),
        identity: file_identity,
        bytes,
    })
}

/// Require all artifact claims to have distinct paths and physical identities.
///
/// # Errors
///
/// Returns [`FsError`] with both labels when any two claims alias.
pub fn reject_aliases(claims: &[ArtifactClaim]) -> Result<(), FsError> {
    for (index, left) in claims.iter().enumerate() {
        for right in &claims[index + 1..] {
            let aliases = left.logical == right.logical
                || left.resolved == right.resolved
                || left
                    .physical
                    .zip(right.physical)
                    .is_some_and(|(left, right)| left == right);
            if aliases {
                return Err(FsError::new(format!(
                    "artifact alias between {} and {}",
                    left.label, right.label
                )));
            }
        }
    }
    Ok(())
}

/// Create and synchronize a private stage beside its destination.
///
/// # Errors
///
/// Returns [`FsError`] when no unique stage can be created or written.
pub(crate) fn stage_file(
    destination: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<StagedFile, FsError> {
    let parent = destination
        .parent()
        .ok_or_else(|| FsError::new("stage destination has no parent"))?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| FsError::new("stage destination has no UTF-8 file name"))?;
    for _ in 0..128 {
        let sequence = STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".{name}.mcp-swap-stage-{}-{sequence}",
            std::process::id()
        ));
        match open(
            &path,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(descriptor) => {
                let mut file = File::from(descriptor);
                let created = file.metadata().map_err(|error| {
                    FsError::new(format!(
                        "inspect new stage {}: {error}; retained stage",
                        path.display()
                    ))
                })?;
                let mut stage = StagedFile {
                    path,
                    identity: identity(&created, &[]),
                    limit: bytes.len(),
                    armed: true,
                    complete: false,
                };
                let result = (|| {
                    file.write_all(bytes).map_err(|error| {
                        FsError::new(format!("write stage {}: {error}", stage.path.display()))
                    })?;
                    file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))
                        .map_err(|error| FsError::new(format!("set stage mode: {error}")))?;
                    file.sync_all().map_err(|error| {
                        FsError::new(format!(
                            "synchronize stage {}: {error}",
                            stage.path.display()
                        ))
                    })?;
                    sync_directory(parent)
                })();
                if let Err(error) = result {
                    return Err(stage.cleanup_error(error));
                }
                let snapshot = match stable_snapshot(stage.path(), bytes.len()) {
                    Ok(snapshot) => snapshot,
                    Err(error) => return Err(stage.cleanup_error(error)),
                };
                if snapshot.identity.device != stage.identity.device
                    || snapshot.identity.inode != stage.identity.inode
                {
                    let error = FsError::new(format!(
                        "stage changed after creation: {}",
                        stage.path.display()
                    ));
                    return Err(stage.cleanup_error(error));
                }
                stage.identity = snapshot.identity;
                stage.complete = true;
                return Ok(stage);
            }
            Err(error) if error == rustix::io::Errno::EXIST => {}
            Err(error) => {
                return Err(FsError::new(format!(
                    "create stage {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Err(FsError::new("could not allocate a unique stage path"))
}

/// Move a source to an absent destination without replacement.
///
/// # Errors
///
/// Returns [`FsError`] when the source cannot be moved or the destination is
/// already present.
pub fn rename_no_replace(source: &Path, destination: &Path) -> Result<(), FsError> {
    let source_parent = source
        .parent()
        .ok_or_else(|| FsError::new("source has no parent"))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| FsError::new("destination has no parent"))?;
    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE).map_err(|error| {
        FsError::new(format!(
            "move {} to absent {}: {error}",
            source.display(),
            destination.display()
        ))
    })?;
    sync_directory(destination_parent)?;
    if source_parent != destination_parent {
        sync_directory(source_parent)?;
    }
    Ok(())
}

/// Remove a file only while its full authenticated identity still matches.
///
/// # Errors
///
/// Returns [`FsError`] when the file changed or cannot be removed.
pub fn remove_exact(path: &Path, expected: &FileIdentity, limit: usize) -> Result<(), FsError> {
    remove_exact_with_hook(path, expected, limit, &mut || Ok(()))
}

fn remove_exact_with_hook(
    path: &Path,
    expected: &FileIdentity,
    limit: usize,
    hook: &mut dyn FnMut() -> Result<(), FsError>,
) -> Result<(), FsError> {
    let current = stable_snapshot(path, limit)?;
    if current.identity != *expected {
        return Err(FsError::new(format!(
            "file changed before removal: {}",
            path.display()
        )));
    }
    let parent = path
        .parent()
        .ok_or_else(|| FsError::new("removed path has no parent"))?;
    let retained_dir = create_retained_directory(path)?;
    let retained = retained_dir.join("artifact");
    if let Err(error) = hook() {
        remove_empty_retained_directory(&retained_dir)?;
        return Err(error);
    }
    if let Err(error) = rename_no_replace(path, &retained) {
        remove_empty_retained_directory(&retained_dir)?;
        return Err(error);
    }
    let moved = stable_snapshot(&retained, limit)?;
    if moved.identity != *expected {
        return restore_unexpected_removal(path, &retained, &retained_dir);
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(FsError::new(format!(
                "destination appeared during removal; retained original at {}",
                retained_dir.display()
            )));
        }
        Err(error) => {
            return Err(FsError::new(format!(
                "inspect removed destination {}: {error}; retained original at {}",
                path.display(),
                retained_dir.display()
            )));
        }
    }
    if stable_snapshot(&retained, limit)?.identity != *expected {
        return Err(FsError::new(format!(
            "retained file changed before removal: {}",
            retained.display()
        )));
    }
    fs::remove_file(&retained)
        .map_err(|error| FsError::new(format!("remove {}: {error}", retained.display())))?;
    sync_directory(&retained_dir)?;
    remove_empty_retained_directory(&retained_dir)?;
    sync_directory(parent)
}

fn create_retained_directory(path: &Path) -> Result<PathBuf, FsError> {
    let parent = path
        .parent()
        .ok_or_else(|| FsError::new("retained path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| FsError::new("retained path has no UTF-8 file name"))?;
    for _ in 0..128 {
        let sequence = RETAIN_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let retained = parent.join(format!(
            ".{name}.mcp-swap-retained-{}-{sequence}",
            std::process::id()
        ));
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&retained) {
            Ok(()) => {
                sync_directory(parent)?;
                return Ok(retained);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(FsError::new(format!(
                    "create retained directory {}: {error}",
                    retained.display()
                )));
            }
        }
    }
    Err(FsError::new("could not allocate a retained directory"))
}

fn restore_unexpected_removal(
    path: &Path,
    retained: &Path,
    retained_dir: &Path,
) -> Result<(), FsError> {
    match rename_no_replace(retained, path) {
        Ok(()) => {
            remove_empty_retained_directory(retained_dir)?;
            Err(FsError::new(format!(
                "file changed during removal and was restored: {}",
                path.display()
            )))
        }
        Err(_) => Err(FsError::new(format!(
            "file changed during removal; retained at {}",
            retained_dir.display()
        ))),
    }
}

fn remove_empty_retained_directory(path: &Path) -> Result<(), FsError> {
    fs::remove_dir(path).map_err(|error| {
        FsError::new(format!(
            "remove retained directory {}: {error}",
            path.display()
        ))
    })?;
    sync_directory(
        path.parent()
            .ok_or_else(|| FsError::new("retained directory has no parent"))?,
    )
}

/// Synchronize one directory after a namespace mutation.
///
/// # Errors
///
/// Returns [`FsError`] when the directory cannot be opened or synchronized.
pub fn sync_directory(path: &Path) -> Result<(), FsError> {
    let directory = File::open(path)
        .map_err(|error| FsError::new(format!("open directory {}: {error}", path.display())))?;
    directory
        .sync_all()
        .map_err(|error| FsError::new(format!("sync directory {}: {error}", path.display())))
}

/// Compute a lowercase SHA-256 digest.
#[must_use]
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn identity(metadata: &fs::Metadata, bytes: &[u8]) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
        links: metadata.nlink(),
        size: metadata.len(),
        sha256: digest(bytes),
    }
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, FsError> {
    if !path.is_absolute() {
        return Err(FsError::new(format!(
            "path must be absolute: {}",
            path.display()
        )));
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => normalized = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) => return Err(FsError::new("unsupported path prefix")),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::{FsError, remove_exact_with_hook, stable_snapshot};

    #[test]
    fn exact_removal_restores_a_replacement_that_arrives_after_authentication() {
        let root = tempdir().expect("temporary directory");
        let path = root.path().join("artifact");
        fs::write(&path, b"owned").expect("owned file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("owned mode");
        let expected = stable_snapshot(&path, 1024)
            .expect("owned snapshot")
            .identity;
        let mut hook = || {
            fs::remove_file(&path).map_err(|error| FsError::new(error.to_string()))?;
            fs::write(&path, b"human").map_err(|error| FsError::new(error.to_string()))?;
            Ok(())
        };

        remove_exact_with_hook(&path, &expected, 1024, &mut hook)
            .expect_err("replacement must not be removed");

        assert_eq!(fs::read(path).expect("replacement survives"), b"human");
    }
}
