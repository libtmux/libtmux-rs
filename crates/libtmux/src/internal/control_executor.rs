//! Dispatch onto a control-mode connection instead of a process.
//!
//! Control mode wraps every command in its own `%begin`/`%end` block, so a
//! connection keeps the one property that makes a process worth spawning:
//! each command reports its own outcome. That is what lets the typed API move
//! onto it whole rather than through a separate set of methods.

use std::sync::Arc;

use crate::command::{CommandRequest, CommandResult, ProcessStatus};
use crate::control::ControlSender;
use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};

/// Runs commands over a connection someone else opened.
pub(crate) struct ControlModeExecutor {
    sender: Arc<ControlSender>,
}

impl ControlModeExecutor {
    pub(crate) fn new(sender: ControlSender) -> Self {
        Self {
            sender: Arc::new(sender),
        }
    }
}

impl Executor for ControlModeExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let sender = Arc::clone(&self.sender);
        let request_id = request.request_id();
        let summary = request.summary().clone();
        let sensitive_input = summary.sensitive_argument_count() > 0;
        let line = request.into_control_line();

        DispatchFuture::new(async move {
            let Some(line) = line else {
                return Err(crate::Error::control_mode_unrepresentable());
            };
            let block = sender.send_line(line, sensitive_input).await?;

            // tmux prints a command's output inside the block, one line at a
            // time, with the trailing newline that separated them removed.
            // Putting it back is what makes the bytes identical to the
            // stdout a process would have written, which every parser above
            // this already reads.
            let mut bytes = Vec::new();
            for line in block.output() {
                bytes.extend_from_slice(line.as_bytes());
                bytes.push(b'\n');
            }

            let succeeded = block.succeeded();
            // A refused command prints its reason where a process would have
            // put it: an `%error` block is stderr, not stdout, and error
            // classification reads stderr.
            let (stdout, stderr) = if succeeded {
                (bytes, Vec::new())
            } else {
                (Vec::new(), bytes)
            };

            Ok(CommandResult::new(
                request_id,
                summary,
                ProcessStatus::from_block_outcome(succeeded),
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
