//! Window handles and their snapshot getters.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::formats::TmuxText;
use crate::internal::core::Core;
use crate::internal::listing;
use crate::internal::scoped;
use crate::pane::Pane;
#[cfg(feature = "query")]
use crate::query::{FilterSchema, Filterable};
use crate::session::Session;
use crate::snapshot::WindowProjection;
#[cfg(feature = "query")]
use crate::snapshot::{WindowFields, WindowInfo};
use crate::target::{ServerIdentity, SessionId, WindowId};
use crate::{Command, CommandResult, Error, ObjectKind};

mod navigation;
mod settings;

/// Which way to move focus among a window's panes.
///
/// tmux decides what is "up" from a pane's position on screen rather than
/// from its index, so these follow the layout rather than the pane order.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{PaneDirection, SplitDirection};
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let window = session.active_window().await?.expect("a session has a window");
/// let lower = window.split(SplitDirection::Below).await?;
///
/// // Focus follows the layout, and tmux wraps at the edge rather than
/// // refusing, so this always names a pane.
/// let focused = window.focus_direction(PaneDirection::Below).await?;
/// assert_eq!(focused.id(), lower.id());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum PaneDirection {
    /// The pane above.
    Above,
    /// The pane below.
    Below,
    /// The pane to the left.
    Left,
    /// The pane to the right.
    Right,
}

impl PaneDirection {
    /// The tmux flag that selects this direction.
    const fn flag(self) -> &'static str {
        match self {
            Self::Above => "-U",
            Self::Below => "-D",
            Self::Left => "-L",
            Self::Right => "-R",
        }
    }
}

/// One tmux window, as reached through one session that links it.
///
/// A window is not owned by a single session. `link-window` makes the same
/// underlying window appear in several sessions at once, so discovery returns
/// one `Window` per link rather than per window. Two handles for the same
/// window reached through different sessions are **not** equal: they describe
/// different places in the hierarchy.
///
/// Getters that describe the window itself, such as [`Window::name`], read the
/// window. Getters that describe its place in a session, such as
/// [`Window::index`] and [`Window::is_active`], read the link.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::SplitDirection;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let window = session.active_window().await?.expect("a session has a window");
///
/// // Splitting is detached by default, so focus stays where it was.
/// window.split(SplitDirection::Below).await?;
/// assert_eq!(window.panes().await?.len(), 2);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Window {
    core: Arc<Core>,
    projection: WindowProjection,
}

impl Window {
    /// Build a handle from a hydrated projection.
    pub(crate) const fn new(core: Arc<Core>, projection: WindowProjection) -> Self {
        Self { core, projection }
    }

    /// Find the window this process is running in.
    ///
    /// Resolved through the pane named by `TMUX_PANE`, which is the only
    /// value tmux gives a process that identifies exactly where it is.
    ///
    /// `Ok(None)` means tmux no longer has that pane.
    ///
    /// # Errors
    ///
    /// Returns an error when `TMUX_PANE` is absent or is not a pane ID, or
    /// when the listing fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::Window;
    ///
    /// let session = server.new_session("locating-window").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// // Standing in for the environment tmux gives a process it starts.
    /// let found = Window::from_env_value(server, Some(pane.id().as_ref())).await?;
    ///
    /// assert_eq!(found.expect("the window exists").id(), pane.window_id());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_env(server: &crate::Server) -> Result<Option<Self>, Error> {
        Self::from_env_value(server, std::env::var_os("TMUX_PANE")).await
    }

    /// Find a window from an explicit `TMUX_PANE` value.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is absent or is not a pane ID, or when
    /// the listing fails.
    pub async fn from_env_value(
        server: &crate::Server,
        value: Option<impl AsRef<OsStr>>,
    ) -> Result<Option<Self>, Error> {
        let Some(pane) = Pane::from_env_value(server, value).await? else {
            return Ok(None);
        };

        server.window_by_id(pane.window_id()).await
    }

    /// Return the tmux window identity.
    ///
    /// This is the `@`-prefixed id, which is shared by every link to the same
    /// window.
    #[must_use]
    pub const fn id(&self) -> &WindowId {
        self.projection.window().window_id()
    }

    /// Return the session this handle reached the window through.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        self.projection.link().identity().session_id()
    }

    /// Return the window's index within [`Window::session_id`].
    ///
    /// A linked window can hold a different index in each session, so this is
    /// a property of the link rather than of the window.
    #[must_use]
    pub const fn index(&self) -> i32 {
        self.projection.link().identity().window_index()
    }

    /// Return the window name.
    #[must_use]
    pub fn name(&self) -> &TmuxText {
        self.projection.window().window_name()
    }

    /// Return how many panes the window contains.
    #[must_use]
    pub fn pane_count(&self) -> u32 {
        *self.projection.window().window_panes()
    }

    /// Return the window width in cells.
    #[must_use]
    pub fn width(&self) -> u32 {
        *self.projection.window().window_width()
    }

    /// Return the window height in cells.
    #[must_use]
    pub fn height(&self) -> u32 {
        *self.projection.window().window_height()
    }

    /// Return the window's pane layout string.
    #[must_use]
    pub fn layout(&self) -> &TmuxText {
        self.projection.window().window_layout()
    }

    /// Report whether this window is the active one in its session.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.projection.link().is_active()
    }

    /// Report whether the window is linked into more than one session.
    #[must_use]
    pub const fn is_linked(&self) -> bool {
        self.projection.link().is_linked()
    }

    /// Report whether the window has unseen activity in this session.
    #[must_use]
    pub const fn has_activity(&self) -> bool {
        self.projection.link().has_activity()
    }

    /// Report whether the window rang a bell in this session.
    #[must_use]
    pub const fn has_bell(&self) -> bool {
        self.projection.link().has_bell()
    }

    /// Return when the window last produced output, in seconds since the
    /// Unix epoch.
    ///
    /// tmux stamps this on every byte a pane in the window writes, whatever
    /// the window options say. That is what separates it from
    /// [`Self::has_activity`], which is an alert and stays false unless
    /// `monitor-activity` was turned on -- and it is off by default.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("busy").await?;
    /// let window = session.active_window().await?.expect("a window");
    ///
    /// // A window that has just been made has already produced output.
    /// assert!(window.last_activity() > 0);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn last_activity(&self) -> i64 {
        *self.projection.window().window_activity()
    }

    /// Report whether one of the window's panes is zoomed to fill it.
    ///
    /// This is the window's own flag rather than the pane's, because tmux
    /// only reports `pane_zoomed_flag` from 3.7 onwards.
    #[must_use]
    pub fn is_zoomed(&self) -> bool {
        *self.projection.window().window_zoomed_flag()
    }

    /// Return the identity of the server this window belongs to.
    pub(crate) fn server_identity(&self) -> &ServerIdentity {
        self.core.configuration().identity()
    }

    fn link_target(&self) -> String {
        format!("{}:{}", self.session_id(), self.id())
    }

    /// Replace this handle's snapshot with the window's current state.
    ///
    /// The handle keeps following the same link, so a window that moved to a
    /// different index in the same session is found at its new index.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when this session no longer links the
    /// window, or a listing error when tmux could not be read.
    pub async fn refresh(&mut self) -> Result<&mut Self, Error> {
        let session = self.session_id().clone();
        self.refresh_in(&session).await
    }

    async fn refresh_in(&mut self, session: &SessionId) -> Result<&mut Self, Error> {
        let session = session.to_string();
        let projection = listing::windows(&self.core, listing::Scope::Target(&session), None)
            .await?
            .into_iter()
            .find(|projection| projection.window().window_id() == self.id())
            .ok_or_else(|| Error::ObjectGone {
                kind: ObjectKind::Window,
                id: self.id().to_string(),
            })?;

        self.projection = projection;
        Ok(self)
    }

    /// Return a new handle holding the window's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when this session no longer links the
    /// window, or a listing error when tmux could not be read.
    pub async fn refreshed(&self) -> Result<Self, Error> {
        let mut refreshed = self.clone();
        refreshed.refresh().await?;
        Ok(refreshed)
    }

    /// Split this window and return the new pane.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the split, which includes a target
    /// pane that is too small to divide.
    pub async fn split(&self, options: impl Into<SplitOptions>) -> Result<Pane, Error> {
        let options = options.into();
        let window = self.id().to_string();
        let projection =
            listing::create_pane(&self.core, |format| options.into_command(&window, format))
                .await?;

        Ok(Pane::new(Arc::clone(&self.core), projection))
    }

    /// Rename the window and update this handle.
    ///
    /// The name belongs to the window, so every session linking it sees the
    /// change.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the name.
    /// tmux expands the name as a format before it checks it, so `#(command)`
    /// in one runs a shell command. See [the crate documentation][crate#a-name-reaches-tmux-as-a-format]
    /// before passing text a caller supplied.
    pub async fn rename(&mut self, name: impl Into<OsString>) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "rename-window",
            Command::new("rename-window")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(name.into()),
        )
        .await?;

        self.refresh()
            .await
            .map_err(|error| error.after_effect("rename-window"))?;
        Ok(self)
    }

    /// Make this window active in the session it was reached through.
    ///
    /// Targeted by id rather than by index. An index is a slot, not an
    /// identity: anything that renumbers windows -- breaking a lone pane out
    /// of one moves it to a free index -- leaves a handle's cached index
    /// pointing at whatever occupies that slot now, which is a different live
    /// window rather than nothing. tmux answers such a target without
    /// complaint, so an index-scoped select would switch to the wrong window
    /// and report success.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command, and
    /// [`Error::ObjectGone`] when the window is no longer on the server.
    pub async fn select(&mut self) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "select-window",
            Command::new("select-window")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await?;

        self.refresh()
            .await
            .map_err(|error| error.after_effect("select-window"))?;
        Ok(self)
    }

    /// Set the window's pane layout.
    ///
    /// Takes a [`Layout`] tmux knows by name, or a layout string tmux itself
    /// produced through [`LayoutSpec::Saved`]. A `&str`, `String`, or
    /// `OsString` is read as a saved layout, so a caller who already had one
    /// keeps working.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the layout, and
    /// [`crate::ErrorKind::UnsupportedVersion`] when a named layout needs a
    /// newer tmux than this endpoint runs. See [`Layout::minimum_release`].
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::Layout;
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("arranged").await?;
    /// let mut window = session.active_window().await?.expect("a window");
    ///
    /// window.select_layout(Layout::Tiled).await?;
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn select_layout(
        &mut self,
        layout: impl Into<LayoutSpec>,
    ) -> Result<&mut Self, Error> {
        let layout = layout.into();
        let argument = match &layout {
            LayoutSpec::Named(named) => {
                crate::Server::from_core(Arc::clone(&self.core))
                    .require(named.as_str(), named.minimum_release())
                    .await?;
                OsString::from(named.as_str())
            }
            LayoutSpec::Saved(saved) => saved.clone(),
        };

        listing::mutate(
            &self.core,
            "select-layout",
            Command::new("select-layout")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(argument),
        )
        .await?;

        self.refresh()
            .await
            .map_err(|error| error.after_effect("select-layout"))?;
        Ok(self)
    }

    /// Restart the window's command in place.
    ///
    /// Passing `None` reruns whatever the window started with. Every pane in
    /// the window is replaced by the one the command runs in, which is what
    /// makes this different from respawning a pane: [`crate::Pane::respawn`]
    /// restarts one pane and leaves its neighbours alone.
    ///
    /// # Errors
    ///
    /// Returns an error when a pane is still running and `kill` is not set.
    pub async fn respawn(
        &mut self,
        command: Option<impl Into<OsString>>,
        kill: bool,
    ) -> Result<&mut Self, Error> {
        let mut respawn = Command::new("respawn-window")
            .arg("-t")
            .arg(self.id().to_string());
        if kill {
            respawn = respawn.arg("-k");
        }
        if let Some(command) = command {
            respawn = respawn.arg(command.into());
        }

        listing::mutate(&self.core, "respawn-window", respawn).await?;
        self.refresh()
            .await
            .map_err(|error| error.after_effect("respawn-window"))?;
        Ok(self)
    }

    /// Move to the next named layout, and return the window.
    ///
    /// tmux steps through its own list rather than taking a name, so this is a
    /// flag on `select-layout` and not something [`Window::select_layout`]
    /// could express.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn next_layout(&mut self) -> Result<&mut Self, Error> {
        self.step_layout("-n").await
    }

    /// Move to the previous named layout, and return the window.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn previous_layout(&mut self) -> Result<&mut Self, Error> {
        self.step_layout("-p").await
    }

    /// Step through tmux's layout list in one direction.
    async fn step_layout(&mut self, flag: &'static str) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "select-layout",
            Command::new("select-layout")
                .arg(flag)
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await?;

        self.refresh()
            .await
            .map_err(|error| error.after_effect("select-layout"))?;
        Ok(self)
    }

    /// Move the window's panes round one position, keeping the layout.
    ///
    /// The panes swap places within the arrangement rather than the
    /// arrangement changing, so a rotation of a three-pane window three times
    /// returns it to where it started.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the rotation.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::{Rotation, SplitDirection, SplitOptions};
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("turning").await?;
    /// let mut window = session.active_window().await?.expect("a window");
    /// window.split(SplitOptions::new(SplitDirection::Below)).await?;
    ///
    /// let before: Vec<_> = window.panes().await?.iter().map(|p| p.id().clone()).collect();
    /// window.rotate(Rotation::Up).await?;
    /// let after: Vec<_> = window.panes().await?.iter().map(|p| p.id().clone()).collect();
    ///
    /// assert_ne!(before, after, "the panes moved");
    /// window.rotate(Rotation::Down).await?;
    /// let back: Vec<_> = window.panes().await?.iter().map(|p| p.id().clone()).collect();
    /// assert_eq!(before, back, "and the other way undoes it");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn rotate(&self, rotation: Rotation) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "rotate-window",
            Command::new("rotate-window")
                .arg(rotation.flag())
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Kill the window, closing it in every session that links it.
    ///
    /// This consumes the handle. Use [`Window::unlink`] to remove only this
    /// session's link while leaving the window alive elsewhere.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn kill(self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "kill-window",
            Command::new("kill-window")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Remove this session's link to the window, leaving other links intact.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command, which includes
    /// unlinking a window that only one session holds.
    pub async fn unlink(self) -> Result<(), Error> {
        // `session:id` rather than `session:index`. A link is what this
        // removes, so the session half is needed; the window half is an
        // identity, and a cached index stops being one the moment anything
        // renumbers the session.
        let Err(error) = listing::mutate(
            &self.core,
            "unlink-window",
            Command::new("unlink-window").arg("-t").arg(format!(
                "{}:{}",
                self.session_id(),
                self.id()
            )),
        )
        .await
        else {
            return Ok(());
        };

        Err(link_or_object_gone(&self.core, error, &[(self.session_id(), self.id())]).await)
    }

    /// Run a raw tmux command against this window.
    ///
    /// The escape hatch for anything this crate does not wrap. `-t` is placed
    /// after the subcommand for you, because tmux stops reading flags at the
    /// first positional: a target appended after one is taken as text, and the
    /// command then succeeds having acted on something else.
    ///
    /// A non-zero exit status comes back in the [`CommandResult`] rather than
    /// as an error, the same as [`crate::Server::cmd`].
    ///
    /// # Errors
    ///
    /// Returns an error when the process cannot be started, captured, or
    /// awaited.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("raw").await?;
    /// let window = session.active_window().await?.expect("a window");
    ///
    /// let result = window
    ///     .cmd(libtmux::Command::new("display-message").arg("-p").arg("#{window_id}"))
    ///     .await?;
    /// assert_eq!(result.stdout_lossy().trim(), window.id().to_string());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn cmd(&self, command: Command) -> Result<CommandResult, Error> {
        self.core
            .execute(command.targeting(self.id().to_string()))
            .await
    }

    /// Expand a tmux format string in this window's context.
    ///
    /// This returns `display-message` output instead of showing it in front of
    /// a person with [`Self::display`]. See [`crate::Server::format`] for the
    /// shell boundary around command and recursive formats.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the format.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("shown").await?;
    /// let window = session.active_window().await?.expect("a window");
    ///
    /// let expanded = window.format("#{window_id}").await?;
    /// assert_eq!(expanded.to_string_lossy(), window.id().to_string());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn format(&self, template: &str) -> Result<TmuxText, Error> {
        let result = self
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg(OsString::from(template)),
            )
            .await?;
        if !result.success() {
            return Err(Error::from_refused_result(
                "display-message",
                &result,
                Some(OsStr::new(&self.id().to_string())),
            ));
        }

        // `-p` ends its output with a newline that frames it rather than
        // belonging to the expansion.
        let stdout = result.stdout();
        Ok(TmuxText::from(
            stdout.strip_suffix(b"\n").unwrap_or(stdout).to_vec(),
        ))
    }

    /// Show a message on the clients viewing this window.
    ///
    /// The showing half of tmux's `display-message`. Nothing is returned
    /// because nothing is read: use [`Self::format`] to get text back.
    /// Succeeds when nobody is watching, having shown it to nobody.
    /// Like [`Self::format`], this interprets the message as a tmux format;
    /// command and recursive formats can run shell commands.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the message.
    pub async fn display(&self, message: &str) -> Result<(), Error> {
        let result = self
            .cmd(Command::new("display-message").arg(OsString::from(message)))
            .await?;
        if result.success() {
            return Ok(());
        }

        Err(Error::from_refused_result(
            "display-message",
            &result,
            Some(OsStr::new(&self.id().to_string())),
        ))
    }

    /// Create a pane, run an operation with it, then kill it.
    ///
    /// Once this future is polled, the scope owns creation and cleanup.
    /// Cancellation or unwinding can let an in-flight creation finish, but a
    /// pane whose creation yields a handle is killed while the Tokio runtime
    /// remains active. Ordinary handle `Drop` remains non-destructive.
    ///
    /// Setup and teardown failures convert into the operation's own error
    /// type, so a caller writes one `?` rather than unwrapping twice. When
    /// both the operation and cleanup fail, the cleanup error is returned as
    /// [`Error::AfterEffect`], because tmux had already accepted the scope's
    /// creation; the operation error is discarded. When the operation fails
    /// and cleanup succeeds, its generic error is returned unchanged: the
    /// scope cannot certify replay safety for arbitrary callback work.
    /// A canceled caller cannot receive a cleanup error, so tracing is its
    /// only report.
    ///
    /// # Errors
    ///
    /// Returns the operation's error, or a converted [`Error`] when the
    /// pane could not be created or could not be killed after creation.
    pub async fn with_pane<T, E>(
        &self,
        options: impl Into<SplitOptions>,
        operation: impl AsyncFnOnce(&Pane) -> Result<T, E>,
    ) -> Result<T, E>
    where
        E: From<Error>,
    {
        let window = self.clone();
        let options = options.into();
        scoped::run(
            "with-pane",
            async move { window.split(options).await },
            Pane::kill,
            operation,
        )
        .await
    }

    /// Swap this window's position with another.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when `other` belongs to another
    /// server, or an error when tmux refuses the swap.
    pub async fn swap_with(&mut self, other: &Self) -> Result<&mut Self, Error> {
        self.core
            .require_same_server(other.server_identity(), "swap-window")?;
        if let Err(error) = listing::mutate(
            &self.core,
            "swap-window",
            Command::new("swap-window")
                .arg("-s")
                .arg(self.link_target())
                .arg("-t")
                // `session:id`, not `session:index`: the session half says
                // which of the target's links to swap, and the id half cannot
                // go stale under a renumber the way a cached index does.
                .arg(format!("{}:{}", other.session_id(), other.id())),
        )
        .await
        {
            // Either half can be the one tmux could not resolve, and a window
            // still linked elsewhere is not a window that is gone.
            return Err(link_or_object_gone(
                &self.core,
                error,
                &[
                    (self.session_id(), self.id()),
                    (other.session_id(), other.id()),
                ],
            )
            .await);
        }

        self.refresh_in(other.session_id())
            .await
            .map_err(|error| error.after_effect("swap-window"))?;
        Ok(self)
    }

    /// Move this window to an index, possibly in another session.
    ///
    /// The destination is a session and an index rather than a target string,
    /// because those are the two things tmux needs and a string could express
    /// neither or both.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when `session` belongs to another
    /// server, or an error when the destination is occupied or absent.
    pub async fn move_to(&mut self, session: &Session, index: i32) -> Result<&mut Self, Error> {
        self.core
            .require_same_server(session.server_identity(), "move-window")?;
        listing::mutate(
            &self.core,
            "move-window",
            Command::new("move-window")
                .arg("-s")
                .arg(self.link_target())
                .arg("-t")
                .arg(format!("{}:{index}", session.id())),
        )
        .await?;

        self.refresh_in(session.id())
            .await
            .map_err(|error| error.after_effect("move-window"))?;
        Ok(self)
    }

    /// Resize the window to an exact size in cells.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the size.
    /// Move one edge of the window by a number of cells.
    ///
    /// This is the form a keybinding uses: "make it two columns wider" rather
    /// than a size computed from the current one.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the resize.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::ResizeDirection;
    ///
    /// let session = server.new_session("resized-window").await?;
    /// let mut window = session.windows().await?.remove(0);
    ///
    /// window.resize(80, 24).await?;
    /// window.resize_by(ResizeDirection::Down, 4).await?;
    /// assert_eq!(window.height(), 28);
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn resize_by(
        &mut self,
        direction: ResizeDirection,
        cells: u32,
    ) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "resize-window",
            Command::new("resize-window")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(direction.flag())
                .arg(cells.to_string()),
        )
        .await?;

        self.refresh()
            .await
            .map_err(|error| error.after_effect("resize-window"))?;
        Ok(self)
    }

    /// Link this window into another session, so both hold the same window.
    ///
    /// A linked window is one window with two winlinks, not a copy: renaming
    /// it or splitting it shows up in both sessions. [`Window::unlink`] takes
    /// one link away again.
    ///
    /// `index` places the link at a window index, or appends when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when `session` belongs to another
    /// server, or an error when tmux refuses the link, including an occupied
    /// index.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// let source = server.new_session("linking-from").await?;
    /// let target = server.new_session("linking-to").await?;
    /// let window = source.windows().await?.remove(0);
    ///
    /// window.link_to(&target, None).await?;
    ///
    /// let linked = target.windows().await?;
    /// assert!(linked.iter().any(|other| other.id() == window.id()));
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn link_to(&self, session: &Session, index: Option<i32>) -> Result<(), Error> {
        self.core
            .require_same_server(session.server_identity(), "link-window")?;
        let target = index.map_or_else(
            || session.id().to_string(),
            |index| format!("{}:{index}", session.id()),
        );

        listing::mutate(
            &self.core,
            "link-window",
            Command::new("link-window")
                .arg("-s")
                .arg(self.link_target())
                .arg("-t")
                .arg(target),
        )
        .await?;

        Ok(())
    }

    /// Resize the window to an exact size in cells.
    ///
    /// Use [`Window::resize_by`] to move one edge instead.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the size.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// let session = server.new_session("sized").await?;
    /// let mut window = session.windows().await?.remove(0);
    ///
    /// window.resize(100, 40).await?;
    /// assert_eq!((window.width(), window.height()), (100, 40));
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn resize(&mut self, width: u32, height: u32) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "resize-window",
            Command::new("resize-window")
                .arg("-t")
                .arg(self.id().to_string())
                .arg("-x")
                .arg(width.to_string())
                .arg("-y")
                .arg(height.to_string()),
        )
        .await?;

        self.refresh()
            .await
            .map_err(|error| error.after_effect("resize-window"))?;
        Ok(self)
    }
}

/// Windows compare by server endpoint, session, index, and window id.
///
/// Equality follows the link, not the window, because a linked window occupies
/// a genuinely different position in each session that holds it. Compare
/// [`Window::id`] directly to ask whether two handles name the same underlying
/// window.
impl PartialEq for Window {
    fn eq(&self, other: &Self) -> bool {
        self.server_identity() == other.server_identity()
            && self.projection.link().identity() == other.projection.link().identity()
    }
}

impl Eq for Window {}

impl Hash for Window {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.server_identity().hash(state);
        self.projection.link().identity().hash(state);
    }
}

/// Renders identity only, never snapshot text.
impl fmt::Debug for Window {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Window")
            .field("id", &self.id())
            .field("session_id", &self.session_id())
            .field("index", &self.index())
            .finish_non_exhaustive()
    }
}

/// Filtering a window uses the same handles as the snapshot beneath it.
///
/// Matching and validation delegate to that snapshot, so an expression can
/// only name fields the catalog knows. The companion is re-parameterized to
/// [`Window`] so the type a listing returns is the type an expression
/// filters.
#[cfg(feature = "query")]
impl Filterable for Window {
    type Fields = WindowFields<Self>;

    const FILTER_TARGET: &'static str = <WindowInfo as Filterable>::FILTER_TARGET;

    fn filter_fields() -> Self::Fields {
        Self::Fields::for_target(Self::FILTER_TARGET)
    }

    fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
        self.projection.window().__filter_matches(predicate)
    }

    fn __filter_validate(
        predicate: &crate::query::__private::Predicate,
    ) -> Result<(), crate::query::FilterExpressionError> {
        <WindowInfo as Filterable>::__filter_validate(predicate)
    }
}

#[cfg(feature = "query")]
impl FilterSchema for Window {
    fn __filter_schema() -> crate::query::__private::FilterSchemaDescriptor {
        <WindowInfo as FilterSchema>::__filter_schema()
    }
}

/// Where a split puts the new pane, relative to the one being divided.
///
/// tmux spells these `-v`, `-v -b`, `-h`, and `-h -b`, where "horizontal"
/// means side by side. Naming the resulting position instead removes a
/// question every tmux user has asked at least once.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{Rotation, SplitDirection};
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let window = session.active_window().await?.expect("a session has a window");
/// window.split(SplitDirection::Below).await?;
///
/// // Rotating moves panes between positions; it does not create or destroy any.
/// let before = window.panes().await?.len();
/// window.rotate(Rotation::Down).await?;
/// assert_eq!(window.panes().await?.len(), before);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Rotation {
    /// Move each pane to the position before it, so the first becomes last.
    Up,
    /// Move each pane to the position after it, which is tmux's default.
    Down,
}

impl Rotation {
    /// The tmux flag that asks for this direction.
    const fn flag(self) -> &'static str {
        match self {
            Self::Up => "-U",
            Self::Down => "-D",
        }
    }
}

/// One of the pane arrangements tmux knows by name.
///
/// tmux has exactly these, so a layout that does not exist is a compile error
/// rather than a refusal at the far end of a round trip. A saved layout
/// string is a different thing and goes through [`LayoutSpec::Saved`].
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::Layout;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("arranged").await?;
/// let mut window = session.active_window().await?.expect("a window");
///
/// window.select_layout(Layout::EvenHorizontal).await?;
/// assert_eq!(Layout::EvenHorizontal.as_str(), "even-horizontal");
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Layout {
    /// Panes side by side, each the full height.
    EvenHorizontal,
    /// Panes stacked, each the full width.
    EvenVertical,
    /// One large pane above a row of the rest.
    MainHorizontal,
    /// The arrangement of [`Layout::MainHorizontal`] with the large pane
    /// below the row rather than above it.
    MainHorizontalMirrored,
    /// One large pane beside a column of the rest.
    MainVertical,
    /// The arrangement of [`Layout::MainVertical`] with the large pane on
    /// the right rather than the left.
    MainVerticalMirrored,
    /// Panes in as even a grid as their count allows.
    Tiled,
}

impl Layout {
    /// The name tmux knows this layout by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EvenHorizontal => "even-horizontal",
            Self::EvenVertical => "even-vertical",
            Self::MainHorizontal => "main-horizontal",
            Self::MainHorizontalMirrored => "main-horizontal-mirrored",
            Self::MainVertical => "main-vertical",
            Self::MainVerticalMirrored => "main-vertical-mirrored",
            Self::Tiled => "tiled",
        }
    }

    /// The first tmux release that arranges panes this way.
    ///
    /// The mirrored pair arrived in 3.5; the rest predate everything this
    /// crate supports.
    #[must_use]
    pub const fn minimum_release(self) -> crate::ReleaseVersion {
        match self {
            Self::MainHorizontalMirrored | Self::MainVerticalMirrored => {
                crate::version::since::MIRRORED_LAYOUTS
            }
            _ => crate::TmuxVersion::MIN_SUPPORTED,
        }
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What to arrange a window's panes as.
///
/// A [`Layout`] names an arrangement tmux computes. A saved string is one
/// tmux already computed: [`Window::layout`] reports one, and handing it back
/// restores that exact arrangement including the pane sizes, which a named
/// layout cannot express.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::Layout;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("restored").await?;
/// let mut window = session.active_window().await?.expect("a window");
///
/// // Keep what tmux reports, rearrange, then put it back exactly.
/// let before = window.layout().to_owned();
/// window.select_layout(Layout::EvenVertical).await?;
/// window.select_layout(&before).await?;
/// assert_eq!(window.layout().as_bytes(), before.as_bytes());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutSpec {
    /// An arrangement tmux knows by name.
    Named(Layout),
    /// A layout string tmux produced, restoring pane sizes exactly.
    Saved(OsString),
}

impl From<Layout> for LayoutSpec {
    fn from(layout: Layout) -> Self {
        Self::Named(layout)
    }
}

impl From<OsString> for LayoutSpec {
    fn from(saved: OsString) -> Self {
        Self::Saved(saved)
    }
}

impl From<String> for LayoutSpec {
    fn from(saved: String) -> Self {
        Self::Saved(saved.into())
    }
}

impl From<&str> for LayoutSpec {
    fn from(saved: &str) -> Self {
        Self::Saved(saved.into())
    }
}

impl From<&OsStr> for LayoutSpec {
    fn from(saved: &OsStr) -> Self {
        Self::Saved(saved.to_owned())
    }
}

impl From<&TmuxText> for LayoutSpec {
    /// Take a layout straight from [`Window::layout`].
    ///
    /// tmux writes a layout as printable ASCII, and this keeps the bytes
    /// rather than the lossy text either way, so a layout that round-trips
    /// through a handle is the one tmux produced.
    fn from(saved: &TmuxText) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;

            Self::Saved(OsString::from_vec(saved.as_bytes().to_vec()))
        }
        #[cfg(not(unix))]
        {
            Self::Saved(OsString::from(saved.to_string_lossy().into_owned()))
        }
    }
}

/// Where a split puts the pane it makes.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::SplitDirection;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let window = session.active_window().await?.expect("a session has a window");
///
/// // The direction names where the *new* pane lands, not which edge moves.
/// window.split(SplitDirection::Right).await?;
/// window.split(SplitDirection::Below).await?;
/// assert_eq!(window.panes().await?.len(), 3);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    /// Above the pane being divided.
    Above,
    /// Below it, which is tmux's default.
    Below,
    /// To its left.
    Left,
    /// To its right.
    Right,
}

impl SplitDirection {
    /// Return the tmux flags that produce this position.
    pub(crate) const fn flags(self) -> (&'static str, bool) {
        match self {
            Self::Above => ("-v", true),
            Self::Below => ("-v", false),
            Self::Left => ("-h", true),
            Self::Right => ("-h", false),
        }
    }
}

/// Where a pane lands when it is moved into another window.
///
/// [`Pane::break_out`] takes a pane out into a window of its own and this puts
/// one back, so between them a pane can be moved anywhere. tmux spawns nothing
/// here, which is why this carries none of the command, directory, or
/// environment a [`SplitOptions`] does: the pane already exists and keeps
/// everything running in it.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{JoinOptions, SplitDirection};
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("moving").await?;
/// let window = session.active_window().await?.expect("a window");
/// let second = session.new_window("elsewhere").await?;
/// let stranded = second.panes().await?.remove(0);
///
/// // Put a pane from the other window beside this one.
/// let here = window.panes().await?.remove(0);
/// let moved = stranded
///     .join_into(&here, JoinOptions::new(SplitDirection::Below))
///     .await?;
/// assert_eq!(moved.window_id(), window.id());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JoinOptions {
    direction: SplitDirection,
    size: Option<PaneSize>,
    full: bool,
}

impl JoinOptions {
    /// Land the pane in one direction from the pane it joins.
    #[must_use]
    pub const fn new(direction: SplitDirection) -> Self {
        Self {
            direction,
            size: None,
            full: false,
        }
    }

    /// Ask for a size rather than letting tmux halve the space.
    #[must_use]
    pub const fn size(mut self, size: PaneSize) -> Self {
        self.size = Some(size);
        self
    }

    /// Span the window rather than only the pane being joined.
    #[must_use]
    pub const fn full(mut self) -> Self {
        self.full = true;
        self
    }

    /// Add this placement to a `join-pane` command.
    pub(crate) fn apply(self, command: Command) -> Command {
        let (axis, before) = self.direction.flags();
        let mut command = command.arg(axis);
        if before {
            command = command.arg("-b");
        }
        if self.full {
            command = command.arg("-f");
        }
        if let Some(size) = self.size {
            command = command.arg("-l").arg(size.to_string());
        }
        command
    }
}

/// How much space a new pane gets.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{PaneSize, SplitDirection, SplitOptions};
///
/// // A size renders as tmux's own `-l` argument, cells bare and shares suffixed.
/// assert_eq!(PaneSize::Cells(20).to_string(), "20");
/// assert_eq!(PaneSize::Percent(25).to_string(), "25%");
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let window = session.active_window().await?.expect("a session has a window");
///
/// let pane = window
///     .split(SplitOptions::new(SplitDirection::Below).size(PaneSize::Percent(25)))
///     .await?;
/// assert!(pane.height() > 0);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PaneSize {
    /// A number of rows or columns, depending on the split direction.
    Cells(u32),
    /// A share of the space being divided, as a percentage.
    Percent(u32),
}

impl fmt::Display for PaneSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cells(cells) => write!(formatter, "{cells}"),
            Self::Percent(percent) => write!(formatter, "{percent}%"),
        }
    }
}

/// Which edge a resize moves.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::ResizeDirection;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let mut window = session.active_window().await?.expect("a session has a window");
///
/// // The direction names the edge that moves, so `Down` makes a window taller
/// // rather than shorter.
/// let before = window.height();
/// window.resize_by(ResizeDirection::Down, 3).await?;
/// assert!(window.height() >= before);
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResizeDirection {
    /// Move the top edge up, making it taller.
    Up,
    /// Move the bottom edge down.
    Down,
    /// Move the left edge left.
    Left,
    /// Move the right edge right.
    Right,
}

impl ResizeDirection {
    /// Return the tmux flag for this direction.
    pub(crate) const fn flag(self) -> &'static str {
        match self {
            Self::Up => "-U",
            Self::Down => "-D",
            Self::Left => "-L",
            Self::Right => "-R",
        }
    }
}

/// Options for splitting a window or pane into a new pane.
///
/// A bare [`SplitDirection`] is accepted wherever this is:
/// `window.split(SplitDirection::Below)`.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{SplitDirection, SplitOptions};
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("split").await?;
/// let window = session.active_window().await?.expect("a session has a window");
///
/// // Splitting leaves focus where it was unless asked otherwise, so a caller
/// // that wants the new pane selected says so.
/// let pane = window
///     .split(SplitOptions::new(SplitDirection::Right).select())
///     .await?;
/// assert_eq!(window.active_pane().await?.map(|active| active.id().to_string()),
///            Some(pane.id().to_string()));
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[must_use = "options describe a split but do not perform one"]
#[derive(Clone)]
pub struct SplitOptions {
    direction: SplitDirection,
    start_directory: Option<std::path::PathBuf>,
    command: Option<OsString>,
    size: Option<PaneSize>,
    environment: Vec<(OsString, OsString)>,
    full: bool,
    zoom: bool,
    select: bool,
}

impl fmt::Debug for SplitOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SplitOptions")
            .field("direction", &self.direction)
            .field("has_start_directory", &self.start_directory.is_some())
            .field("has_command", &self.command.is_some())
            .field("size", &self.size)
            .field("environment_count", &self.environment.len())
            .field("full", &self.full)
            .field("zoom", &self.zoom)
            .field("select", &self.select)
            .finish()
    }
}

impl SplitOptions {
    /// Describe a split placing the new pane in one direction.
    pub const fn new(direction: SplitDirection) -> Self {
        Self {
            direction,
            start_directory: None,
            command: None,
            size: None,
            environment: Vec::new(),
            full: false,
            zoom: false,
            select: false,
        }
    }

    /// Set the new pane's working directory.
    ///
    /// tmux expands this as a format, so [`crate::escape_format`] belongs
    /// around text a program did not write.
    pub fn start_directory(mut self, directory: impl Into<std::path::PathBuf>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Run a command instead of the default shell.
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Give the new pane a size, in cells or as a share of the space.
    pub const fn size(mut self, size: PaneSize) -> Self {
        self.size = Some(size);
        self
    }

    /// Set an environment variable for the process the new pane starts.
    ///
    /// Call this more than once for more than one variable. tmux applies
    /// these to the new process only, not to the session.
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Span the full width or height of the window rather than of the pane.
    pub const fn full(mut self) -> Self {
        self.full = true;
        self
    }

    /// Zoom the new pane, filling the window until it is unzoomed.
    pub const fn zoom(mut self) -> Self {
        self.zoom = true;
        self
    }

    /// Make the new pane active.
    pub const fn select(mut self) -> Self {
        self.select = true;
        self
    }

    /// Lower these options into a `split-window` command for one target.
    ///
    /// `print_format` is placed with the other flags because tmux stops
    /// parsing flags at the first positional, and the shell command is one.
    pub(crate) fn into_command(self, target: &str, print_format: &str) -> Command {
        let (axis, before) = self.direction.flags();
        let mut command = Command::new("split-window")
            .arg("-P")
            .arg("-F")
            .arg(print_format)
            .arg("-t")
            .arg(target)
            .arg(axis);
        if before {
            command = command.arg("-b");
        }
        if !self.select {
            command = command.arg("-d");
        }
        if self.full {
            command = command.arg("-f");
        }
        if self.zoom {
            command = command.arg("-Z");
        }
        if let Some(size) = self.size {
            command = command.arg("-l").arg(size.to_string());
        }
        if let Some(directory) = self.start_directory {
            command = command.arg("-c").arg(directory.into_os_string());
        }
        for (name, value) in self.environment {
            command = command.arg("-e").sensitive_arg(assignment(&name, &value));
        }
        if let Some(shell_command) = self.command {
            command = command.sensitive_arg(shell_command);
        }
        command
    }
}

impl From<SplitDirection> for SplitOptions {
    fn from(direction: SplitDirection) -> Self {
        Self::new(direction)
    }
}

/// Render one `NAME=VALUE` pair for tmux's `-e` flag.
///
/// Built from bytes because tmux accepts environment values that are not
/// valid UTF-8, and rejecting them here would be stricter than tmux is.
pub(crate) fn assignment(name: &OsStr, value: &OsStr) -> OsString {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let mut bytes = name.as_bytes().to_vec();
    bytes.push(b'=');
    bytes.extend_from_slice(value.as_bytes());

    OsString::from_vec(bytes)
}

/// Renders `session:id`, the form tmux targets one of a window's links by.
///
/// The id alone would be ambiguous about which link is meant, and this is the
/// spelling that can be pasted into a tmux command. It names the window by
/// identity rather than by index: an index is a place within a session, and
/// the window sitting at one changes whenever anything renumbers or unlinks,
/// so a rendered coordinate goes stale in a handle that has not refreshed and
/// reaches a different window rather than nothing.
impl fmt::Display for Window {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.session_id(), self.id())
    }
}

/// Tell a missing link from a missing window, on the failure path only.
///
/// A link-scoped command targets `session:@id`: a window linked into three
/// sessions has one id and three links, so the session half says which link to
/// act on, while the window half stays an identity because a cached index
/// stops being one the moment anything renumbers the session.
///
/// tmux echoes such a target back unresolved whether the window died or is
/// merely linked somewhere else -- the two are the same string -- so the
/// classifier cannot separate them and reports the stronger fact. This buys
/// the weaker one for a lookup, and only when the answer changes what a caller
/// should do with the handle.
///
/// Asked of the server rather than of a handle. A window refreshes within its
/// own session, and a window whose link to that session is gone is exactly the
/// case being told apart -- so refreshing would report it missing and answer
/// the question with its own premise.
/// `links` are the `session:id` targets the command sent, so the one tmux
/// named is the one to ask about: a command carrying both a source and a
/// destination fails on whichever it could not resolve, and probing the other
/// would answer about a window that was never in question.
async fn link_or_object_gone(
    core: &Arc<Core>,
    error: Error,
    links: &[(&SessionId, &WindowId)],
) -> Error {
    let Error::ObjectGone {
        kind: ObjectKind::Window,
        ref id,
    } = error
    else {
        return error;
    };

    let Some(&(session, window)) = links.iter().find(|(_, window)| window.to_string() == *id)
    else {
        return error;
    };

    let alive = crate::Server::from_core(Arc::clone(core))
        .window_by_id(window)
        .await
        .is_ok_and(|found| found.is_some());

    if alive {
        return Error::LinkGone {
            kind: ObjectKind::Window,
            target: format!("{session}:{window}"),
        };
    }
    error
}

#[cfg(test)]
mod split_option_tests {
    use super::{SplitDirection, SplitOptions};

    #[test]
    fn split_options_redact_process_inputs() {
        let secret = "sentinel-split-process";
        let options = SplitOptions::new(SplitDirection::Below)
            .environment("TOKEN", secret)
            .command(secret);
        assert!(!format!("{options:?}").contains(secret));

        let summary = options.into_command("@1", "#{pane_id}").summary();
        assert_eq!(summary.sensitive_argument_count(), 2);
        assert!(!summary.to_string().contains(secret));
    }
}
