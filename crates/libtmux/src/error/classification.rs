use std::io;

#[cfg(feature = "control-mode")]
use super::ControlModeErrorKind;
use super::{Error, ErrorKind, ServerGoneKind};

impl Error {
    /// Mark this failure as following an effect that tmux accepted.
    ///
    /// Use a fixed operation name without targets or argument values. Calling
    /// this method on an already marked error leaves its existing operation
    /// intact, so nested composed operations do not obscure the more specific
    /// replay boundary.
    ///
    /// The returned error has [`ErrorKind::PartialEffect`] and
    /// [`Self::is_transient`] returns `false`, regardless of the source.
    #[must_use]
    pub fn after_effect(self, operation: &'static str) -> Self {
        match self {
            Self::AfterEffect { .. } => self,
            source => Self::AfterEffect {
                operation,
                source: Box::new(source),
            },
        }
    }

    /// Return what this failure means for the caller.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::ErrorKind;
    ///
    /// // The shape this exists for: use it if it is there, make it if not.
    /// let session = match server.session("work").await? {
    ///     Some(session) => session,
    ///     None => server.new_session("work").await?,
    /// };
    ///
    /// // And when an operation races something else removing it. The handle
    /// // is cloned because killing consumes one, which is how the crate
    /// // stops you from using a window you just destroyed.
    /// let window = session.new_window("doomed").await?;
    /// let mut stale = window.clone();
    /// window.kill().await?;
    ///
    /// let error = stale.rename("gone").await.expect_err("the window was killed");
    /// assert_eq!(error.kind(), ErrorKind::ObjectGone);
    /// assert!(error.is_object_gone());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::AfterEffect { .. } => ErrorKind::PartialEffect,
            // A replaced daemon reissues ids from the start, so every handle
            // captured from the previous one names something that is not
            // there. That is the same decision as a missing object, and the
            // same branch a caller already writes for one.
            Self::ObjectGone { .. } | Self::ServerGenerationChanged { .. } => ErrorKind::ObjectGone,
            // Not `ObjectGone`: the object may still exist, so a caller must
            // not read this as a reason to drop the handle.
            Self::LinkGone { .. } => ErrorKind::Refused,
            // tmux carried out neither a read nor a change: the client is
            // there but not answering. `Refused` rather than `ObjectGone`
            // so a caller keeps the handle.
            Self::ClientSuspended { .. } => ErrorKind::Refused,
            Self::ServerGone { .. } => ErrorKind::ServerGone,
            Self::CommandFailed { .. }
            | Self::OutputLimitExceeded { .. }
            | Self::Overloaded { .. }
            | Self::SessionExists { .. }
            | Self::OptionRejected { .. } => ErrorKind::Refused,
            Self::Timeout { .. } => ErrorKind::Timeout,
            Self::ExecutableNotFound { .. }
            | Self::InvalidServerConfiguration { .. }
            | Self::RuntimeUnavailable { .. } => ErrorKind::Unreachable,
            // The call is wrong, not the environment: the same future awaited
            // directly would work.
            Self::RuntimeNested => ErrorKind::InvalidInput,
            Self::UnsupportedTmuxVersion { .. }
            | Self::UnsupportedCapability { .. }
            | Self::CapabilityDefective { .. } => ErrorKind::UnsupportedVersion,
            Self::InvalidCommandInput { .. } | Self::ServerMismatch { .. } => {
                ErrorKind::InvalidInput
            }
            #[cfg(feature = "plan")]
            Self::InvalidPlan { .. } => ErrorKind::InvalidInput,
            Self::Spawn { .. }
            | Self::ReadOutput { .. }
            | Self::WaitChild { .. }
            | Self::VersionProbeFailed { .. }
            | Self::ExecutorShutdown { .. }
            | Self::DuplicateRequest { .. }
            | Self::SupervisorLost { .. } => ErrorKind::Transport,
            Self::InvalidVersionOutput { .. }
            | Self::DecodeListing { .. }
            | Self::UnreadableFormatValue { .. } => ErrorKind::Decode,
            #[cfg(feature = "control-mode")]
            Self::ControlModeFrameTooLarge { .. } => ErrorKind::Decode,
            #[cfg(feature = "control-mode")]
            Self::ControlMode { kind, .. } => match kind {
                ControlModeErrorKind::UnrepresentableCommand
                | ControlModeErrorKind::InvalidSubscriptionName => ErrorKind::InvalidInput,
                // A limit was reached and the command was not carried out,
                // which is what `Refused` says. The connection is fine.
                ControlModeErrorKind::Unread => ErrorKind::Refused,
                ControlModeErrorKind::DispatchTimedOut | ControlModeErrorKind::TimedOut => {
                    ErrorKind::Timeout
                }
                ControlModeErrorKind::Transport
                | ControlModeErrorKind::MissingPipes
                | ControlModeErrorKind::Closed => ErrorKind::Transport,
            },
        }
    }

    /// Report whether tmux no longer has the object the call named.
    ///
    /// The most common branch a caller writes, and the one that is easy to
    /// get wrong: an object disappearing is an ordinary race, not a failure
    /// of the request.
    #[must_use]
    pub fn is_object_gone(&self) -> bool {
        self.kind() == ErrorKind::ObjectGone
    }

    /// Report whether retrying the same call unchanged is safe and may succeed.
    ///
    /// `true` means this error proves that the requested mutation did not run,
    /// or came from an operation that only reads state. The condition may need
    /// to clear first: capacity can become available, a client can resume, a
    /// resource-limited spawn can be retried, or a server can answer again.
    ///
    /// `false` does not mean that the handle is unusable. A subprocess timeout,
    /// output-reader failure, child-wait failure, or lost supervisor can leave
    /// the executor ready for another call, but tmux may already have carried
    /// out the first one. Replaying a mutation could duplicate its effect. A
    /// shut down executor and a closed or timed-out control connection are also
    /// false because they require a new server or connection.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Overloaded { .. } | Self::ClientSuspended { .. } => true,
            Self::Spawn { source, .. } => matches!(
                source.kind(),
                io::ErrorKind::Interrupted
                    | io::ErrorKind::WouldBlock
                    | io::ErrorKind::OutOfMemory
                    | io::ErrorKind::ResourceBusy
                    | io::ErrorKind::ExecutableFileBusy
            ),
            Self::ServerGone {
                kind: ServerGoneKind::NotRunning | ServerGoneKind::Unreachable,
                ..
            } => true,
            Self::AfterEffect { .. }
            | Self::InvalidServerConfiguration { .. }
            | Self::InvalidVersionOutput { .. }
            | Self::UnsupportedTmuxVersion { .. }
            | Self::OptionRejected { .. }
            | Self::UnreadableFormatValue { .. }
            | Self::ServerGenerationChanged { .. }
            | Self::OutputLimitExceeded { .. }
            | Self::SessionExists { .. }
            | Self::UnsupportedCapability { .. }
            | Self::CapabilityDefective { .. }
            | Self::VersionProbeFailed { .. }
            | Self::InvalidCommandInput { .. }
            | Self::ServerMismatch { .. }
            | Self::ExecutableNotFound { .. }
            | Self::ExecutorShutdown { .. }
            | Self::DuplicateRequest { .. }
            | Self::ReadOutput { .. }
            | Self::WaitChild { .. }
            | Self::Timeout { .. }
            | Self::SupervisorLost { .. }
            | Self::ObjectGone { .. }
            | Self::LinkGone { .. }
            | Self::RuntimeUnavailable { .. }
            | Self::RuntimeNested
            | Self::ServerGone {
                kind: ServerGoneKind::Lost | ServerGoneKind::Stopped,
                ..
            }
            | Self::CommandFailed { .. }
            | Self::DecodeListing { .. } => false,
            #[cfg(feature = "plan")]
            Self::InvalidPlan { .. } => false,
            #[cfg(feature = "control-mode")]
            Self::ControlMode {
                kind: ControlModeErrorKind::DispatchTimedOut,
                ..
            } => true,
            #[cfg(feature = "control-mode")]
            Self::ControlModeFrameTooLarge { .. } | Self::ControlMode { .. } => false,
        }
    }
}
