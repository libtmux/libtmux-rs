use std::ffi::OsString;
use std::path::Path;

use crate::{
    Error, NewSession, PaneId, ServerGeneration, ServerIdentity, SessionId, WindowId, framed,
    parse_number,
};

const KEY_FORMAT: &str = "#{pid} #{start_time} #{pane_id}";

/// An object ID together with the endpoint and daemon generation that supplied it.
///
/// A daemon may reuse the same ID after a restart. Both execution drivers
/// compare the generation before acting on a key.
///
/// # Examples
///
/// ```
/// use libtmux_core::{NewSession, PaneKey};
/// let created = NewSession::new("work")
///     .decode("/example/socket".as_ref(), b"123 456 %7 $8 @9\n")?;
/// let pane: &PaneKey = created.pane();
/// assert_eq!(pane.id().to_string(), "%7");
/// # Ok::<(), libtmux_core::Error>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ObjectKey<I> {
    server: ServerIdentity,
    generation: ServerGeneration,
    id: I,
}

/// A session observed on one daemon generation.
pub type SessionKey = ObjectKey<SessionId>;
/// A window observed on one daemon, independent of its links into sessions.
pub type WindowKey = ObjectKey<WindowId>;
/// A pane observed on one daemon generation.
pub type PaneKey = ObjectKey<PaneId>;

impl<I> ObjectKey<I> {
    /// Restore an identity from an observation on the named daemon.
    #[must_use]
    pub const fn new(server: ServerIdentity, generation: ServerGeneration, id: I) -> Self {
        Self { server, generation, id }
    }

    /// The object's tmux ID.
    #[must_use]
    pub fn id(&self) -> I where I: Clone {
        self.id.clone()
    }

    /// The daemon generation that supplied the ID.
    #[must_use]
    pub const fn generation(&self) -> ServerGeneration {
        self.generation
    }

    /// The socket endpoint at which the object was observed.
    #[must_use]
    pub const fn server_identity(&self) -> &ServerIdentity {
        &self.server
    }

    fn on_same_daemon<T>(&self, id: T) -> ObjectKey<T> {
        ObjectKey::new(self.server.clone(), self.generation, id)
    }
}

fn decode_key(socket: &Path, row: &[u8]) -> Option<PaneKey> {
    let mut fields = std::str::from_utf8(row).ok()?.split(' ');
    let pid = parse_number(fields.next()?)?;
    let started = parse_number(fields.next()?)?;
    let pane = fields.next()?.parse().ok()?;
    if fields.next().is_some() {
        return None;
    }
    Some(PaneKey {
        server: ServerIdentity::from_socket_path(socket.to_owned()),
        generation: ServerGeneration::from_raw(pid, started),
        id: pane,
    })
}

/// Session, window and pane identities acknowledged by one detached creation.
///
/// All IDs and the daemon generation come from the same creation action. They
/// remain available when a later hook or client exit fails.
///
/// # Examples
///
/// ```
/// use libtmux_core::NewSession;
/// use std::path::Path;
///
/// let receipt = NewSession::new("work")
///     .decode(Path::new("/example/socket"), b"123 456 %7 $8 @9\n")?;
/// assert_eq!(receipt.session().to_string(), "$8");
/// assert_eq!(receipt.window().to_string(), "@9");
/// # Ok::<(), libtmux_core::Error>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedSession {
    session: SessionId,
    window: WindowId,
    pane: PaneKey,
}

impl CreatedSession {
    /// The created session's ID.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session.clone()
    }

    /// The first window's ID.
    #[must_use]
    pub fn window(&self) -> WindowId {
        self.window.clone()
    }

    /// The created session bound to the daemon that acknowledged it.
    #[must_use]
    pub fn session_key(&self) -> SessionKey {
        self.pane.on_same_daemon(self.session())
    }

    /// The created window bound to the same daemon as its first pane.
    #[must_use]
    pub fn window_key(&self) -> WindowKey {
        self.pane.on_same_daemon(self.window())
    }

    /// The generation-stamped key for the first pane.
    #[must_use]
    pub const fn pane(&self) -> &PaneKey {
        &self.pane
    }

    /// The daemon generation reported by the creation action.
    #[must_use]
    pub const fn generation(&self) -> ServerGeneration {
        self.pane.generation
    }
}

impl NewSession {
    /// Decode an identity row after the caller has verified action success.
    ///
    /// Native drivers use [`crate::Reply::creation`] to verify control frames
    /// and preserve creation evidence across later failures.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AfterCreate`] when a successful action did not return
    /// exactly one complete row with a daemon generation and three valid IDs.
    pub fn decode(&self, socket: &Path, reply: &[u8]) -> Result<CreatedSession, Error> {
        let row = reply.strip_suffix(b"\n").ok_or(Error::AfterCreate)?;
        let mut fields = std::str::from_utf8(row)
            .map_err(|_| Error::AfterCreate)?
            .rsplitn(3, ' ');
        let window = fields
            .next()
            .and_then(|id| id.parse().ok())
            .ok_or(Error::AfterCreate)?;
        let session = fields
            .next()
            .and_then(|id| id.parse().ok())
            .ok_or(Error::AfterCreate)?;
        let pane = decode_key(socket, fields.next().ok_or(Error::AfterCreate)?.as_bytes())
            .ok_or(Error::AfterCreate)?;
        Ok(CreatedSession {
            session,
            window,
            pane,
        })
    }
}

/// An exact pane observation that refuses tmux target fallbacks.
///
/// A window linked into several sessions can produce repeated identical rows;
/// inconsistent rows and rows for another pane are refused.
///
/// # Examples
///
/// ```
/// use libtmux_core::{ObservePane, PaneId};
///
/// let pane: PaneId = "%7".parse()?;
/// let arguments = ObservePane::arguments(pane);
/// assert_eq!(arguments[2], "list-panes");
/// # Ok::<(), libtmux_core::IdParseError>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct ObservePane;

impl ObservePane {
    /// Encode a non-starting one-shot client with an exact pane filter.
    #[must_use]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the encoded observation consumes its submitted pane ID"
    )]
    pub fn arguments(pane: PaneId) -> [OsString; 8] {
        [
            "-N",
            "-C",
            "list-panes",
            "-a",
            "-f",
            &format!("#{{==:#{{pane_id}},{pane}}}"),
            "-F",
            KEY_FORMAT,
        ]
        .map(OsString::from)
    }

    pub(crate) fn decode_frames(
        socket: &Path,
        pane: &PaneId,
        status: Option<i32>,
        stdout: &[u8],
    ) -> Result<PaneKey, Error> {
        if status != Some(0) {
            return Err(Error::FramedRead);
        }
        let (listing, _) = framed::first(stdout)?.take(None)?;
        Self::decode_rows(socket, pane, listing.success_bytes()?)
    }

    pub(crate) fn decode_rows(
        socket: &Path,
        pane: &PaneId,
        reply: &[u8],
    ) -> Result<PaneKey, Error> {
        if reply.is_empty() {
            return Err(Error::PaneNotFound(pane.clone()));
        }

        let rows = reply.strip_suffix(b"\n").ok_or(Error::InvalidKey)?;
        let mut observed = None;
        for row in rows.split(|&byte| byte == b'\n') {
            let key = decode_key(socket, row).ok_or(Error::InvalidKey)?;
            if &key.id != pane || observed.as_ref().is_some_and(|prior| prior != &key) {
                return Err(Error::InvalidKey);
            }
            // A pane appears once per session that links its window.
            observed = Some(key);
        }
        observed.ok_or(Error::InvalidKey)
    }
}

/// A pane capture guarded by its endpoint, daemon generation and exact ID.
///
/// The generation check and capture occupy one tmux command queue. Screen bytes
/// are returned only from a complete, unambiguous capture action frame.
///
/// # Examples
///
/// ```
/// use libtmux_core::{Capture, NewSession};
/// use std::path::Path;
///
/// let socket = Path::new("/example/socket");
/// let receipt = NewSession::new("work").decode(socket, b"123 456 %7 $8 @9\n")?;
/// let arguments = Capture::arguments(socket, receipt.pane())?;
/// assert!(arguments.iter().any(|argument| argument == "capture-pane"));
/// # Ok::<(), libtmux_core::Error>(())
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Capture;

impl Capture {
    // A syntax error aborts the queue; an unknown name can resolve through an alias.
    const STALE_BRANCH: &'static str = "}";

    /// Encode a same-queue generation guard followed by a pane capture.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongEndpoint`] before dispatch when the key belongs to
    /// another captured socket path.
    pub fn arguments(socket: &Path, key: &PaneKey) -> Result<Vec<OsString>, Error> {
        if socket.as_os_str() != key.server.socket_path().as_os_str() {
            return Err(Error::WrongEndpoint);
        }
        let pane = key.id.to_string();
        let pid = key.generation.pid();
        let started = key.generation.unix_start_time();
        let predicate = format!(
            "#{{&&:#{{==:#{{pane_id}},{pane}}},#{{&&:#{{==:#{{pid}},{pid}}},#{{==:#{{start_time}},{started}}}}}}}"
        );
        Ok([
            "-N",
            "-C",
            "if-shell",
            "-F",
            "-t",
            &pane,
            &predicate,
            "",
            Self::STALE_BRANCH,
            ";",
            "capture-pane",
            "-p",
            "-t",
            &pane,
        ]
        .map(OsString::from)
        .into())
    }

    pub(crate) fn decode_frames(status: Option<i32>, stdout: &[u8]) -> Result<Vec<u8>, Error> {
        let (guard, rest) = framed::first(stdout)?.take(None)?;
        if status == Some(1)
            && !guard.success
            && guard.bytes == b"syntax error\n"
            && rest.is_empty()
        {
            return Err(Error::StalePane);
        }
        if status != Some(0) || !guard.success_bytes()?.is_empty() {
            return Err(Error::FramedRead);
        }
        let (capture, _) = rest.take(Some(guard.number))?;
        Ok(capture.success_bytes()?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_receipt_binds_shared_ids_generation_and_redacted_endpoint() {
        let receipt = NewSession::new("work")
            .decode(Path::new("/private/socket"), b"123 456 %7 $8 @9\n")
            .unwrap();
        assert_eq!(receipt.session().to_string(), "$8");
        assert_eq!(receipt.window().to_string(), "@9");
        assert_eq!(receipt.pane().id().to_string(), "%7");
        assert_eq!(receipt.generation().pid(), 123);
        assert_eq!(receipt.generation().unix_start_time(), 456);
        assert!(!format!("{receipt:?} {receipt:#?}").contains("/private/socket"));
        assert!(matches!(
            Capture::arguments(Path::new("/another/socket"), receipt.pane()),
            Err(Error::WrongEndpoint)
        ));
    }

    #[test]
    fn acknowledged_creation_does_not_guess_ids_from_malformed_rows() {
        for unreadable in [
            &b""[..],
            b"123 456 %7 $8 @9",
            b"123 456 %7 $8 @bad\n",
            b"+123 456 %7 $8 @9\n",
            b"123 456 %7 $8 @9\n123 456 %7 $8 @9\n",
            b"123 456 %4294967296 $8 @9\n",
            b"123 456 %7 $8 @9 \n",
        ] {
            assert!(matches!(
                NewSession::new("work").decode(Path::new("/socket"), unreadable),
                Err(Error::AfterCreate)
            ));
        }
    }

    #[test]
    fn observation_refuses_target_fallback_and_accepts_identical_link_rows() {
        let pane: PaneId = "%7".parse().unwrap();
        let socket = Path::new("/socket");
        let single = ObservePane::decode_rows(socket, &pane, b"123 456 %7\n").unwrap();
        assert_eq!(
            ObservePane::decode_rows(socket, &pane, b"123 456 %7\n123 456 %7\n").unwrap(),
            single
        );
        assert!(matches!(
            ObservePane::decode_rows(socket, &pane, b""),
            Err(Error::PaneNotFound(found)) if found == pane
        ));
        for malformed in [&b"123 456 %0\n"[..], b"123 456 %7\n124 456 %7\n"] {
            assert!(matches!(
                ObservePane::decode_rows(socket, &pane, malformed),
                Err(Error::InvalidKey)
            ));
        }
    }

    #[test]
    fn capture_keeps_screen_bytes_inside_its_unique_frame() {
        let output = b"%begin 1 2 0\n%end 1 2 0\n%begin 1 4 0\n%pause %0\n%end 0 0 0\n\xff\n\x00\n%end 1 4 0\n%begin 1 5 0\nhook\n%end 1 5 0\n%exit\n";
        assert_eq!(
            Capture::decode_frames(Some(0), output).unwrap(),
            b"%pause %0\n%end 0 0 0\n\xff\n\x00\n"
        );
        assert!(matches!(
            Capture::decode_frames(
                Some(1),
                b"%begin 1 2 0\nsyntax error\n%error 1 2 0\n%exit\n"
            ),
            Err(Error::StalePane)
        ));
    }

    #[test]
    fn capture_refuses_duplicate_terminators_anywhere_in_the_collected_reply() {
        let start = b"%begin 1 2 0\n%end 1 2 0\n%begin 1 3 0\n";
        for rest in [
            &b"first\n%end 1 3 0\nmore\n%end 1 3 0\n%exit\n"[..],
            b"first\n%error 1 3 0\nmore\n%end 1 3 0\n%exit\n",
            b"first\n%end 1 3 0\n%begin 1 4 0\n%end 1 3 0\n%end 1 4 0\n%exit\n",
            b"first\n%end 1 8 0\n%exit\n",
        ] {
            let output = [start.as_slice(), rest].concat();
            assert!(matches!(
                Capture::decode_frames(Some(0), &output),
                Err(Error::FramedRead)
            ));
        }
    }
}
