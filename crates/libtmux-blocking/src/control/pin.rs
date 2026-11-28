use std::ffi::OsStr;
use std::fs::{File, Permissions};
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::SocketAddr;
use std::path::{Path, PathBuf};

pub(super) struct PinnedEndpoint {
    directory: tempfile::TempDir,
    executable: PathBuf,
}

impl PinnedEndpoint {
    pub(super) fn new(socket: &Path, executable: &OsStr) -> io::Result<Self> {
        let socket = socket.canonicalize()?;
        let metadata = std::fs::symlink_metadata(&socket)?;
        if !metadata.file_type().is_socket() {
            return Err(io::Error::other("control endpoint is not a Unix socket"));
        }
        let parent = socket
            .parent()
            .ok_or_else(|| io::Error::other("socket has no parent"))?;
        let mut failure = None;
        for parent in parent.ancestors() {
            let directory = match tempfile::Builder::new()
                .prefix(".lt-")
                .permissions(Permissions::from_mode(0o700))
                .tempdir_in(parent)
            {
                Ok(directory) => directory,
                Err(error) => {
                    failure = Some(error);
                    continue;
                }
            };
            let link = directory.path().join("s");
            if let Err(error) =
                SocketAddr::from_pathname(&link).and_then(|_| std::fs::hard_link(&socket, &link))
            {
                failure = Some(error);
                continue;
            }
            let linked = std::fs::symlink_metadata(&link)?;
            if !linked.file_type().is_socket()
                || (metadata.dev(), metadata.ino()) != (linked.dev(), linked.ino())
            {
                return Err(io::Error::other("control socket changed while pinning"));
            }
            let mut original = open_executable(executable)?;
            let executable = directory.path().join("client");
            let mut copy = File::options()
                .write(true)
                .create_new(true)
                .open(&executable)?;
            io::copy(&mut original, &mut copy)?;
            drop(copy);
            std::fs::set_permissions(&executable, Permissions::from_mode(0o500))?;
            return Ok(Self {
                directory,
                executable,
            });
        }
        Err(failure.unwrap_or_else(|| io::Error::other("no usable private control socket path")))
    }

    pub(super) fn socket(&self) -> PathBuf {
        self.directory.path().join("s")
    }
    pub(super) fn executable(&self) -> &Path {
        &self.executable
    }
}

fn open_executable(executable: &OsStr) -> io::Result<File> {
    let path = Path::new(executable);
    if path.components().count() > 1 || path.is_absolute() {
        return checked_file(path);
    }
    let mut failure = None;
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        match checked_file(&directory.join(path)) {
            Ok(file) => return Ok(file),
            Err(error) => failure = Some(error),
        }
    }
    Err(failure.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "control executable was not found")
    }))
}

fn checked_file(path: &Path) -> io::Result<File> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "control executable is not an executable file",
        ));
    }
    Ok(file)
}
