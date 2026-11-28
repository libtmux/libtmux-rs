//! Dispatch onto a control-mode connection instead of a process.
//!
//! Control mode wraps every command in its own `%begin`/`%end` block, so a
//! connection keeps the one property that makes a process worth spawning:
//! each command reports its own outcome. That is what lets the typed API move
//! onto it whole rather than through a separate set of methods.

use std::sync::Arc;

use crate::command::{CommandRequest, CommandResult, ProcessStatus};
use crate::control::ControlSender;
use crate::internal::executor::{DispatchFuture, DispatchSpan, Executor, ShutdownFuture};

/// Runs commands over a connection someone else opened.
pub(crate) struct ControlModeExecutor {
    sender: Arc<ControlSender>,
    output_limits: crate::OutputLimits,
}

impl ControlModeExecutor {
    pub(crate) fn new(sender: ControlSender, output_limits: crate::OutputLimits) -> Self {
        Self {
            sender: Arc::new(sender),
            output_limits,
        }
    }
}

impl Executor for ControlModeExecutor {
    fn native_server(&self) -> Option<&crate::Server> {
        Some(self.sender.native_server())
    }
    fn control_line_limit(&self) -> Option<usize> {
        Some(self.sender.line_limit())
    }
    fn checked_control_version(&self) -> Option<&crate::TmuxVersion> {
        Some(self.sender.version())
    }
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        if let Err(error) = request.require_shared_control() {
            return DispatchFuture::new(async move { Err(error) });
        }
        let sender = Arc::clone(&self.sender);
        let limits = self.output_limits;
        let span = DispatchSpan::new(&request, "control");
        let request_id = request.request_id();
        let summary = request.summary().clone();
        let sensitive_input = summary.sensitive_argument_count() > 0;
        let commands = request.command_count();
        let line = request.into_control_line();

        span.run(async move {
            let Some(line) = line else {
                return Err(crate::Error::control_mode_unrepresentable());
            };
            let block = sender
                .send_line(line, sensitive_input, commands, limits)
                .await?;

            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            for input in block.blocks() {
                if input.succeeded() {
                    if input.output().len() > limits.max_stdout_bytes.saturating_sub(stdout.len()) {
                        return Err(crate::Error::OutputLimitExceeded {
                            request_id: request_id.get(),
                            command: summary,
                            stream: "stdout",
                            limit: limits.max_stdout_bytes,
                        });
                    }
                    stdout.extend_from_slice(input.output());
                } else {
                    if input.output().len() > limits.max_stderr_bytes.saturating_sub(stderr.len()) {
                        return Err(crate::Error::OutputLimitExceeded {
                            request_id: request_id.get(),
                            command: summary,
                            stream: "stderr",
                            limit: limits.max_stderr_bytes,
                        });
                    }
                    stderr.extend_from_slice(input.output());
                }
            }

            Ok(CommandResult::new(
                request_id,
                summary,
                ProcessStatus::from_block_outcome(block.succeeded()),
                stdout,
                stderr,
            ))
        })
    }

    fn shutdown(&self) -> ShutdownFuture {
        // The connection belongs to whoever attached it. Closing it from a
        // routed handle would end a caller's event stream as a side effect of
        // dropping a server object.
        ShutdownFuture::new(async { Ok(()) })
    }

    fn renders_control_line(&self) -> bool {
        true
    }
}
