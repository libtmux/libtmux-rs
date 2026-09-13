use std::{io, marker::PhantomData, os::fd::OwnedFd, rc::Rc};

use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
use rustix::{process::Pid, termios};

use super::{CliError, Result};

pub(super) struct Terminal {
    fd: OwnedFd,
    attributes: termios::Termios,
    owner: Pid,
    child: Option<Pid>,
    old_mask: Option<SigSet>,
    thread: PhantomData<Rc<()>>,
}

impl Terminal {
    pub(super) fn stdin() -> Result<Option<Self>> {
        let owner = match termios::tcgetpgrp(io::stdin()) {
            Ok(owner) => owner,
            Err(rustix::io::Errno::NOTTY) => return Ok(None),
            Err(error) => return Err(io::Error::from(error).into()),
        };
        if owner != rustix::process::getpgrp() {
            return Err(CliError::new(
                "terminal_owner",
                "bootstrap input terminal belongs to another foreground process group",
            ));
        }
        Ok(Some(Self {
            fd: rustix::io::fcntl_dupfd_cloexec(io::stdin(), 3).map_err(io::Error::from)?,
            attributes: termios::tcgetattr(io::stdin()).map_err(io::Error::from)?,
            owner,
            child: None,
            old_mask: None,
            thread: PhantomData,
        }))
    }

    fn wait_foreground(&self) -> Result<()> {
        while termios::tcgetpgrp(&self.fd).map_err(io::Error::from)? != self.owner {
            rustix::process::kill_process_group(self.owner, rustix::process::Signal::TTIN)
                .map_err(io::Error::from)?;
            if termios::tcgetpgrp(&self.fd).map_err(io::Error::from)? != self.owner {
                rustix::process::kill_process_group(self.owner, rustix::process::Signal::STOP)
                    .map_err(io::Error::from)?;
            }
        }
        Ok(())
    }

    pub(super) fn handoff(&mut self, child: Pid) -> Result<()> {
        self.wait_foreground()?;
        let mut signals = SigSet::empty();
        signals.add(Signal::SIGTTOU);
        let mut old = SigSet::empty();
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&signals), Some(&mut old))
            .map_err(io::Error::from)?;
        self.old_mask = Some(old);
        termios::tcsetpgrp(&self.fd, child).map_err(io::Error::from)?;
        self.child = Some(child);
        Ok(())
    }

    pub(super) fn restore(&mut self) -> Result<()> {
        let restored = (|| -> Result<()> {
            let Some(child) = self.child.take() else {
                return Ok(());
            };
            let current = termios::tcgetpgrp(&self.fd).map_err(io::Error::from)?;
            if current != child && current != self.owner {
                return Err(CliError::new(
                    "terminal_owner",
                    "bootstrap terminal foreground ownership changed before restoration",
                ));
            }
            termios::tcsetpgrp(&self.fd, self.owner)
                .and_then(|()| {
                    termios::tcsetattr(&self.fd, termios::OptionalActions::Now, &self.attributes)
                })
                .map_err(io::Error::from)?;
            Ok(())
        })();
        let mask = self.old_mask.take().map_or(Ok(()), |old| {
            pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&old), None).map_err(io::Error::from)
        });
        restored.and(mask.map_err(CliError::from))
    }

    pub(super) fn stopped(&mut self, signal: i32) -> Result<()> {
        let child = self.child.ok_or_else(|| {
            CliError::new(
                "terminal_owner",
                "stopped child no longer owns the input terminal",
            )
        })?;
        if signal != rustix::process::Signal::TTIN.as_raw()
            && signal != rustix::process::Signal::TTOU.as_raw()
        {
            let child_attributes = termios::tcgetattr(&self.fd).map_err(io::Error::from)?;
            self.restore()?;
            rustix::process::kill_process_group(self.owner, rustix::process::Signal::TSTP)
                .map_err(io::Error::from)?;
            self.handoff(child)?;
            termios::tcsetattr(&self.fd, termios::OptionalActions::Now, &child_attributes)
                .map_err(io::Error::from)?;
        } else if termios::tcgetpgrp(&self.fd).map_err(io::Error::from)? != child {
            self.wait_foreground()?;
            termios::tcsetpgrp(&self.fd, child).map_err(io::Error::from)?;
        }
        rustix::process::kill_process_group(child, rustix::process::Signal::CONT)
            .map_err(io::Error::from)?;
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        use std::io::Write;
        if let Err(error) = self.restore() {
            let _ = writeln!(io::stderr(), "terminal restoration failed: {error}");
        }
    }
}
