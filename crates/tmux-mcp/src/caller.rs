//! Where this server is running, and what it must therefore not destroy.
//!
//! tmux sets `TMUX` and `TMUX_PANE` in every process it starts, so an MCP
//! server launched from a pane can say which pane that is. Two things are
//! built on that: pane listings say which pane is the caller's own, and direct
//! kill tools and destructive plan operations refuse it.
//!
//! A pane id is only unique within one tmux server. `%1` on the socket this
//! process was started from and `%1` on the socket it was asked about are
//! different panes, so every comparison here weighs the socket as well.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
/// `TMUX` carries `socket_path,server_pid,session_id`; `TMUX_PANE` carries the
/// pane id. Both are read tolerantly: tmux writes them, but a shell between
/// tmux and this process may not have passed both along, and a partial
/// identity is still worth having.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallerIdentity {
    socket: Option<PathBuf>,
    pane_id: Option<String>,
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
        // An empty variable is not a value. Shells and process managers export
        // these routinely, and `TMUX_PANE=""` read as a pane id would produce
        // an identity that matches no pane -- which is worse than having none,
        // because it looks like the caller is known and simply somewhere else.
        let pane_id = pane
            .and_then(|value| value.into_string().ok())
            .filter(|value| !value.is_empty());
        let tmux = tmux
            .and_then(|value| value.into_string().ok())
            .filter(|value| !value.is_empty());
        if tmux.is_none() && pane_id.is_none() {
            return None;
        }

        // `TMUX` is `socket_path,server_pid,session_id`. Only the socket is
        // read: the pid names a process nothing here compares against, and the
        // session is where this process started rather than where its pane is
        // now, which a pane moved between sessions would make a lie.
        //
        // tmux always writes an absolute path there. A value that is not one
        // has been mangled by something in between, and a mangled socket is
        // not evidence of a *different* server — it is no evidence at all.
        // Dropping it is what routes this identity into the cautious branch of
        // `may_be_on` rather than letting a garbled variable clear the way to
        // killing the caller's own pane.
        let socket = tmux.as_deref().and_then(|tmux| {
            tmux.split(',')
                .next()
                .filter(|field| !field.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        });

        Some(Self { socket, pane_id })
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

    /// Whether a pane on the given server is provably this process's own.
    ///
    /// Positive only on a confirmed socket match, because this drives an
    /// annotation an agent reads as fact. Anything less is [`Relation::Other`]:
    /// a bare pane-id match across two sockets is the false positive this
    /// whole module exists to avoid.
    #[must_use]
    pub fn relation_to(&self, pane_id: &str, server_socket: Option<&Path>) -> Relation {
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
    /// Deliberately the opposite bias to [`CallerIdentity::relation_to`]. This
    /// answers "may I destroy things here", so every case it cannot resolve
    /// answers yes: an identity with no socket, a server whose socket cannot
    /// be read, or a socket whose path differs but whose name matches. A false
    /// positive costs one refused command that the operator can run by hand. A
    /// false negative kills the session the agent is talking through.
    ///
    /// The name-only fallback catches a real divergence rather than a
    /// hypothetical one: `$TMUX_TMPDIR` can differ between this process and
    /// the shell that started it, leaving two correct paths to one socket.
    #[must_use]
    pub fn may_be_on(&self, server_socket: Option<&Path>, socket_name: Option<&str>) -> bool {
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
        caller
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == socket_name.unwrap_or("default"))
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
    fn an_empty_variable_is_not_an_identity() {
        // A process manager that exports TMUX_PANE without a value would
        // otherwise produce a caller whose pane id matches nothing, which
        // reads as "known, and elsewhere" rather than "not known".
        assert!(
            CallerIdentity::from_values(Some(OsString::from("")), Some(OsString::from("")))
                .is_none(),
        );
        assert!(CallerIdentity::from_values(None, Some(OsString::from(""))).is_none());
        assert!(CallerIdentity::from_values(Some(OsString::from("")), None).is_none());
    }

    #[test]
    fn a_pane_without_a_socket_is_still_an_identity() {
        // Losing TMUX but keeping TMUX_PANE is what a shell that re-execs can
        // leave behind, and the pane is still worth protecting.
        let caller = CallerIdentity::from_values(None, Some(OsString::from("%3")))
            .expect("a pane alone identifies something");
        assert_eq!(caller.pane_id(), Some("%3"));
    }

    use super::*;

    fn identity(tmux: &str, pane: &str) -> CallerIdentity {
        CallerIdentity::from_values(Some(tmux.into()), Some(pane.into()))
            .unwrap_or_else(|| unreachable!("both values are present"))
    }

    #[test]
    fn an_absent_environment_is_no_identity() {
        assert_eq!(CallerIdentity::from_values(None, None), None);
    }

    #[test]
    fn a_pane_without_tmux_is_still_an_identity() {
        let caller = CallerIdentity::from_values(None, Some("%3".into()));
        let caller = caller.unwrap_or_else(|| unreachable!("a pane is present"));

        assert_eq!(caller.pane_id(), Some("%3"));
        assert_eq!(caller.socket(), None);
    }

    #[test]
    fn tmux_carries_socket_pid_and_session() {
        let caller = identity("/tmp/tmux-1000/default,48188,10", "%3");

        assert_eq!(caller.socket(), Some(Path::new("/tmp/tmux-1000/default")));
        assert_eq!(caller.pane_id(), Some("%3"));
    }

    #[test]
    fn a_truncated_tmux_value_keeps_what_it_has() {
        let caller = identity("/tmp/sock", "%1");

        assert_eq!(caller.socket(), Some(Path::new("/tmp/sock")));
    }

    #[test]
    fn extra_fields_do_not_disturb_the_socket() {
        let caller = identity("/tmp/sock,1,$1,extra", "%1");

        assert_eq!(caller.socket(), Some(Path::new("/tmp/sock")));
    }

    #[test]
    fn the_same_pane_on_the_same_socket_is_the_callers_own() {
        let caller = identity("/tmp/sock,1,$0", "%1");

        assert_eq!(
            caller.relation_to("%1", Some(Path::new("/tmp/sock"))),
            Relation::Own
        );
    }

    #[test]
    fn the_same_pane_id_on_another_socket_is_not() {
        let caller = identity("/tmp/sock-a,1,$0", "%1");

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
        let caller = identity("/tmp/sock,1,$0", "%1");

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
        for mangled in ["garbage-with-no-commas", "relative/path,1,$0", "..,1,$0"] {
            let caller = identity(mangled, "%1");

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
        let caller = identity("/tmp/sock,1,$0", "%1");

        assert!(caller.may_be_on(None, None));
    }

    #[test]
    fn the_guard_accepts_a_name_match_when_paths_diverge() {
        // Two correct paths to one socket, which is what $TMUX_TMPDIR
        // divergence between this process and its parent shell produces.
        let caller = identity("/private/tmp/tmux-1000/work,1,$0", "%1");

        assert!(caller.may_be_on(Some(Path::new("/tmp/tmux-1000/work")), Some("work")));
    }

    #[test]
    fn the_guard_clears_an_unrelated_server() {
        let caller = identity("/tmp/tmux-1000/default,1,$0", "%1");

        assert!(
            !caller.may_be_on(Some(Path::new("/tmp/tmux-1000/other")), Some("other")),
            "a different socket by both path and name is a different server"
        );
    }
}
