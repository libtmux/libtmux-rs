use rmcp::model::{CallToolResult, ContentBlock, ErrorCode, ErrorData, IntoContents};

/// A tool-execution failure, reported as `isError` tool content.
///
/// MCP separates protocol faults (unknown tool, arguments that do not match
/// the schema -- rejected before a tool body ever runs) from failures a tool
/// body discovers itself. Only the first belongs in the JSON-RPC `error`
/// field; the second is data the model reads and acts on, which is what
/// [`rmcp::model::CallToolResult::is_error`] is for. Every helper below
/// builds one of these instead of a raw [`ErrorData`], so a tool signature
/// that returns `Result<_, ToolError>` cannot surface a tmux or input
/// refusal as a protocol error by construction.
///
/// Public, and re-exported at the crate root: a caller that invokes
/// [`crate::TmuxTools`]'s methods directly, rather than over the wire, gets
/// this type back and needs [`Self::into_error_data`] to read it.
#[derive(Debug)]
pub struct ToolError(ErrorData);

impl From<ErrorData> for ToolError {
    fn from(value: ErrorData) -> Self {
        Self(value)
    }
}

/// The message a model would read, so a direct caller can propagate a tool
/// failure with `?` like any other error.
impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.message)
    }
}

impl std::error::Error for ToolError {}

impl ToolError {
    /// Recover the underlying [`ErrorData`].
    ///
    /// For the one caller that reports a nested tool's own failure inside a
    /// batch item rather than as this tool's failure
    /// ([`crate::TmuxTools::call_read_tools_batch`]), and for a
    /// test that calls a tool directly and inspects the classification a
    /// wire client would otherwise read from `isError` content.
    #[must_use]
    pub fn into_error_data(self) -> ErrorData {
        self.0
    }
}

impl IntoContents for ToolError {
    fn into_contents(self) -> Vec<ContentBlock> {
        let body = serde_json::json!({
            "message": self.0.message,
            "data": self.0.data,
        });
        vec![ContentBlock::text(body.to_string())]
    }
}

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
pub(crate) fn tmux_error(error: &libtmux_tokio::Error) -> ToolError {
    use libtmux_tokio::ErrorKind;

    let kind = error.kind();
    let retryable = error.is_transient();
    let link_gone = matches!(error, libtmux_tokio::Error::LinkGone { .. });
    let wire_kind = match error {
        libtmux_tokio::Error::LinkGone { .. } => "link_gone",
        libtmux_tokio::Error::ServerGenerationChanged { .. } => "server_replaced",
        _ => match kind {
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
    };
    let mut detail = serde_json::json!({
        "kind": wire_kind,
        "retryable": retryable,
        "stale": link_gone || error.is_object_gone(),
    });
    if let Some(effect) = error.operation_effect() {
        detail["effect"] = effect.as_str().into();
    }
    if let Some(receipt) = error.creation_receipt() {
        detail["created"] = serde_json::json!({
            "session_id": receipt.session().as_ref(),
            "window_id": receipt.window().as_ref(),
            "pane_id": receipt.pane().id().as_ref(),
            "server_pid": receipt.generation().pid(),
            "server_start_time": receipt.generation().unix_start_time(),
        });
    }
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
    .into()
}

fn partial_effect(message: impl Into<String>) -> ToolError {
    ErrorData::internal_error(
        message.into(),
        Some(serde_json::json!({
            "kind": "partial_effect",
            "retryable": false,
            "stale": false,
        })),
    )
    .into()
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

    pub(super) fn error(&self, error: libtmux_tokio::Error) -> ToolError {
        let error = if self.effect_seen {
            error.after_effect(self.operation)
        } else {
            error
        };
        tmux_error(&error)
    }

    pub(super) fn tmux<T>(&self, result: Result<T, libtmux_tokio::Error>) -> Result<T, ToolError> {
        result.map_err(|error| self.error(error))
    }

    pub(super) fn tool_error(&self, error: ToolError) -> ToolError {
        if !self.effect_seen {
            return error;
        }
        let mut wrapped = partial_effect(format!(
            "{} changed state before a follow-up failed: {error}",
            self.operation
        ));
        if let Some(detail) = &mut wrapped.0.data {
            detail["cause"] = error.0.data.unwrap_or(serde_json::Value::Null);
        }
        wrapped
    }

    pub(super) fn local(&self, message: impl Into<String>) -> ToolError {
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
pub(crate) fn object_gone(what: &str, id: &str) -> ToolError {
    ErrorData::invalid_params(format!("no {what} {id}"), Some(stale_detail())).into()
}

/// Report state that moved between two calls this server made.
///
/// Not the caller's mistake — the handle was good when it was taken — so the
/// code stays an internal error. The classification is the one for a target
/// that was already gone, because the useful response is the same: look again.
pub(super) fn vanished(message: &str) -> ToolError {
    ErrorData::internal_error(message.to_owned(), Some(stale_detail())).into()
}

/// Report an argument this server will not pass to tmux.
///
/// Nothing about the server needs to change for the next call to work, and
/// nothing has gone stale: the caller has to send something else.
pub(crate) fn bad_input(message: impl Into<String>) -> ToolError {
    ErrorData::invalid_params(
        message.into(),
        Some(serde_json::json!({
            "kind": "invalid_input",
            "retryable": false,
            "stale": false,
        })),
    )
    .into()
}

/// Give a failed result rmcp answered itself the body every tool failure has.
///
/// rmcp reports arguments that do not deserialize as a failed result whose
/// only content is its own message. Every tool here fails through
/// [`ToolError`], whose content is a JSON object carrying `data`, so a failed
/// result without one is rmcp's, and it concerned the arguments.
pub(crate) fn typed_result(mut result: CallToolResult) -> CallToolResult {
    if result.is_error != Some(true) {
        return result;
    }
    let [block] = result.content.as_slice() else {
        return result;
    };
    let Some(text) = block.as_text() else {
        return result;
    };
    let typed = serde_json::from_str::<serde_json::Value>(&text.text)
        .is_ok_and(|body| body.get("data").is_some_and(serde_json::Value::is_object));
    if !typed {
        result.content = bad_input(text.text.clone()).into_contents();
    }
    result
}

/// Give a protocol error rmcp raised before any tool ran the three fields.
pub(crate) fn typed_protocol_error(mut error: ErrorData) -> ErrorData {
    if error.data.is_none() {
        let kind = if error.code == ErrorCode::INVALID_PARAMS {
            "invalid_input"
        } else {
            "internal"
        };
        error.data = Some(serde_json::json!({
            "kind": kind,
            "retryable": false,
            "stale": false,
        }));
    }
    error
}

/// Refuse a tool name this process does not serve.
///
/// The name is either a tool the startup selection left out or no tool at
/// all, and the fix differs: only the operator can offer the first.
pub(crate) fn unoffered_tool(tool: &str, exists: bool) -> ErrorData {
    let message = if exists {
        format!(
            "tool {tool} is not offered: this server's startup selection left it out; the \
             operator can add it with LIBTMUX_TOOLSETS or LIBTMUX_TOOLS"
        )
    } else {
        format!("no tool {tool}")
    };
    bad_input(message).into_error_data()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use libtmux_tokio::test::TestServer;
    use libtmux_tokio::{Command, CommandChain, DispatchLimits, ErrorKind, Server};
    use rmcp::model::ErrorCode;

    use super::{EffectBoundary, tmux_error};

    /// An unsupported daemon is reported with its remedy and a kind an agent
    /// can branch on, not as a transport failure.
    #[test]
    fn a_version_refusal_names_its_remedy() {
        let found = libtmux_tokio::TmuxVersion::parse_output(b"tmux 3.2a\n").expect("parses");
        let refusal: libtmux_tokio::Error = found
            .ensure_supported()
            .expect_err("3.2a is below the floor")
            .into();
        let data = tmux_error(&refusal).into_error_data();
        let detail = data.data.expect("the refusal carries detail");
        assert_eq!(detail["kind"], "unsupported_version", "{detail}");
        assert!(
            data.message.contains("requires tmux 3.3a or newer"),
            "{}",
            data.message
        );
        assert!(
            data.message.contains("Upgrade tmux and restart the server"),
            "{}",
            data.message
        );
    }

    #[test]
    fn an_effect_boundary_changes_only_later_failures() {
        let mut boundary = EffectBoundary::new("send_keys");
        let first = boundary
            .error(libtmux_tokio::Error::UnrecognizedLayout)
            .into_error_data();
        assert_eq!(first.code, ErrorCode::INVALID_PARAMS);
        assert_eq!(
            first.data.expect("the first error carries detail")["kind"],
            "invalid_input",
        );

        boundary.mark();
        let later = boundary
            .error(libtmux_tokio::Error::UnrecognizedLayout)
            .into_error_data();
        assert_eq!(later.code, ErrorCode::INTERNAL_ERROR);
        let detail = later.data.expect("the later error carries detail");
        assert_eq!(detail["kind"], "partial_effect", "{detail}");
        assert_eq!(detail["retryable"], false, "{detail}");
        assert_eq!(detail["stale"], false, "{detail}");

        let local = boundary
            .local("the selected object vanished")
            .into_error_data();
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
                    .chain(CommandChain::new(
                        // The nested queue holds an ordinary permit without
                        // resolving another tmux executable through PATH.
                        Command::new("run-shell")
                            .arg("-C")
                            .arg("wait-for -S retry-refused-held ; wait-for retry-refused-release"),
                    ))
                    .await
            })
        };
        let held = coordinator
            .wait_for_channel("retry-refused-held", Duration::from_secs(2))
            .await
            .expect("the holding dispatch starts");
        assert_eq!(held, libtmux_tokio::ChannelWait::Signalled);
        assert!(
            !holding.is_finished(),
            "the signalled dispatch is still held"
        );

        let refused = limited.cmd(Command::new("list-sessions")).await;
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

        let error = refused.expect_err("the only dispatch permit is occupied");
        assert!(matches!(
            &error,
            libtmux_tokio::Error::Overloaded { in_flight: 1, .. }
        ));
        assert_eq!(error.kind(), ErrorKind::Refused);
        let projected = tmux_error(&error).into_error_data();
        assert_eq!(projected.code, ErrorCode::INTERNAL_ERROR);
        let detail = projected.data.expect("the refusal carries detail");
        assert_eq!(detail["kind"], "refused", "{detail}");
        assert_eq!(detail["retryable"], true, "{detail}");
        assert_eq!(detail["stale"], false, "{detail}");
    }
}
