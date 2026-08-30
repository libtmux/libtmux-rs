use rmcp::model::ErrorData;

// Every error this server returns carries the same three fields on its `data`,
// so an agent decides what to do next by reading them rather than by matching
// on prose. A pane that has closed and a tmux that is not running both fail;
// the first wants the listing refreshed, the second wants the agent to stop.
//
// * `kind` — a short name for what went wrong.
// * `retryable` — whether repeating the same call unchanged is safe and may
//   succeed.
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
    let retryable = error.is_transient();
    let detail = serde_json::json!({
        "kind": match kind {
            ErrorKind::PartialEffect => "partial_effect",
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
        "retryable": retryable,
        "stale": error.is_object_gone(),
    });
    let message = error.to_string();

    match (kind, retryable) {
        (ErrorKind::PartialEffect, _) | (ErrorKind::Refused, true) => {
            ErrorData::internal_error(message, Some(detail))
        }
        (ErrorKind::ObjectGone | ErrorKind::InvalidInput, _) | (ErrorKind::Refused, false) => {
            ErrorData::invalid_params(message, Some(detail))
        }
        _ => ErrorData::internal_error(message, Some(detail)),
    }
}

fn partial_effect(message: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(
        message.into(),
        Some(serde_json::json!({
            "kind": "partial_effect",
            "retryable": false,
            "stale": false,
        })),
    )
}

pub(super) struct EffectBoundary {
    operation: &'static str,
    effect_seen: bool,
}

impl EffectBoundary {
    pub(super) const fn new(operation: &'static str) -> Self {
        Self {
            operation,
            effect_seen: false,
        }
    }

    pub(super) fn mark(&mut self) {
        self.effect_seen = true;
    }

    pub(super) fn error(&self, error: libtmux::Error) -> ErrorData {
        let error = if self.effect_seen {
            error.after_effect(self.operation)
        } else {
            error
        };
        tmux_error(&error)
    }

    pub(super) fn tmux<T>(&self, result: Result<T, libtmux::Error>) -> Result<T, ErrorData> {
        result.map_err(|error| self.error(error))
    }

    pub(super) fn local(&self, message: impl Into<String>) -> ErrorData {
        debug_assert!(self.effect_seen);
        partial_effect(message)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use libtmux::test::TestServer;
    use libtmux::{Command, CommandChain, DispatchLimits, ErrorKind, Server};
    use rmcp::model::ErrorCode;

    use super::{EffectBoundary, tmux_error};

    #[test]
    fn an_effect_boundary_changes_only_later_failures() {
        let mut boundary = EffectBoundary::new("send_keys");
        let first = boundary.error(libtmux::Error::RuntimeNested);
        assert_eq!(first.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(
            first.data.expect("the first error carries detail")["kind"],
            "invalid_input",
        );

        boundary.mark();
        let later = boundary.error(libtmux::Error::RuntimeNested);
        assert_eq!(later.code, ErrorCode::INTERNAL_ERROR);
        let detail = later.data.expect("the later error carries detail");
        assert_eq!(detail["kind"], "partial_effect", "{detail}");
        assert_eq!(detail["retryable"], false, "{detail}");
        assert_eq!(detail["stale"], false, "{detail}");

        let local = boundary.local("the selected object vanished");
        assert_eq!(local.code, ErrorCode::INTERNAL_ERROR);
        let detail = local.data.expect("the local error carries detail");
        assert_eq!(detail["kind"], "partial_effect", "{detail}");
        assert_eq!(detail["retryable"], false, "{detail}");
        assert_eq!(detail["stale"], false, "{detail}");
    }

    #[tokio::test]
    async fn a_transient_refusal_is_a_server_error() {
        let limits = DispatchLimits::default()
            .max_in_flight(1)
            .acquire_timeout(Some(Duration::from_millis(100)));
        let guard = TestServer::builder()
            .dispatch_limits(limits)
            .start()
            .await
            .expect("tmux starts");
        let limited = guard.server().clone();
        let coordinator = Server::builder()
            .socket_path(guard.socket_path())
            .config_file(guard.server().config_file().expect("the fixture config"))
            .tmux_executable(guard.server().tmux_executable())
            .build()
            .expect("a coordination handle");

        let holding = {
            let server = limited.clone();
            tokio::spawn(async move {
                server
                    .chain(
                        CommandChain::new(
                            Command::new("wait-for").arg("-S").arg("retry-refused-held"),
                        )
                        .then(Command::new("wait-for").arg("retry-refused-release")),
                    )
                    .await
            })
        };
        let held = coordinator
            .wait_for_channel("retry-refused-held", Duration::from_secs(2))
            .await
            .expect("the holding dispatch starts");

        let error = limited
            .cmd(Command::new("list-sessions"))
            .await
            .expect_err("the only dispatch permit is occupied");
        coordinator
            .signal_channel("retry-refused-release")
            .await
            .expect("the holding dispatch is released");
        holding
            .await
            .expect("the holding task finishes")
            .expect("the holding dispatch succeeds");
        coordinator.shutdown().await.expect("the coordinator stops");
        guard.shutdown().await.expect("tmux fixture shuts down");

        assert_eq!(held, libtmux::ChannelWait::Signalled);
        assert_eq!(error.kind(), ErrorKind::Refused);
        let projected = tmux_error(&error);
        assert_eq!(projected.code, ErrorCode::INTERNAL_ERROR);
        let detail = projected.data.expect("the refusal carries detail");
        assert_eq!(detail["kind"], "refused", "{detail}");
        assert_eq!(detail["retryable"], true, "{detail}");
        assert_eq!(detail["stale"], false, "{detail}");
    }
}
