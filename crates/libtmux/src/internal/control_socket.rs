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
        let pin = self.directory.path().join("s").into_os_string();
        // Keep launch flags, replacing the one captured endpoint selector.
        let mut selected = Vec::with_capacity(global.len() + 2);
        let mut replaced = false;
        let mut index = 0;
        while index < global.len() {
            if global[index] == "-S" || global[index] == "-L" {
                selected.push(OsString::from("-S"));
                selected.push(pin.clone());
                replaced = true;
                index += 2;
            } else {
                selected.push(global[index].clone());
                index += 1;
            }
        }
        // With no selector, tmux was left to pick, and would pick again.
        if !replaced {
            selected.splice(0..0, [OsString::from("-S"), pin]);
        }
        selected
    }

    pub(crate) fn into_directory(self) -> tempfile::TempDir {
        self.directory
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::net::UnixListener;

    use super::PinnedSocket;

    fn arguments(global: &[&str]) -> (Vec<OsString>, OsString) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("server.sock");
        let _listener = UnixListener::bind(&socket).expect("socket inode");
        let pinned = PinnedSocket::new(&socket).expect("socket pins");
        let pin = pinned.directory.path().join("s").into_os_string();
        let global: Vec<OsString> = global.iter().map(OsString::from).collect();
        (pinned.arguments(&global), pin)
    }

    #[test]
    fn a_path_selector_is_replaced_in_place() {
        let (selected, pin) = arguments(&["-S", "/elsewhere.sock", "-f", "tmux.conf", "-u"]);

        assert_eq!(
            selected,
            [
                OsString::from("-S"),
                pin,
                OsString::from("-f"),
                OsString::from("tmux.conf"),
                OsString::from("-u")
            ]
        );
    }

    #[test]
    fn a_name_selector_becomes_the_pinned_path() {
        let (selected, pin) = arguments(&["-L", "named", "-2", "-u"]);

        assert_eq!(
            selected,
            [
                OsString::from("-S"),
                pin,
                OsString::from("-2"),
                OsString::from("-u")
            ]
        );
    }

    #[test]
    fn no_selector_gets_the_pinned_path_in_front() {
        let (selected, pin) = arguments(&["-u"]);

        assert_eq!(selected, [OsString::from("-S"), pin, OsString::from("-u")]);
    }

    #[test]
    fn launch_flags_alone_still_get_the_pinned_path() {
        let (selected, pin) = arguments(&["-N", "-f", "tmux.conf", "-2", "-u"]);

        assert_eq!(selected.len(), 7);
        assert_eq!(selected[..2], [OsString::from("-S"), pin]);
        assert_eq!(
            selected[2..],
            ["-N", "-f", "tmux.conf", "-2", "-u"].map(OsString::from)
        );
    }
}
