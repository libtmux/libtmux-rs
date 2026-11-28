use libtmux_core::{
    CommandChain, CommandKind, CreatedSession, Error, ServerIdentity, WindowLinkSnapshot,
};

use crate::{RequestOptions, Server, before_submission};

impl Server {
    /// Kill the session identified by a creation receipt from either driver.
    ///
    /// # Errors
    ///
    /// Rejects a receipt for another endpoint before contacting tmux. A daemon
    /// replacement stops the queue before it reaches the session.
    pub fn kill_created_session(&self, receipt: &CreatedSession) -> Result<(), Error> {
        self.mutate(
            receipt.pane().server_identity(),
            receipt.kill_session_command(),
            CommandKind::KillSession,
        )
    }

    /// Unlink the window at its observed session index.
    ///
    /// # Errors
    ///
    /// Refuses an endpoint mismatch, daemon replacement, reassigned index
    /// or removal of a window's last link. A failed hook can follow an
    /// applied mutation, so a failed submission must not be replayed blindly.
    pub fn unlink_window(&self, link: &WindowLinkSnapshot) -> Result<(), Error> {
        self.mutate(
            link.window().server_identity(),
            link.unlink_command(),
            CommandKind::UnlinkWindow,
        )
    }

    fn mutate(
        &self,
        endpoint: &ServerIdentity,
        queue: CommandChain,
        operation: CommandKind,
    ) -> Result<(), Error> {
        if endpoint.socket_path() != self.socket {
            return Err(Error::WrongEndpoint);
        }
        let options = RequestOptions::default();
        let (version, _) = self.creation_version(&options).map_err(before_submission)?;
        let (args, _) = queue.into_argv(&["-N".into()]);
        let (output, late) = self.output(version.fence_ordinary_argv(args), options, None)?;
        if output.status.success() && !late {
            return Ok(());
        }
        Err(Error::Command {
            operation,
            status: output.status.code(),
            diagnostic_len: output.stderr.len(),
            late,
        })
    }
}
