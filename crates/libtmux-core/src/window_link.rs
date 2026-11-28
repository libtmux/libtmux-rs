use crate::{Command, CommandChain, SessionId, WindowKey};

/// An observed placement of a window at one session index.
///
/// Several links may refer to the same window. Renumbering a session can leave
/// a different window at an index that was valid when the link was read.
///
/// # Examples
///
/// ```
/// use libtmux_core::{NewSession, WindowLinkSnapshot};
/// let created = NewSession::new("work").decode(
///     "/example/socket".as_ref(), b"123 456 %7 $8 @9\n"
/// )?;
/// let link = WindowLinkSnapshot::new(created.session(), 1, created.window_key());
/// assert_eq!(link.window().id(), created.window());
/// assert_eq!(link.index(), 1);
/// # Ok::<(), libtmux_core::Error>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WindowLinkSnapshot {
    session: SessionId,
    index: i32,
    window: WindowKey,
}

impl WindowLinkSnapshot {
    /// Record a placement observed in a tmux listing.
    #[must_use]
    pub const fn new(session: SessionId, index: i32, window: WindowKey) -> Self {
        Self {
            session,
            index,
            window,
        }
    }

    /// The session that contained this link.
    #[must_use]
    pub const fn session(&self) -> &SessionId {
        &self.session
    }

    /// The observed index in that session.
    #[must_use]
    pub const fn index(&self) -> i32 {
        self.index
    }

    /// The underlying window, shared by every link to it.
    #[must_use]
    pub const fn window(&self) -> &WindowKey {
        &self.window
    }

    /// Address the observed session index.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}:{}", self.session, self.index)
    }

    /// Remove this link if the same daemon still holds the window at this index.
    ///
    /// Run the returned chain with a non-starting client at the observed endpoint.
    pub fn unlink_command(&self) -> CommandChain {
        let target = self.target();
        let placement = Command::new("if-shell")
            .arg("-F")
            .arg("-t")
            .arg(target.clone())
            .arg(format!(
                "#{{&&:#{{==:#{{session_id}},{}}},#{{==:#{{window_id}},{}}}}}",
                self.session,
                self.window.id()
            ))
            .arg("")
            .arg("}");
        CommandChain::new(self.window.generation().queue_guard())
            .then(placement)
            .then(Command::new("unlink-window").arg("-t").arg(target))
    }
}
