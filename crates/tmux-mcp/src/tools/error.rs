use rmcp::model::ErrorData;

// Every error this server returns carries the same three fields on its `data`,
// so an agent decides what to do next by reading them rather than by matching
// on prose. A pane that has closed and a tmux that is not running both fail;
// the first wants the listing refreshed, the second wants the agent to stop.
//
// * `kind` — a short name for what went wrong.
// * `retryable` — whether making the same call again could succeed.
// * `stale` — whether the target is gone, so a listing taken now would say
//   something different.
//
// The JSON-RPC code answers a different question: whose move it is. A caller
// who named a dead pane gets `invalid_params`; a pane that died between two of
// this server's own calls gets `internal_error`. Both are classified `stale`,
// because in both cases looking again is what helps.

/// Convert a tmux failure into a protocol error an agent can act on.
///
/// libtmux already draws the distinctions above, so they are carried through
/// rather than flattened.
pub(super) fn tmux_error(error: &libtmux::Error) -> ErrorData {
    use libtmux::ErrorKind;

    let kind = error.kind();
    let detail = serde_json::json!({
        "kind": match kind {
            ErrorKind::ObjectGone => "object_gone",
            ErrorKind::Refused => "refused",
            ErrorKind::ServerGone => "server_gone",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Unreachable => "unreachable",
            ErrorKind::UnsupportedVersion => "unsupported_version",
            ErrorKind::InvalidInput => "invalid_input",
            ErrorKind::Transport => "transport",
            ErrorKind::Decode => "decode",
            // ErrorKind is #[non_exhaustive]; a kind added upstream is
            // reported rather than mistaken for one of these.
            _ => "other",
        },
        "retryable": error.is_transient(),
        "stale": error.is_object_gone(),
    });
    let message = error.to_string();

    match kind {
        // The caller named something. Whether it is gone or was refused, the
        // request is what needs to change, so it is the caller's error.
        ErrorKind::ObjectGone | ErrorKind::Refused | ErrorKind::InvalidInput => {
            ErrorData::invalid_params(message, Some(detail))
        }
        _ => ErrorData::internal_error(message, Some(detail)),
    }
}

/// The classification for a target that is not where it was said to be.
///
/// Shared by the two ways this server discovers that itself, so a change to
/// what it promises cannot apply to one and not the other.
fn stale_detail() -> serde_json::Value {
    serde_json::json!({
        "kind": "object_gone",
        // Nothing will change on its own to make this id resolve; the caller
        // has to look again and name something else.
        "retryable": false,
        "stale": true,
    })
}

/// Report a target that a listing named but tmux no longer has.
///
/// The `find_*` helpers notice this themselves rather than learning it from
/// libtmux, so they mint the classification directly. An agent should not have
/// to tell the two apart: a pane that vanished between the listing and the call
/// reads the same either way.
pub(super) fn object_gone(what: &str, id: &str) -> ErrorData {
    ErrorData::invalid_params(format!("no {what} {id}"), Some(stale_detail()))
}

/// Report state that moved between two calls this server made.
///
/// Not the caller's mistake — the handle was good when it was taken — so the
/// code stays an internal error. The classification is the one for a target
/// that was already gone, because the useful response is the same: look again.
pub(super) fn vanished(message: &str) -> ErrorData {
    ErrorData::internal_error(message.to_owned(), Some(stale_detail()))
}

/// Report an argument this server will not pass to tmux.
///
/// Nothing about the server needs to change for the next call to work, and
/// nothing has gone stale: the caller has to send something else.
pub(super) fn bad_input(message: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(
        message.into(),
        Some(serde_json::json!({
            "kind": "invalid_input",
            "retryable": false,
            "stale": false,
        })),
    )
}

/// Report a bounded server resource whose active users fill every slot.
pub(super) fn at_capacity(limit: usize) -> ErrorData {
    ErrorData::internal_error(
        format!(
            "background job capacity reached: all {limit} jobs are still starting or running; \
             wait for one to finish or cancel one"
        ),
        Some(serde_json::json!({
            "kind": "capacity",
            "retryable": true,
            "stale": false,
            "capacity": limit,
        })),
    )
}
