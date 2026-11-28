use std::ffi::OsString;
use std::path::Path;

use crate::{
    Error, NewSession, PaneId, ServerGeneration, ServerIdentity, SessionId, TmuxVersion, WindowId,
    framed, parse_number,
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
///
/// # Examples
///
/// ```
/// use libtmux_core::{NewSession, SessionKey};
///
/// let created = NewSession::new("work")
///     .decode("/example/socket".as_ref(), b"123 456 %7 $8 @9\n")?;
/// let session: SessionKey = created.session_key();
/// assert_eq!(session.id().to_string(), "$8");
/// # Ok::<(), libtmux_core::Error>(())
/// ```
pub type SessionKey = ObjectKey<SessionId>;
/// A window observed on one daemon, independent of its links into sessions.
///
/// # Examples
///
/// ```
/// use libtmux_core::{NewSession, WindowKey};
///
/// let created = NewSession::new("work")
///     .decode("/example/socket".as_ref(), b"123 456 %7 $8 @9\n")?;
/// let window: WindowKey = created.window_key();
/// assert_eq!(window.id().to_string(), "@9");
/// # Ok::<(), libtmux_core::Error>(())
/// ```
pub type WindowKey = ObjectKey<WindowId>;
/// A pane observed on one daemon generation.
///
/// # Examples
///
/// ```
/// use libtmux_core::{NewSession, PaneKey};
///
/// let created = NewSession::new("work")
///     .decode("/example/socket".as_ref(), b"123 456 %7 $8 @9\n")?;
/// let pane: &PaneKey = created.pane();
/// assert_eq!(pane.id().to_string(), "%7");
/// # Ok::<(), libtmux_core::Error>(())
/// ```
pub type PaneKey = ObjectKey<PaneId>;

impl<I> ObjectKey<I> {
    /// Restore an identity from an observation on the named daemon.
    #[must_use]
    pub const fn new(server: ServerIdentity, generation: ServerGeneration, id: I) -> Self {
        Self {
            server,
            generation,
            id,
        }
    }

    /// The object's tmux ID.
    #[must_use]
    pub fn id(&self) -> I
    where
        I: Clone,
    {
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

    /// Prepare removal of this session on the daemon that created it.
    ///
    /// Both drivers run this queue with a non-starting client at the receipt's
    /// endpoint. If a new daemon reuses the socket and session ID, the guard
    /// stops the queue before tmux reaches `kill-session`.
    pub fn kill_session_command(&self) -> crate::CommandChain {
        crate::CommandChain::new(self.generation().queue_guard()).then(
            crate::Command::new("kill-session")
                .arg("-t")
                .arg(self.session.as_ref()),
        )
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
        Self::encode(socket, key, false, None)
    }

    /// Encode a generation guard, dead-state observation and pane capture.
    ///
    /// The dead-state conditional is hook-free and emits one additional empty
    /// frame when the pane is retained after its process exits. The native
    /// client must be qualified to emit a nonempty capture frame, including a
    /// line feed when partial capture has no bytes. The observation precedes
    /// capture; it is not an atomic process-liveness snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongEndpoint`] before dispatch when the key belongs to
    /// another captured socket path.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::{Capture, NewSession};
    /// use std::path::Path;
    ///
    /// let socket = Path::new("/example/socket");
    /// let receipt = NewSession::new("work").decode(socket, b"123 456 %7 $8 @9\n")?;
    /// let arguments = Capture::arguments_with_dead(socket, receipt.pane())?;
    /// assert!(arguments.iter().any(|word| word == "#{pane_dead}"));
    /// assert!(arguments.iter().any(|word| word == "capture-pane"));
    /// # Ok::<(), libtmux_core::Error>(())
    /// ```
    #[doc(hidden)]
    pub fn arguments_with_dead(socket: &Path, key: &PaneKey) -> Result<Vec<OsString>, Error> {
        Self::encode(socket, key, true, None)
    }

    /// Fold an admitted version into the existing generation guard without another frame.
    #[doc(hidden)]
    pub fn arguments_with_dead_at_version(
        socket: &Path,
        key: &PaneKey,
        version: &TmuxVersion,
    ) -> Result<Vec<OsString>, Error> {
        version.ensure_supported()?;
        Self::encode(socket, key, true, Some(version))
    }

    fn encode(
        socket: &Path,
        key: &PaneKey,
        with_dead: bool,
        version: Option<&TmuxVersion>,
    ) -> Result<Vec<OsString>, Error> {
        if socket.as_os_str() != key.server.socket_path().as_os_str() {
            return Err(Error::WrongEndpoint);
        }
        let pane = key.id.to_string();
        let pid = key.generation.pid();
        let started = key.generation.unix_start_time();
        let mut predicate = format!(
            "#{{&&:#{{==:#{{pane_id}},{pane}}},#{{&&:#{{==:#{{pid}},{pid}}},#{{==:#{{start_time}},{started}}}}}}}"
        );
        if let Some(version) = version {
            predicate = format!("#{{&&:{predicate},#{{==:#{{version}},{}}}}}", version.raw());
        }
        let mut arguments: Vec<_> = [
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
        ]
        .map(OsString::from)
        .into();
        if with_dead {
            arguments.extend(
                [
                    "if-shell",
                    "-F",
                    "-t",
                    &pane,
                    "#{pane_dead}",
                    "if-shell -F 1 '' ''",
                    "",
                    ";",
                ]
                .map(OsString::from),
            );
        }
        arguments.extend(["capture-pane", "-p", "-t", &pane].map(OsString::from));
        Ok(arguments)
    }

    fn after_guard(status: Option<i32>, stdout: &[u8]) -> Result<(u64, framed::Frames<'_>), Error> {
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
        Ok((guard.number, rest))
    }

    pub(crate) fn decode_frames(status: Option<i32>, stdout: &[u8]) -> Result<Vec<u8>, Error> {
        let (previous, frames) = Self::after_guard(status, stdout)?;
        let (capture, rest) = frames.take(Some(previous))?;
        let bytes = capture.success_bytes()?;
        if !rest.followups_succeeded(capture.number) {
            return Err(Error::FramedRead);
        }
        Ok(bytes.to_vec())
    }

    pub(crate) fn decode_frames_with_dead(
        status: Option<i32>,
        stdout: &[u8],
    ) -> Result<(bool, Vec<u8>), Error> {
        let (previous, frames) = Self::after_guard(status, stdout)?;
        let (condition, frames) = frames.take(Some(previous))?;
        if !condition.success_bytes()?.is_empty() {
            return Err(Error::FramedRead);
        }
        let (candidate, frames) = frames.take(Some(condition.number))?;
        let (dead, capture, rest) = if candidate.success_bytes()?.is_empty() {
            let (capture, rest) = frames.take(Some(candidate.number))?;
            (true, capture, rest)
        } else {
            (false, candidate, frames)
        };
        let bytes = capture.success_bytes()?;
        if bytes.is_empty() || !rest.followups_succeeded(capture.number) {
            return Err(Error::FramedRead);
        }
        Ok((dead, bytes.to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVALID_CAPTURE_FOLLOWUPS: &[(&str, &[u8])] = &[
        ("failed hook", b"%begin 1 6 0\nhook\n%error 1 6 0\n"),
        ("unfinished hook", b"%begin 1 6 0\nhook\n"),
        ("unframed hook", b"hook\n"),
        ("invalid header", b"%begin 1 6 1\nhook\n%end 1 6 1\n"),
        ("reused command", b"%begin 1 3 0\n%end 1 3 0\n"),
        ("wrong close", b"%begin 1 6 0\n%end 1 7 0\n"),
        ("incomplete notice", b"%config-error private"),
        (
            "duplicate hook close inside a later hook",
            b"%begin 1 6 0\n%end 1 6 0\n%begin 1 7 0\n%end 1 6 0\n%end 1 7 0\n",
        ),
        (
            "duplicate hook error inside a later hook",
            b"%begin 1 6 0\n%end 1 6 0\n%begin 1 7 0\n%error 1 6 0\n%end 1 7 0\n",
        ),
    ];

    fn frame(number: u64, payload: &[u8], success: bool) -> Vec<u8> {
        let mut bytes = format!("%begin 1 {number} 0\n").into_bytes();
        bytes.extend_from_slice(payload);
        let close = if success { "end" } else { "error" };
        bytes.extend_from_slice(format!("%{close} 1 {number} 0\n").as_bytes());
        bytes
    }

    fn dead_capture(dead: bool, screen: &[u8], followups: &[u8]) -> Vec<u8> {
        let mut bytes = [frame(2, b"", true), frame(3, b"", true)].concat();
        if dead {
            bytes.extend(frame(4, b"", true));
        }
        bytes.extend(frame(5, screen, true));
        bytes.extend_from_slice(followups);
        bytes.extend_from_slice(b"%exit\n");
        bytes
    }

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
    fn capture_requires_complete_successful_unique_followup_frames() {
        let action = b"%begin 1 2 0\n%end 1 2 0\n%begin 1 3 0\nscreen\n%end 1 3 0\n";
        for &(case, tail) in INVALID_CAPTURE_FOLLOWUPS {
            let output = [action.as_slice(), tail, b"%exit\n"].concat();
            assert!(
                matches!(
                    Capture::decode_frames(Some(0), &output),
                    Err(Error::FramedRead)
                ),
                "{case}",
            );
        }
    }

    #[test]
    fn dead_capture_arguments_preserve_the_guard_and_hook_free_branch() {
        let socket = Path::new("/example/socket");
        let receipt = NewSession::new("work")
            .decode(socket, b"123 456 %7 $8 @9\n")
            .unwrap();
        let original = Capture::arguments(socket, receipt.pane()).unwrap();
        let arguments = Capture::arguments_with_dead(socket, receipt.pane()).unwrap();
        assert_eq!(arguments[..10], original[..10]);
        assert_eq!(arguments[18..], original[10..]);
        assert_eq!(
            arguments[10..18],
            [
                "if-shell",
                "-F",
                "-t",
                "%7",
                "#{pane_dead}",
                "if-shell -F 1 '' ''",
                "",
                ";"
            ]
            .map(OsString::from),
        );
        let version = TmuxVersion::parse_output(b"tmux 3.3a\n").unwrap();
        let guarded =
            Capture::arguments_with_dead_at_version(socket, receipt.pane(), &version).unwrap();
        assert_eq!(guarded.len(), arguments.len());
        assert_eq!(guarded[..6], arguments[..6]);
        assert_eq!(guarded[7..], arguments[7..]);
        assert_eq!(
            guarded[6],
            OsString::from(format!(
                "#{{&&:{},#{{==:#{{version}},3.3a}}}}",
                arguments[6].to_str().unwrap()
            )),
        );
        let unsupported = TmuxVersion::parse_output(b"tmux 3.2a\n").unwrap();
        let error = Capture::arguments_with_dead_at_version(socket, receipt.pane(), &unsupported)
            .unwrap_err();
        assert_eq!(error.effect(), crate::Effect::NotSubmitted);
        assert!(matches!(error, Error::Version(_)));
        assert!(matches!(
            Capture::arguments_with_dead(Path::new("/another/socket"), receipt.pane()),
            Err(Error::WrongEndpoint),
        ));
    }

    #[test]
    fn dead_capture_returns_only_screen_bytes_and_the_ordered_dead_observation() {
        let hooks = b"%begin 1 6 0\nprivate-hook\n%end 1 6 0\n%begin 1 7 0\n%end 9 99 0\n%end 1 7 0\n%sessions-changed\n";
        for dead in [false, true] {
            for screen in [&b"\n"[..], b"screen\n", b"%pause %7\n%end 9 99 0\n\xff\0\n"] {
                assert_eq!(
                    Capture::decode_frames_with_dead(Some(0), &dead_capture(dead, screen, hooks))
                        .unwrap(),
                    (dead, screen.to_vec()),
                );
            }
        }
        let historical = [
            frame(2, b"", true),
            frame(3, b"", true),
            b"%exit\n".to_vec(),
        ]
        .concat();
        assert_eq!(Capture::decode_frames(Some(0), &historical).unwrap(), b"");
    }

    #[test]
    fn dead_capture_refuses_failed_prologues_and_incomplete_shapes() {
        let guard = frame(2, b"", true);
        let condition = frame(3, b"", true);
        let marker = frame(4, b"", true);
        let capture = frame(5, b"screen\n", true);
        for (case, mut bytes) in [
            (
                "nonempty guard",
                [
                    frame(2, b"guard\n", true),
                    condition.clone(),
                    capture.clone(),
                ]
                .concat(),
            ),
            (
                "failed guard",
                [frame(2, b"", false), condition.clone(), capture.clone()].concat(),
            ),
            (
                "nonempty condition",
                [
                    guard.clone(),
                    frame(3, b"condition\n", true),
                    capture.clone(),
                ]
                .concat(),
            ),
            (
                "failed condition",
                [guard.clone(), frame(3, b"", false), capture.clone()].concat(),
            ),
            (
                "missing condition",
                [guard.clone(), capture.clone()].concat(),
            ),
            (
                "missing capture",
                [guard.clone(), condition.clone()].concat(),
            ),
            (
                "empty capture",
                [guard.clone(), condition.clone(), frame(5, b"", true)].concat(),
            ),
            (
                "empty dead capture",
                [
                    guard.clone(),
                    condition.clone(),
                    marker.clone(),
                    frame(5, b"", true),
                ]
                .concat(),
            ),
            (
                "extra marker",
                [
                    guard.clone(),
                    condition.clone(),
                    marker.clone(),
                    frame(5, b"", true),
                    frame(6, b"screen\n", true),
                ]
                .concat(),
            ),
            (
                "failed marker",
                [
                    guard.clone(),
                    condition.clone(),
                    frame(4, b"", false),
                    capture.clone(),
                ]
                .concat(),
            ),
            (
                "failed capture",
                [
                    guard.clone(),
                    condition.clone(),
                    frame(5, b"screen\n", false),
                ]
                .concat(),
            ),
            (
                "failed dead capture",
                [guard, condition, marker, frame(5, b"screen\n", false)].concat(),
            ),
        ] {
            bytes.extend_from_slice(b"%exit\n");
            assert!(
                matches!(
                    Capture::decode_frames_with_dead(Some(0), &bytes),
                    Err(Error::FramedRead)
                ),
                "{case}"
            );
        }
    }

    #[test]
    fn dead_capture_keeps_process_failure_distinct_from_complete_guard_refusal() {
        for dead in [false, true] {
            for status in [None, Some(1)] {
                assert!(matches!(
                    Capture::decode_frames_with_dead(status, &dead_capture(dead, b"screen\n", b"")),
                    Err(Error::FramedRead),
                ));
            }
        }
        let refused = b"%begin 1 2 0\nsyntax error\n%error 1 2 0\n%exit\n";
        assert!(matches!(
            Capture::decode_frames_with_dead(Some(1), refused),
            Err(Error::StalePane)
        ));
        let unconfirmed = [
            refused.strip_suffix(b"%exit\n").unwrap(),
            &frame(3, b"", true),
            b"%exit\n",
        ]
        .concat();
        assert!(matches!(
            Capture::decode_frames_with_dead(Some(1), &unconfirmed),
            Err(Error::FramedRead)
        ));
    }

    #[test]
    fn dead_capture_refuses_duplicate_closes_and_invalid_hook_tails() {
        for dead in [false, true] {
            for &(case, tail) in INVALID_CAPTURE_FOLLOWUPS {
                assert!(
                    matches!(
                        Capture::decode_frames_with_dead(
                            Some(0),
                            &dead_capture(dead, b"screen\n", tail)
                        ),
                        Err(Error::FramedRead),
                    ),
                    "{case}"
                );
            }
            for number in [2, 3, 4, 5] {
                if number == 4 && !dead {
                    continue;
                }
                for close in ["end", "error"] {
                    let screen = format!("%{close} 1 {number} 0\n");
                    assert!(
                        matches!(
                            Capture::decode_frames_with_dead(
                                Some(0),
                                &dead_capture(dead, screen.as_bytes(), b"")
                            ),
                            Err(Error::FramedRead),
                        ),
                        "{dead} {close} {number}"
                    );
                }
            }
        }
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
