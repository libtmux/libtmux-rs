use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::SocketAddr;
use std::path::Path;

use crate::Error;

pub(crate) struct PinnedSocket {
    directory: tempfile::TempDir,
}

impl PinnedSocket {
    pub(crate) fn new(socket: &Path) -> Result<Self, Error> {
        let socket = socket.canonicalize().map_err(Error::control_mode)?;
        let metadata = std::fs::symlink_metadata(&socket).map_err(Error::control_mode)?;
        if !metadata.file_type().is_socket() {
            return Err(Error::control_mode(io::Error::other(
                "control endpoint is not a Unix socket",
            )));
        }
        let parent = socket
            .parent()
            .ok_or_else(|| Error::control_mode(io::Error::other("control socket has no parent")))?;
        // A valid original path may leave no room for a peer carrier. Try
        // shorter ancestors, requiring a usable address and the same inode.
        let mut failure = None;
        for parent in parent.ancestors() {
            let directory = match tempfile::Builder::new()
                .prefix(".lt-")
                .permissions(std::fs::Permissions::from_mode(0o700))
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
            let linked = std::fs::symlink_metadata(&link).map_err(Error::control_mode)?;
            if !linked.file_type().is_socket()
                || (metadata.dev(), metadata.ino()) != (linked.dev(), linked.ino())
            {
                return Err(Error::control_mode(io::Error::other(
                    "control socket changed while pinning",
                )));
            }
            return Ok(Self { directory });
        }
        Err(Error::control_mode(failure.unwrap_or_else(|| {
            io::Error::other("no usable private control socket path")
        })))
    }

    pub(crate) fn arguments(&self, global: &[OsString]) -> Vec<OsString> {
        // Keep launch flags, replacing the one captured endpoint selector.
        let mut selected = Vec::with_capacity(global.len());
        let mut index = 0;
        while index < global.len() {
            if global[index] == "-S" || global[index] == "-L" {
                selected.push(OsString::from("-S"));
                selected.push(self.directory.path().join("s").into_os_string());
                index += 2;
            } else {
                selected.push(global[index].clone());
                index += 1;
            }
        }
        selected
    }

    pub(crate) fn into_directory(self) -> tempfile::TempDir {
        self.directory
    }
}
