//! Private process and filesystem lock for swap transactions.

use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use rustix::fs::{FlockOperation, Mode, OFlags, fcntl_lock, open};

use crate::fs::{FileIdentity, FsError, digest};

static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static ACTIVE_LOCK: OnceLock<Mutex<Option<ActiveLock>>> = OnceLock::new();

struct ActiveLock {
    device: u64,
    inode: u64,
    retained: Vec<File>,
}

/// An exclusive lock whose descriptor and directory entry remain authenticated.
pub struct TransactionLock {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
    directory: DirectoryIdentity,
    namespace: DirectoryIdentity,
    _process: MutexGuard<'static, ()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
}

impl std::fmt::Debug for TransactionLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TransactionLock")
            .field("path", &self.path)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl TransactionLock {
    /// Create or open the private lock and wait for exclusive ownership.
    ///
    /// # Errors
    ///
    /// Returns [`FsError`] when the state directory or lock is a symlink,
    /// public, multiply linked, foreign-owned, replaced, or cannot be locked.
    pub fn acquire(state_dir: &Path) -> Result<Self, FsError> {
        let process = PROCESS_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let namespace = state_dir
            .parent()
            .ok_or_else(|| FsError::new("state directory has no parent"))?;
        ensure_private_directory(namespace)?;
        ensure_private_directory(state_dir)?;
        let namespace_identity = directory_identity(namespace)?;
        let directory_identity = directory_identity(state_dir)?;
        let path = state_dir.join("state.lock");
        let existed = fs::symlink_metadata(&path).is_ok();
        let descriptor = open(
            &path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| {
            let detail = if fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                "symlink".to_owned()
            } else {
                error.to_string()
            };
            FsError::new(format!("open lock {}: {detail}", path.display()))
        })?;
        let file = File::from(descriptor);
        if !existed {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                .map_err(|error| FsError::new(format!("set lock mode: {error}")))?;
        }
        fcntl_lock(&file, FlockOperation::LockExclusive)
            .map_err(|error| FsError::new(format!("lock {}: {error}", path.display())))?;
        let identity = lock_identity(&file)?;
        validate_lock_identity(&identity)?;
        *ACTIVE_LOCK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ActiveLock {
            device: identity.device,
            inode: identity.inode,
            retained: Vec::new(),
        });
        let lock = Self {
            path,
            file,
            identity,
            directory: directory_identity,
            namespace: namespace_identity,
            _process: process,
        };
        lock.verify()?;
        Ok(lock)
    }

    /// Require the locked descriptor and current path to name the same file.
    ///
    /// # Errors
    ///
    /// Returns [`FsError`] when the lock path changed while held.
    pub fn verify(&self) -> Result<(), FsError> {
        if directory_identity(&self.directory.path)? != self.directory
            || directory_identity(&self.namespace.path)? != self.namespace
        {
            return Err(FsError::new("state lock directory changed while held"));
        }
        let descriptor = lock_identity(&self.file)?;
        let metadata = fs::symlink_metadata(&self.path)
            .map_err(|error| FsError::new(format!("lock path changed: {error}")))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(FsError::new("lock path changed to a symlink or non-file"));
        }
        let path_identity = identity_from_metadata(&metadata);
        if descriptor != self.identity || path_identity != self.identity {
            return Err(FsError::new("lock path changed while held"));
        }
        validate_lock_identity(&descriptor)?;
        Ok(())
    }

    /// Absolute state-lock path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Identity reserved by the held lock.
    #[must_use]
    pub fn identity(&self) -> &FileIdentity {
        &self.identity
    }
}

impl Drop for TransactionLock {
    fn drop(&mut self) {
        let _ = fcntl_lock(&self.file, FlockOperation::Unlock);
        let mut active = ACTIVE_LOCK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.as_ref().is_some_and(|state| {
            state.device == self.identity.device && state.inode == self.identity.inode
        }) {
            *active = None;
        }
    }
}

pub(crate) fn retain_if_active_lock(
    file: File,
    metadata: &fs::Metadata,
    path: &Path,
) -> Result<File, FsError> {
    let mut active = ACTIVE_LOCK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(state) = active.as_mut() {
        if state.device == metadata.dev() && state.inode == metadata.ino() {
            state.retained.push(file);
            return Err(FsError::new(format!(
                "artifact aliases the active state lock: {}",
                path.display()
            )));
        }
    }
    Ok(file)
}

pub(crate) fn ensure_private_directory(path: &Path) -> Result<(), FsError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(FsError::new(format!("create state directory: {error}")));
                }
            }
            validate_directory(
                path,
                &fs::symlink_metadata(path)
                    .map_err(|error| FsError::new(format!("inspect state directory: {error}")))?,
            )
        }
        Err(error) => Err(FsError::new(format!("inspect state directory: {error}"))),
    }
}

fn validate_directory(path: &Path, metadata: &fs::Metadata) -> Result<(), FsError> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(FsError::new(format!(
            "state directory is a symlink or non-directory: {}",
            path.display()
        )));
    }
    if metadata.mode() & 0o7777 != 0o700 {
        return Err(FsError::new(format!(
            "state directory must have mode 0700: {}",
            path.display()
        )));
    }
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(FsError::new("state directory has a different owner"));
    }
    Ok(())
}

fn directory_identity(path: &Path) -> Result<DirectoryIdentity, FsError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| FsError::new(format!("inspect state directory: {error}")))?;
    validate_directory(path, &metadata)?;
    Ok(DirectoryIdentity {
        path: path.to_path_buf(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
    })
}

fn lock_identity(file: &File) -> Result<FileIdentity, FsError> {
    let metadata = file
        .metadata()
        .map_err(|error| FsError::new(format!("inspect lock descriptor: {error}")))?;
    Ok(identity_from_metadata(&metadata))
}

fn identity_from_metadata(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
        links: metadata.nlink(),
        size: metadata.len(),
        sha256: digest(&[]),
    }
}

fn validate_lock_identity(identity: &FileIdentity) -> Result<(), FsError> {
    if identity.mode != 0o600 {
        return Err(FsError::new("state lock must have mode 0600"));
    }
    if identity.uid != rustix::process::getuid().as_raw() {
        return Err(FsError::new("state lock has a different owner"));
    }
    if identity.links != 1 {
        return Err(FsError::new("state lock must have exactly one hard link"));
    }
    Ok(())
}
