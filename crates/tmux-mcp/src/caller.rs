//! Where this server is running, and what it must not disrupt or destroy.
//!
//! tmux sets `TMUX` and `TMUX_PANE` in every process it starts, so an MCP
//! server launched from a pane can say which pane that is. Two things are
//! built on that: pane listings say which pane is the caller's own, pane input
//! refuses to reach it, and teardown tools refuse to destroy it.
//!
//! A pane id is only unique within one tmux server. `%1` on the socket this
//! process was started from and `%1` on the socket it was asked about are
//! different panes, so every comparison here weighs the socket as well.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use libtmux::ServerGeneration;

use serde::Serialize;

/// How a pane relates to the process answering the request.
///
/// Three values rather than a boolean, because "not the caller's pane" and
/// "there is no caller" are different answers and an agent acts differently on
/// each.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// This process is not running inside tmux, so no pane is its own.
    #[default]
    Unknown,
    /// Confirmed: the same tmux server, and the same pane.
    #[serde(rename = "self")]
    Own,
    /// Some other pane, or a pane this crate cannot prove is the caller's.
    Other,
}

/// The tmux pane hosting this process, as its environment describes it.
///
/// `TMUX` carries `socket_path,server_pid,session_number`; `TMUX_PANE` carries
/// the pane id. Either both variables form one complete identity or the
/// context is malformed; only two absent variables mean detached operation.
#[derive(Clone, Eq, PartialEq)]
pub struct CallerIdentity {
    socket: Option<PathBuf>,
    server_pid: Option<u32>,
    session_id: Option<String>,
    pane_id: Option<String>,
    malformed: bool,
}

impl fmt::Debug for CallerIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallerIdentity")
            .field("socket", &self.socket.as_ref().map(|_| "<redacted>"))
            .field("server_pid", &self.server_pid)
            .field("session_id", &self.session_id)
            .field("pane_id", &self.pane_id)
            .field("malformed", &self.malformed)
            .finish()
    }
}

fn canonical_number(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

fn canonical_pane_id(value: &str) -> bool {
    value
        .parse::<libtmux::PaneId>()
        .is_ok_and(|parsed| parsed.to_string() == value)
}

impl CallerIdentity {
    /// Read the identity from this process's environment.
    ///
    /// Returns `None` when neither variable is set, which is the ordinary case
    /// for a server started outside tmux.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Self::from_values(std::env::var_os("TMUX"), std::env::var_os("TMUX_PANE"))
    }

    /// Build an identity from explicit values, as the environment would give
    /// them.
    ///
    /// Separate from [`CallerIdentity::from_env`] so the parsing can be tested
    /// without a process-wide environment, which no test can hold alone.
    #[must_use]
    pub fn from_values(tmux: Option<OsString>, pane: Option<OsString>) -> Option<Self> {
        let detached = tmux.is_none() && pane.is_none();
        if detached {
            return None;
        }

        let parsed = tmux
            .and_then(|value| value.into_string().ok())
            .zip(pane.and_then(|value| value.into_string().ok()))
            .and_then(|(tmux, pane_id)| {
                let (socket, suffix) = tmux.rsplit_once(',')?;
                let (socket, server_pid) = socket.rsplit_once(',')?;
                let socket = PathBuf::from(socket);
                let server_pid = canonical_number(server_pid)
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value != 0)?;
                let session = canonical_number(suffix)?;
                if !socket.is_absolute() || !canonical_pane_id(&pane_id) {
                    return None;
                }
                Some((socket, server_pid, format!("${session}"), pane_id))
            });

        Some(match parsed {
            Some((socket, server_pid, session_id, pane_id)) => Self {
                socket: Some(socket),
                server_pid: Some(server_pid),
                session_id: Some(session_id),
                pane_id: Some(pane_id),
                malformed: false,
            },
            None => Self {
                socket: None,
                server_pid: None,
                session_id: None,
                pane_id: None,
                malformed: true,
            },
        })
    }

    /// The pane this process runs in, when tmux named one.
    #[must_use]
    pub fn pane_id(&self) -> Option<&str> {
        self.pane_id.as_deref()
    }

    /// The socket path this process was started from, when tmux named one.
    #[must_use]
    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    pub(crate) fn resolve_on<'a>(
        &'a self,
        server_socket: &Path,
        generation: ServerGeneration,
        panes: &[libtmux::Pane],
    ) -> Result<Option<&'a str>, &'static str> {
        if self.malformed {
            return Err("malformed");
        }
        let (Some(socket), Some(server_pid), Some(session_id), Some(pane_id)) = (
            self.socket.as_deref(),
            self.server_pid,
            self.session_id.as_deref(),
            self.pane_id.as_deref(),
        ) else {
            return Err("incomplete");
        };
        if !same_path(socket, server_socket) {
            return Ok(None);
        }
        if server_pid != generation.pid() {
            return Err("stale server process");
        }
        if !panes.iter().any(|pane| {
            pane.id().to_string() == pane_id && pane.session_id().to_string() == session_id
        }) {
            return Err("unresolved pane and session");
        }
        Ok(Some(pane_id))
    }

    /// Whether a pane on the given server is provably this process's own.
    ///
    /// Positive only on a confirmed socket match, because this drives an
    /// annotation an agent reads as fact. Anything less is [`Relation::Other`]:
    /// a bare pane-id match across two sockets is the false positive this
    /// whole module exists to avoid.
    #[must_use]
    pub fn relation_to(&self, pane_id: &str, server_socket: Option<&Path>) -> Relation {
        if self.malformed {
            return Relation::Other;
        }
        if self.pane_id.as_deref() != Some(pane_id) {
            return Relation::Other;
        }
        if same_socket(self.socket.as_deref(), server_socket) {
            Relation::Own
        } else {
            Relation::Other
        }
    }

    /// Whether this process might be running on the given server.
    ///
    /// Malformed context and an unreadable target socket remain protected.
    /// Complete identities on a different physical socket are foreign; a
    /// socket basename alone never authenticates a caller.
    #[must_use]
    pub fn may_be_on(&self, server_socket: Option<&Path>, _socket_name: Option<&str>) -> bool {
        if self.malformed {
            return true;
        }
        let Some(caller) = self.socket.as_deref() else {
            // No socket to compare. If tmux named a pane, assume it is here.
            return self.pane_id.is_some();
        };
        let Some(target) = server_socket else {
            return true;
        };
        if same_path(caller, target) {
            return true;
        }
        false
    }
}

/// Whether two socket paths name the same socket, when both are known.
fn same_socket(caller: Option<&Path>, target: Option<&Path>) -> bool {
    match (caller, target) {
        (Some(caller), Some(target)) => same_path(caller, target),
        _ => false,
    }
}

/// Compare two paths, resolving symlinks when the filesystem allows it.
///
/// Temporary directories are routinely symlinked, so the resolved forms are
/// what matter. Resolution can fail — the socket may have been removed since —
/// and an exact match is still a match, so failure falls back to comparing the
/// paths as written.
fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    #[test]
    fn any_present_caller_variable_is_not_detached() {
        for (tmux, pane) in [
            (Some(OsString::new()), Some(OsString::new())),
            (Some(OsString::from("/tmp/socket,1,0")), None),
            (None, Some(OsString::from("%0"))),
            (
                Some(OsString::from("/tmp/socket,not-a-pid,0")),
                Some(OsString::from("%0")),
            ),
        ] {
            let caller = CallerIdentity::from_values(tmux, pane)
                .expect("only two absent variables represent a detached caller");
            assert!(caller.malformed);
        }
    }

    #[test]
    fn a_pane_without_a_socket_is_malformed() {
        let caller = CallerIdentity::from_values(None, Some(OsString::from("%3")))
            .expect("a present variable is not detached");
        assert!(caller.malformed);
        assert_eq!(caller.pane_id(), None);
    }

    use super::*;

    fn identity(tmux: &str, pane: &str) -> CallerIdentity {
        let caller = CallerIdentity::from_values(Some(tmux.into()), Some(pane.into()))
            .unwrap_or_else(|| unreachable!("both values are present"));
        assert!(!caller.malformed, "fixture identity is complete");
        caller
    }

    #[test]
    fn an_absent_environment_is_no_identity() {
        assert_eq!(CallerIdentity::from_values(None, None), None);
    }

    #[test]
    fn a_pane_without_tmux_is_not_usable_identity() {
        let caller = CallerIdentity::from_values(None, Some("%3".into()));
        let caller = caller.unwrap_or_else(|| unreachable!("a pane is present"));

        assert!(caller.malformed);
        assert_eq!(caller.pane_id(), None);
        assert_eq!(caller.socket(), None);
    }

    #[test]
    fn tmux_carries_socket_pid_and_session() {
        let caller = identity("/tmp/tmux-1000/a,comma,48188,10", "%3");

        assert_eq!(caller.socket(), Some(Path::new("/tmp/tmux-1000/a,comma")));
        assert_eq!(caller.server_pid, Some(48188));
        assert_eq!(caller.session_id.as_deref(), Some("$10"));
        assert_eq!(caller.pane_id(), Some("%3"));
    }

    #[test]
    fn caller_fields_require_the_exact_tmux_encoding() {
        for (tmux, pane) in [
            ("/tmp/socket,0,0", "%0"),
            ("/tmp/socket,01,0", "%0"),
            ("/tmp/socket,1,00", "%0"),
            ("/tmp/socket,1,$0", "%0"),
            ("/tmp/socket,1,0", "%00"),
            ("relative/socket,1,0", "%0"),
            ("/tmp/socket,1,0,extra", "%0"),
        ] {
            let caller = CallerIdentity::from_values(Some(tmux.into()), Some(pane.into()))
                .expect("present context");
            assert!(caller.malformed, "{tmux} {pane}");
        }
    }

    #[test]
    fn debug_surfaces_redact_the_socket_path() {
        let path = "/tmp/libtmux-rs-test/caller-debug.sock";
        let caller = identity(&format!("{path},48188,10"), "%3");
        let builder = crate::TmuxTools::builder(
            libtmux::Server::builder()
                .socket_path("/tmp/libtmux-rs-test/caller-target.sock")
                .build()
                .expect("an inert server builds"),
        )
        .caller(Some(caller.clone()));
        let surfaces = [
            format!("{caller:?}"),
            format!("{builder:?}"),
            format!("{:?}", builder.build()),
        ];

        for surface in surfaces {
            assert!(surface.contains("CallerIdentity"), "{surface}");
            assert!(!surface.contains(path), "{surface}");
        }
    }

    #[test]
    fn a_truncated_tmux_value_is_malformed() {
        let caller = CallerIdentity::from_values(Some("/tmp/sock".into()), Some("%1".into()))
            .expect("present context");

        assert!(caller.malformed);
        assert_eq!(caller.socket(), None);
    }

    #[test]
    fn extra_fields_make_the_context_malformed() {
        let caller =
            CallerIdentity::from_values(Some("/tmp/sock,1,1,extra".into()), Some("%1".into()))
                .expect("present context");

        assert!(caller.malformed);
        assert_eq!(caller.socket(), None);
    }

    #[test]
    fn the_same_pane_on_the_same_socket_is_the_callers_own() {
        let caller = identity("/tmp/sock,1,0", "%1");

        assert_eq!(
            caller.relation_to("%1", Some(Path::new("/tmp/sock"))),
            Relation::Own
        );
    }

    #[test]
    fn the_same_pane_id_on_another_socket_is_not() {
        let caller = identity("/tmp/sock-a,1,0", "%1");

        assert_eq!(
            caller.relation_to("%1", Some(Path::new("/tmp/sock-b"))),
            Relation::Other,
            "a pane id is only unique within one server"
        );
    }

    #[test]
    fn an_unprovable_socket_annotates_as_other() {
        let caller = CallerIdentity::from_values(None, Some("%1".into()));
        let caller = caller.unwrap_or_else(|| unreachable!("a pane is present"));

        assert_eq!(
            caller.relation_to("%1", Some(Path::new("/tmp/sock"))),
            Relation::Other,
            "the annotation states fact, so it declines what it cannot prove"
        );
    }

    #[test]
    fn a_different_pane_is_other() {
        let caller = identity("/tmp/sock,1,0", "%1");

        assert_eq!(
            caller.relation_to("%2", Some(Path::new("/tmp/sock"))),
            Relation::Other
        );
    }

    #[test]
    fn a_mangled_socket_is_no_evidence_rather_than_contrary_evidence() {
        // tmux writes an absolute path. Anything else reached this process
        // through something that damaged it, and reading it as "a different
        // server" would clear the way to killing the caller's own pane.
        for mangled in ["garbage-with-no-commas", "relative/path,1,0", "..,1,0"] {
            let caller = CallerIdentity::from_values(Some(mangled.into()), Some("%1".into()))
                .expect("present context");

            assert!(caller.malformed);
            assert_eq!(
                caller.socket(),
                None,
                "{mangled} should not parse as a socket"
            );
            assert!(
                caller.may_be_on(Some(Path::new("/tmp/tmux-1000/default")), Some("default")),
                "{mangled} must leave the guard cautious"
            );
            assert_eq!(
                caller.relation_to("%1", Some(Path::new("/tmp/tmux-1000/default"))),
                Relation::Other,
                "{mangled} proves nothing, so the annotation declines"
            );
        }
    }

    #[test]
    fn the_guard_blocks_what_the_annotation_declines() {
        let caller = CallerIdentity::from_values(None, Some("%1".into()));
        let caller = caller.unwrap_or_else(|| unreachable!("a pane is present"));

        assert!(
            caller.may_be_on(Some(Path::new("/tmp/sock")), Some("default")),
            "with no socket to compare, a kill must assume the worst"
        );
    }

    #[test]
    fn the_guard_blocks_when_the_target_socket_is_unreadable() {
        let caller = identity("/tmp/sock,1,0", "%1");

        assert!(caller.may_be_on(None, None));
    }

    #[test]
    fn the_guard_does_not_authenticate_a_basename_match() {
        let caller = identity("/private/tmp/tmux-1000/work,1,0", "%1");

        assert!(!caller.may_be_on(Some(Path::new("/tmp/tmux-1000/work")), Some("work")));
    }

    #[test]
    fn the_guard_clears_an_unrelated_server() {
        let caller = identity("/tmp/tmux-1000/default,1,0", "%1");

        assert!(
            !caller.may_be_on(Some(Path::new("/tmp/tmux-1000/other")), Some("other")),
            "a different socket by both path and name is a different server"
        );
    }
}
