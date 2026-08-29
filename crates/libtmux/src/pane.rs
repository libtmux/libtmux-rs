//! Pane handles and their snapshot getters.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::formats::TmuxText;
use crate::internal::core::Core;
use crate::internal::listing;
use crate::internal::options;
#[cfg(feature = "query")]
use crate::query::Filterable;
use crate::snapshot::PaneProjection;
#[cfg(feature = "query")]
use crate::snapshot::{PaneFields, PaneInfo};
use crate::target::{PaneId, ServerIdentity, SessionId, WindowId};
use crate::window::Window;
use crate::{Command, CommandResult, Error, IndexedHooks, ObjectKind, OptionValue};

/// One tmux pane, as reached through one window link.
///
/// A pane belongs to exactly one window, but that window can be linked into
/// several sessions. The handle retains the link it was discovered through so
/// traversal back up the hierarchy lands where the caller came from.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("work").await?;
/// let window = session.active_window().await?.expect("a session has a window");
/// let pane = window.active_pane().await?.expect("a window has a pane");
///
/// pane.send_keys("echo hello").await?;
/// pane.send_key_names(["Enter"]).await?;
///
/// // Capture returns `TmuxText`, because a pane can print any bytes.
/// let lines = pane.capture().await?;
/// assert!(!lines.is_empty());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Pane {
    core: Arc<Core>,
    projection: PaneProjection,
}

impl Pane {
    /// Build a handle from a hydrated projection.
    pub(crate) const fn new(core: Arc<Core>, projection: PaneProjection) -> Self {
        Self { core, projection }
    }

    /// Find the pane this process is running in.
    ///
    /// tmux sets `TMUX_PANE` in every process it starts, so a program running
    /// inside a pane can locate itself without being told which one it is.
    /// Pair it with [`crate::Server::from_env`], which reads the server from
    /// the same place.
    ///
    /// `Ok(None)` means tmux no longer has that pane, which is possible when
    /// the value came from an environment that outlived it.
    ///
    /// # Errors
    ///
    /// Returns an error when `TMUX_PANE` is absent or is not a pane ID, or
    /// when the pane listing fails.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::Pane;
    ///
    /// let session = server.new_session("locating").await?;
    /// let expected = session.panes().await?.remove(0);
    ///
    /// // Standing in for the environment tmux gives a process it starts.
    /// let found = Pane::from_env_value(server, Some(expected.id().as_ref())).await?;
    ///
    /// assert_eq!(found.expect("the pane exists").id(), expected.id());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_env(server: &crate::Server) -> Result<Option<Self>, Error> {
        Self::from_env_value(server, std::env::var_os("TMUX_PANE")).await
    }

    /// Find a pane from an explicit `TMUX_PANE` value.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is absent or is not a pane ID, or when
    /// the pane listing fails.
    pub async fn from_env_value(
        server: &crate::Server,
        value: Option<impl AsRef<OsStr>>,
    ) -> Result<Option<Self>, Error> {
        server
            .pane_by_id(&parse_env_id(value.as_ref().map(AsRef::as_ref))?)
            .await
    }

    /// Return the tmux pane identity.
    #[must_use]
    pub const fn id(&self) -> &PaneId {
        self.projection.pane().pane_id()
    }

    /// Return the window that contains this pane.
    #[must_use]
    pub const fn window_id(&self) -> &WindowId {
        self.projection.link_identity().window_id()
    }

    /// Return the session this handle reached the pane through.
    ///
    /// A pane reached through a linked window can report a different session
    /// depending on which link discovery followed.
    #[must_use]
    pub const fn session_id(&self) -> &SessionId {
        self.projection.link_identity().session_id()
    }

    /// Return the pane's index within its window.
    #[must_use]
    pub fn index(&self) -> u32 {
        *self.projection.pane().pane_index()
    }

    /// Return the command currently running in the pane.
    #[must_use]
    pub fn current_command(&self) -> Option<&TmuxText> {
        self.projection.pane().pane_current_command().available()
    }

    /// Return the pane's working directory.
    #[must_use]
    pub fn current_path(&self) -> Option<&TmuxText> {
        self.projection.pane().pane_current_path().available()
    }

    /// Return the pane title.
    #[must_use]
    pub fn title(&self) -> &TmuxText {
        self.projection.pane().pane_title()
    }

    /// Return the pane's controlling terminal.
    #[must_use]
    pub fn tty(&self) -> &TmuxText {
        self.projection.pane().pane_tty()
    }

    /// Return the process id of the pane's foreground process.
    #[must_use]
    pub fn pid(&self) -> u32 {
        *self.projection.pane().pane_pid()
    }

    /// Return the pane width in cells.
    #[must_use]
    pub fn width(&self) -> u32 {
        *self.projection.pane().pane_width()
    }

    /// Return the pane height in cells.
    #[must_use]
    pub fn height(&self) -> u32 {
        *self.projection.pane().pane_height()
    }

    /// Report whether this pane is the active one in its window.
    #[must_use]
    pub fn is_active(&self) -> bool {
        *self.projection.pane().pane_active()
    }

    /// Report whether the pane touches the top edge of its window.
    ///
    /// The four edge predicates are what tell a caller a directional move has
    /// nowhere to go: a pane at the top has no neighbour above it, so
    /// selecting one would wrap or fail rather than move.
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
    /// let session = guard.server().new_session("edges").await?;
    /// let mut window = session.active_window().await?.expect("a window");
    ///
    /// // One pane fills the window, so it touches every edge.
    /// let only = window.active_pane().await?.expect("a pane");
    /// assert!(only.is_at_top() && only.is_at_bottom());
    ///
    /// // Splitting below leaves the original touching the top and not the
    /// // bottom, and the new one the other way round.
    /// let lower = window.split(SplitOptions::new(SplitDirection::Below)).await?;
    /// let upper = window
    ///     .panes()
    ///     .await?
    ///     .into_iter()
    ///     .find(|pane| pane.id() != lower.id())
    ///     .expect("the original pane");
    ///
    /// assert!(upper.is_at_top() && !upper.is_at_bottom());
    /// assert!(lower.is_at_bottom() && !lower.is_at_top());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn is_at_top(&self) -> bool {
        *self.projection.pane().pane_at_top()
    }

    /// Report whether the pane touches the bottom edge of its window.
    ///
    /// See [`Self::is_at_top`] for what the edge predicates are for.
    #[must_use]
    pub fn is_at_bottom(&self) -> bool {
        *self.projection.pane().pane_at_bottom()
    }

    /// Report whether the pane touches the left edge of its window.
    ///
    /// See [`Self::is_at_top`] for what the edge predicates are for.
    #[must_use]
    pub fn is_at_left(&self) -> bool {
        *self.projection.pane().pane_at_left()
    }

    /// Report whether the pane touches the right edge of its window.
    ///
    /// See [`Self::is_at_top`] for what the edge predicates are for.
    #[must_use]
    pub fn is_at_right(&self) -> bool {
        *self.projection.pane().pane_at_right()
    }

    /// Report whether the pane's process has exited while the pane remains.
    ///
    /// This is only observable when `remain-on-exit` keeps the pane open.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        *self.projection.pane().pane_dead()
    }

    /// Report whether the pane is in copy mode or another pane mode.
    ///
    /// tmux reports this as a count rather than a flag, so any nonzero value
    /// means the pane has a mode open.
    #[must_use]
    pub fn is_in_mode(&self) -> bool {
        *self.projection.pane().pane_in_mode() > 0
    }

    /// Return the identity of the server this pane belongs to.
    pub(crate) fn server_identity(&self) -> &ServerIdentity {
        self.core.configuration().identity()
    }

    /// Replace this handle's snapshot with the pane's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the pane no longer exists, or a
    /// listing error when tmux could not be read.
    pub async fn refresh(&mut self) -> Result<&mut Self, Error> {
        let target = self.id().to_string();
        let projection = listing::panes(&self.core, listing::Scope::Target(&target), None)
            .await?
            .into_iter()
            .find(|projection| projection.pane().pane_id() == self.id())
            .ok_or_else(|| Error::ObjectGone {
                kind: ObjectKind::Pane,
                id: self.id().to_string(),
            })?;

        self.projection = projection;
        Ok(self)
    }

    /// Return a new handle holding the pane's current state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ObjectGone`] when the pane no longer exists, or a
    /// listing error when tmux could not be read.
    pub async fn refreshed(&self) -> Result<Self, Error> {
        let mut refreshed = self.clone();
        refreshed.refresh().await?;
        Ok(refreshed)
    }

    /// Return the window that contains this pane.
    ///
    /// This re-reads tmux, so a window renamed or moved since discovery is
    /// reported as it is now. `Ok(None)` means the window no longer exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the window listing fails.
    pub async fn window(&self) -> Result<Option<Window>, Error> {
        let session = self.session_id().to_string();

        Ok(
            listing::windows(&self.core, listing::Scope::Target(&session), None)
                .await?
                .into_iter()
                .find(|projection| projection.window().window_id() == self.window_id())
                .map(|projection| Window::new(Arc::clone(&self.core), projection)),
        )
    }

    /// Watch what this pane writes, as it writes it.
    ///
    /// [`Pane::capture`] reads what is on screen now; this reports every byte
    /// the pane produces from here on, including what scrolls away. It opens a
    /// control-mode connection to the pane's session and keeps it, so there is
    /// no polling and no sampling interval to get wrong.
    ///
    /// The bytes are the pane's own, terminal escapes included. tmux reports
    /// them in whatever sized chunks it has, so a caller wanting lines has to
    /// buffer.
    ///
    /// tmux discards what a pane has buffered when the pane exits, so a
    /// command that writes and returns immediately may be reported as nothing
    /// at all.
    ///
    /// # Errors
    ///
    /// Returns an error when the control-mode connection cannot be opened.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn watch(pane: &libtmux::Pane) -> Result<(), libtmux::Error> {
    /// let mut output = pane.stream_output().await?;
    ///
    /// while let Some(chunk) = output.next_chunk().await {
    ///     println!("{} bytes", chunk.len());
    /// }
    ///
    /// output.shutdown().await
    /// # }
    /// ```
    #[cfg(feature = "control-mode")]
    pub async fn stream_output(&self) -> Result<crate::control::PaneOutput, Error> {
        let server = crate::Server::from_core(Arc::clone(&self.core));
        let (sender, events) = crate::control::ControlMode::attach(&server, self.session_id())
            .await?
            .split();

        // tmux sends a control client every pane on the server, so narrowing
        // happens before the caller reads rather than after the bytes arrive.
        sender.watch_only(std::slice::from_ref(self.id())).await?;

        Ok(crate::control::PaneOutput::new(
            self.id().clone(),
            events,
            sender,
        ))
    }

    /// Split this pane, putting a new one beside it.
    ///
    /// [`Window::split`] divides whichever pane is active; this divides the
    /// one you name, which is what building a layout needs.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the split, which includes a pane
    /// too small to divide.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::{PaneSize, SplitDirection, SplitOptions};
    ///
    /// let session = server.new_session("split-from-pane").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// let above = pane
    ///     .split(
    ///         SplitOptions::new(SplitDirection::Above)
    ///             .size(PaneSize::Percent(30))
    ///             .command("sleep 300"),
    ///     )
    ///     .await?;
    ///
    /// assert_ne!(above.id(), pane.id());
    /// assert_eq!(above.window_id(), pane.window_id());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn split(&self, options: impl Into<crate::SplitOptions>) -> Result<Self, Error> {
        let options = options.into();
        let pane = self.id().to_string();
        let projection =
            listing::create_pane(&self.core, |format| options.into_command(&pane, format)).await?;

        Ok(Self::new(Arc::clone(&self.core), projection))
    }

    /// Move one edge of the pane by a number of cells.
    ///
    /// This is the form a keybinding uses: "two rows taller" rather than a
    /// size computed from the current one. A pane that is alone in its window
    /// has no edge to move, and tmux accepts the request without doing
    /// anything.
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
    /// use libtmux::{ResizeDirection, SplitDirection, SplitOptions};
    ///
    /// let session = server.new_session("resized-pane").await?;
    /// let pane = session.panes().await?.remove(0);
    /// pane.split(SplitOptions::new(SplitDirection::Below).command("sleep 300")).await?;
    ///
    /// let mut pane = pane.refreshed().await?;
    /// let before = pane.height();
    /// pane.resize_by(ResizeDirection::Down, 2).await?;
    ///
    /// assert_eq!(pane.height(), before + 2);
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn resize_by(
        &mut self,
        direction: crate::ResizeDirection,
        cells: u32,
    ) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "resize-pane",
            Command::new("resize-pane")
                .arg("-t")
                .arg(self.id().to_string())
                .arg(direction.flag())
                .arg(cells.to_string()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Zoom the pane to fill its window, or restore it if it already fills it.
    ///
    /// tmux models this as one toggle rather than two operations, and
    /// [`Window::is_zoomed`] reports which way it went.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the request.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::{SplitDirection, SplitOptions};
    ///
    /// let session = server.new_session("zoomed").await?;
    /// let pane = session.panes().await?.remove(0);
    /// pane.split(SplitOptions::new(SplitDirection::Below).command("sleep 300")).await?;
    ///
    /// let mut pane = pane.refreshed().await?;
    /// pane.toggle_zoom().await?;
    /// assert!(pane.window().await?.expect("the window").is_zoomed());
    ///
    /// pane.toggle_zoom().await?;
    /// assert!(!pane.window().await?.expect("the window").is_zoomed());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn toggle_zoom(&mut self) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "resize-pane",
            Command::new("resize-pane")
                .arg("-t")
                .arg(self.id().to_string())
                .arg("-Z"),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Send keys to the pane as if typed.
    ///
    /// The text is sent literally with tmux's `-l` flag, so key names such as
    /// `C-c` are typed rather than interpreted. Use [`Pane::send_key_names`]
    /// for tmux's key vocabulary.
    ///
    /// The text is marked sensitive, so it never reaches `Debug`, an error, or
    /// a tracing span: a pane is where passwords get typed.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn send_keys(&self, keys: impl Into<OsString>) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "send-keys",
            Command::new("send-keys")
                .arg("-t")
                .arg(self.id().to_string())
                .arg("-l")
                .sensitive_arg(keys.into()),
        )
        .await
    }

    /// Send tmux key names, such as `C-c` or `Enter`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize a key name.
    pub async fn send_key_names<I, K>(&self, keys: I) -> Result<(), Error>
    where
        I: IntoIterator<Item = K>,
        K: Into<OsString>,
    {
        let mut command = Command::new("send-keys")
            .arg("-t")
            .arg(self.id().to_string());
        for key in keys {
            command = command.arg(key.into());
        }

        listing::mutate(&self.core, "send-keys", command).await
    }

    /// Capture the pane's visible contents, one entry per line.
    ///
    /// Lines are [`TmuxText`] because a terminal's contents are arbitrary
    /// bytes, not guaranteed UTF-8.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn capture(&self) -> Result<Vec<TmuxText>, Error> {
        self.capture_with(CaptureOptions::visible()).await
    }

    /// Read the pane's contents, choosing how much and in what form.
    ///
    /// [`Pane::capture`] reads the visible screen. This reaches into
    /// scrollback, which is where the output a caller is looking for has
    /// usually gone by the time anyone asks.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the capture, which includes a pane
    /// that has been closed.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// # let guard = libtmux::test::TestServer::new().await?;
    /// # let server = guard.server();
    /// use libtmux::CaptureOptions;
    ///
    /// let session = server.new_session("captured").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// let visible = pane.capture_with(CaptureOptions::visible()).await?;
    /// let everything = pane.capture_with(CaptureOptions::history()).await?;
    /// assert!(everything.len() >= visible.len());
    ///
    /// let last_ten = pane.capture_with(CaptureOptions::visible().start(-10)).await?;
    /// assert!(last_ten.len() >= visible.len());
    /// # guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capture_with(&self, options: CaptureOptions) -> Result<Vec<TmuxText>, Error> {
        let command = options.into_command(self.id().as_ref());
        let target = command.target().map(OsStr::to_os_string);
        let result = self.core.execute(command).await?;
        if !result.success() {
            return Err(Error::refused(
                "capture-pane",
                result.exit_code(),
                result.stderr_lossy().into_owned(),
                target.as_deref(),
            ));
        }

        // tmux terminates every line, including the last, so a trailing empty
        // element after the final newline is framing rather than content.
        let stdout = result.stdout();
        let stdout = stdout.strip_suffix(b"\n").unwrap_or(stdout);
        if stdout.is_empty() {
            return Ok(Vec::new());
        }

        Ok(stdout
            .split(|byte| *byte == b'\n')
            .map(|line| TmuxText::from(line.to_vec()))
            .collect())
    }

    /// Capture with the per-line flags tmux records, marking shell prompts.
    ///
    /// tmux records where a prompt and its output begin from the OSC 133
    /// sequences a shell emits, and keeps those marks as lines scroll into
    /// history. That is what answers "show me the last command's output"
    /// exactly, rather than by guessing at a prompt's shape.
    ///
    /// Every [`CapturedLine::starts_prompt`] is `false` when the pane's shell
    /// does not emit OSC 133, which is the common case: fish does, bash and
    /// zsh do not without shell integration installed. An empty result is a
    /// real answer about the shell, not a failure.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] below tmux 3.7, which is where
    /// `capture-pane -F` arrived, and an error when tmux refuses the capture.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::CaptureOptions;
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    /// let session = server.new_session("prompts").await?;
    /// let pane = session.panes().await?.remove(0);
    ///
    /// if server.capabilities().await?.tmux_version().meets(&libtmux::since::CAPTURE_LINE_FLAGS) {
    ///     let lines = pane.capture_lines(CaptureOptions::history()).await?;
    ///     // Without shell integration nothing is marked, which is an answer.
    ///     let prompts = lines.iter().filter(|line| line.starts_prompt).count();
    ///     assert!(prompts <= lines.len());
    /// }
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn capture_lines(&self, options: CaptureOptions) -> Result<Vec<CapturedLine>, Error> {
        crate::Server::from_core(Arc::clone(&self.core))
            .require("capture-pane -F", crate::version::since::CAPTURE_LINE_FLAGS)
            .await?;

        Ok(self
            .capture_with(options.line_flags())
            .await?
            .into_iter()
            .map(|row| CapturedLine::parse(&row))
            .collect())
    }

    /// Make this pane active in its window.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn select(&mut self) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "select-pane",
            Command::new("select-pane")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Resize the pane to an exact size in cells.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the size.
    pub async fn resize(&mut self, width: u32, height: u32) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "resize-pane",
            Command::new("resize-pane")
                .arg("-t")
                .arg(self.id().to_string())
                .arg("-x")
                .arg(width.to_string())
                .arg("-y")
                .arg(height.to_string()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Kill the pane.
    ///
    /// This consumes the handle. Killing a window's last pane closes the
    /// window.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn kill(self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "kill-pane",
            Command::new("kill-pane")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Read one option's exact stored value.
    ///
    /// A user option, whose name begins with `@`, exists only while it is
    /// set, so an unset one reports `None`. A built-in option always exists,
    /// so an unset one also reports `None`. An unrecognized built-in name is
    /// an error.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        let target = self.id().to_string();
        options::get(&self.core, options::Scope::Pane(&target), name).await
    }

    /// List the option names set at this pane's scope.
    ///
    /// Values are not included: tmux renders them for display with three
    /// different quoting styles, so re-parsing them would be guesswork. Read
    /// each value with [`Self::get_option`], which returns exact bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing.
    pub async fn option_names(&self) -> Result<Vec<String>, Error> {
        let target = self.id().to_string();
        options::names(&self.core, options::Scope::Pane(&target)).await
    }

    /// Read every option set on this pane, decoded by its declared kind.
    ///
    /// Costs one tmux command per option, because each value is read as the
    /// bytes tmux stored rather than the form it lists them in. Use
    /// [`Self::option_names`] when only the names are wanted, and
    /// [`Self::typed_option`] for one value.
    ///
    /// Reports what is set *at this scope*, not what the object resolves to.
    /// A session that has set nothing of its own answers empty even though
    /// every option still has an effective value it inherits.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
    /// An empty map means nothing is set, never that the listing failed.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("opts").await?;
    /// let window = session.active_window().await?.expect("a session has a window");
    /// let pane = window.active_pane().await?.expect("a window has a pane");
    ///
    /// pane.set_option("@marker", "set").await?;
    /// assert!(pane.options().await?.contains_key("@marker"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        let target = self.id().to_string();
        options::typed_all(&self.core, options::Scope::Pane(&target)).await
    }

    /// Set one option.
    ///
    /// The value is marked sensitive, so it never reaches `Debug`, an error,
    /// or a tracing span.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(
            &self.core,
            options::Scope::Pane(&target),
            name,
            value,
            false,
        )
        .await
    }

    /// Append to one option rather than replacing it.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn append_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(&self.core, options::Scope::Pane(&target), name, value, true).await
    }

    /// Remove one option, restoring whatever it inherits.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_option(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset(&self.core, options::Scope::Pane(&target), name).await
    }

    /// Set one hook to a tmux command.
    ///
    /// Hooks live in the same option tables, so a hook is an array option and
    /// [`Self::get_option`] reads it under an indexed name such as
    /// `after-new-window[0]`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name or command.
    pub async fn set_hook(&self, name: &str, command: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set_hook(&self.core, options::Scope::Pane(&target), name, command).await
    }

    /// Remove one hook.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name.
    pub async fn unset_hook(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset_hook(&self.core, options::Scope::Pane(&target), name).await
    }

    /// Run a raw tmux command against this pane.
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
    /// let pane = window.active_pane().await?.expect("a pane");
    ///
    /// let result = pane
    ///     .cmd(libtmux::Command::new("display-message").arg("-p").arg("#{pane_id}"))
    ///     .await?;
    /// assert_eq!(result.stdout_lossy().trim(), pane.id().to_string());
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

    /// Expand a tmux format string in this pane's context.
    ///
    /// The reading half of tmux's `display-message`, separated from the
    /// showing half ([`Self::display`]) because they answer different
    /// questions: one returns text to the caller, the other puts text in
    /// front of a person.
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
    /// let pane = window.active_pane().await?.expect("a pane");
    ///
    /// let expanded = pane.format("#{pane_id}").await?;
    /// assert_eq!(expanded.to_string_lossy(), pane.id().to_string());
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
            return Err(Error::refused(
                "display-message",
                result.exit_code(),
                result.stderr_lossy().into_owned(),
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

    /// Show a message on the clients viewing this pane's window.
    ///
    /// The showing half of tmux's `display-message`. Nothing is returned
    /// because nothing is read: use [`Self::format`] to get text back.
    /// Succeeds when nobody is watching, having shown it to nobody.
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

        Err(Error::refused(
            "display-message",
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            Some(OsStr::new(&self.id().to_string())),
        ))
    }

    /// Read one hook's commands, or `None` when it holds nothing.
    ///
    /// There is deliberately no listing counterpart at this scope. tmux does
    /// not enumerate hooks set on a window or a pane: `show-hooks` reports
    /// nothing for them, and `show-options` omits them while still listing
    /// ordinary options. A listing here could only ever answer empty, which
    /// would read as "no hooks" rather than "tmux will not say". Reading one
    /// by name works, so that is what is offered; [`crate::Server::hooks`] and
    /// [`crate::Session::hooks`] list the scopes tmux does enumerate.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let session = guard.server().new_session("hooked").await?;
    /// let window = session.active_window().await?.expect("a session has a window");
    /// let pane = window.active_pane().await?.expect("a window has a pane");
    ///
    /// assert!(pane.hook("alert-bell").await?.is_none());
    /// pane.set_hook("alert-bell", "display-message rang").await?;
    /// assert!(pane.hook("alert-bell").await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hook(&self.core, options::Scope::Pane(&target), name).await
    }

    /// Set the pane's title.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the title.
    pub async fn set_title(&mut self, title: impl Into<OsString>) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "select-pane",
            Command::new("select-pane")
                .arg("-t")
                .arg(self.id().to_string())
                .arg("-T")
                .sensitive_arg(title.into()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Clear the pane's scrollback history.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn clear_history(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "clear-history",
            Command::new("clear-history")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Paste a buffer's contents into the pane.
    ///
    /// Passing `None` pastes the most recent buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when no such buffer exists.
    pub async fn paste_buffer(&self, name: Option<&str>) -> Result<(), Error> {
        let mut command = Command::new("paste-buffer")
            .arg("-t")
            .arg(self.id().to_string());
        if let Some(name) = name {
            command = command.arg("-b").arg(OsString::from(name));
        }

        listing::mutate(&self.core, "paste-buffer", command).await
    }

    /// Pipe the pane's output to a shell command.
    ///
    /// Passing `None` stops any pipe already running for this pane.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn pipe(&self, command: Option<impl Into<OsString>>) -> Result<(), Error> {
        let mut pipe = Command::new("pipe-pane")
            .arg("-t")
            .arg(self.id().to_string());
        if let Some(command) = command {
            pipe = pipe.sensitive_arg(command.into());
        }

        listing::mutate(&self.core, "pipe-pane", pipe).await
    }

    /// Restart the pane's command in place.
    ///
    /// Passing `None` reruns whatever the pane started with.
    ///
    /// # Errors
    ///
    /// Returns an error when the pane is still running and `kill` is not set.
    pub async fn respawn(
        &mut self,
        command: Option<impl Into<OsString>>,
        kill: bool,
    ) -> Result<&mut Self, Error> {
        let mut respawn = Command::new("respawn-pane")
            .arg("-t")
            .arg(self.id().to_string());
        if kill {
            respawn = respawn.arg("-k");
        }
        if let Some(command) = command {
            respawn = respawn.arg(command.into());
        }

        listing::mutate(&self.core, "respawn-pane", respawn).await?;
        self.refresh().await?;
        Ok(self)
    }

    /// Swap this pane's position with another.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the swap.
    pub async fn swap_with(&mut self, other: &Self) -> Result<&mut Self, Error> {
        listing::mutate(
            &self.core,
            "swap-pane",
            Command::new("swap-pane")
                .arg("-s")
                .arg(self.id().to_string())
                .arg("-t")
                .arg(other.id().to_string()),
        )
        .await?;

        self.refresh().await?;
        Ok(self)
    }

    /// Move this pane out into a window of its own.
    ///
    /// This consumes the handle, because the pane's window changes and any
    /// snapshot of its old position is now wrong.
    ///
    /// # Errors
    ///
    /// Returns an error when the pane is its window's only one, since tmux
    /// has nothing to break it out of.
    pub async fn break_out(self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "break-pane",
            Command::new("break-pane")
                .arg("-d")
                .arg("-s")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Move this pane into another window, beside a pane already there.
    ///
    /// The inverse of [`Pane::break_out`], and it consumes the handle for the
    /// same reason: the pane's window changes, so a snapshot of where it used
    /// to be is wrong. The pane keeps its id and everything running in it, so
    /// the handle returned is this same pane read again where it now lives.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the move, which includes joining a
    /// pane to its own window.
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
    /// let elsewhere = session.new_window("elsewhere").await?;
    ///
    /// let stranded = elsewhere.panes().await?.remove(0);
    /// let here = window.panes().await?.remove(0);
    /// let moved = stranded
    ///     .join_into(&here, JoinOptions::new(SplitDirection::Below))
    ///     .await?;
    ///
    /// assert_eq!(moved.window_id(), window.id());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn join_into(
        self,
        beside: &Self,
        options: crate::JoinOptions,
    ) -> Result<Self, Error> {
        let command = options.apply(
            Command::new("join-pane")
                .arg("-d")
                .arg("-s")
                .arg(self.id().to_string())
                .arg("-t")
                .arg(beside.id().to_string()),
        );
        listing::mutate(&self.core, "join-pane", command).await?;

        self.refreshed().await
    }

    /// Enter copy mode.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn copy_mode(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "copy-mode",
            Command::new("copy-mode")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Leave copy mode, or any other mode the pane is in.
    ///
    /// A pane that is in no mode is left alone rather than refused, so this can
    /// be called to reach a known state without asking first.
    ///
    /// This dispatches `copy-mode -q` rather than the cancel key, because the
    /// key reaches a pane through the copy-mode key table and clock mode and
    /// tree mode have none: sending it there is answered "not in a mode" while
    /// the pane stays in one. `-q` is the only exit that covers every mode,
    /// including the ones [`Self::clock_mode`] and `choose-tree` create.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn exit_mode(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "copy-mode",
            Command::new("copy-mode")
                .arg("-q")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Show the clock in this pane.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn clock_mode(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "clock-mode",
            Command::new("clock-mode")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Send the configured prefix key to the pane.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the command.
    pub async fn send_prefix(&self) -> Result<(), Error> {
        listing::mutate(
            &self.core,
            "send-prefix",
            Command::new("send-prefix")
                .arg("-t")
                .arg(self.id().to_string()),
        )
        .await
    }

    /// Read one option, decoded according to what tmux declares about it.
    ///
    /// A flag comes back as [`OptionValue::Flag`] and a number as
    /// [`OptionValue::Number`], so a caller does not decide for itself that
    /// `on` means one. Everything else, including user options, stays text.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn typed_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        let target = self.id().to_string();
        Ok(
            options::get(&self.core, options::Scope::Pane(&target), name)
                .await?
                .map(|value| OptionValue::decode(name, value)),
        )
    }
}

/// Panes compare by server endpoint and pane id.
///
/// Unlike [`Window`](crate::Window), the discovery link is not part of pane
/// identity: a pane exists in exactly one window, so two handles with the same
/// pane id name the same pane however they were reached.
impl PartialEq for Pane {
    fn eq(&self, other: &Self) -> bool {
        self.server_identity() == other.server_identity() && self.id() == other.id()
    }
}

impl Eq for Pane {}

impl Hash for Pane {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.server_identity().hash(state);
        self.id().hash(state);
    }
}

/// Renders identity only, never snapshot text.
///
/// Pane titles and paths carry arbitrary bytes from the user's shell, so they
/// stay out of diagnostics.
impl fmt::Debug for Pane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pane")
            .field("id", &self.id())
            .field("window_id", &self.window_id())
            .finish_non_exhaustive()
    }
}

/// Filtering a pane uses the same handles as the snapshot beneath it.
///
/// Matching and validation delegate to that snapshot, so an expression can
/// only name fields the catalog knows. The companion is re-parameterized to
/// [`Pane`] so the type a listing returns is the type an expression
/// filters.
#[cfg(feature = "query")]
impl Filterable for Pane {
    type Fields = PaneFields<Self>;

    const FILTER_TARGET: &'static str = <PaneInfo as Filterable>::FILTER_TARGET;

    fn filter_fields() -> Self::Fields {
        Self::Fields::for_target(Self::FILTER_TARGET)
    }

    fn __filter_matches(&self, predicate: &crate::query::__private::Predicate) -> bool {
        self.projection.pane().__filter_matches(predicate)
    }

    fn __filter_validate(
        predicate: &crate::query::__private::Predicate,
    ) -> Result<(), crate::query::FilterExpressionError> {
        <PaneInfo as Filterable>::__filter_validate(predicate)
    }
}

/// Renders the pane id, which is what a tmux target wants.
impl fmt::Display for Pane {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.id())
    }
}

/// How far back a capture reaches, and in what form.
///
/// Line numbers follow tmux: zero is the top of the visible screen, negative
/// numbers are scrollback, and positive numbers run down the screen.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::CaptureOptions;
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let session = guard.server().new_session("capture").await?;
/// let window = session.active_window().await?.expect("a session has a window");
/// let pane = window.active_pane().await?.expect("a window has a pane");
///
/// // The constructors name the two questions people actually ask, rather
/// // than making a caller remember that zero is the top of the screen.
/// let visible = pane.capture_with(CaptureOptions::visible()).await?;
/// let everything = pane.capture_with(CaptureOptions::history()).await?;
/// assert!(everything.len() >= visible.len());
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[must_use = "options describe a capture but do not perform one"]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CaptureOptions {
    start: Option<CaptureBound>,
    end: Option<CaptureBound>,
    escape_sequences: bool,
    join_wrapped: bool,
    line_flags: bool,
}

impl CaptureOptions {
    /// Capture the visible screen, which is what tmux does by default.
    pub const fn visible() -> Self {
        Self {
            start: None,
            end: None,
            escape_sequences: false,
            join_wrapped: false,
            line_flags: false,
        }
    }

    /// Capture everything tmux still holds, scrollback included.
    pub const fn history() -> Self {
        Self {
            start: Some(CaptureBound::Limit),
            ..Self::visible()
        }
    }

    /// Start the capture at a line.
    pub const fn start(mut self, line: i32) -> Self {
        self.start = Some(CaptureBound::Line(line));
        self
    }

    /// End the capture at a line.
    pub const fn end(mut self, line: i32) -> Self {
        self.end = Some(CaptureBound::Line(line));
        self
    }

    /// Keep the terminal escape sequences rather than the text alone.
    pub const fn escape_sequences(mut self) -> Self {
        self.escape_sequences = true;
        self
    }

    /// Ask tmux for the per-line flags, which mark where prompts begin.
    ///
    /// Only [`Pane::capture_lines`] reads these; a plain capture would carry
    /// them as text at the front of every line.
    const fn line_flags(mut self) -> Self {
        self.line_flags = true;
        self
    }

    /// Return a wrapped line as one line rather than as the rows it occupies.
    pub const fn join_wrapped(mut self) -> Self {
        self.join_wrapped = true;
        self
    }

    /// Lower these options into a `capture-pane` command for one pane.
    fn into_command(self, pane: &str) -> Command {
        let mut command = Command::new("capture-pane").arg("-p").arg("-t").arg(pane);
        if let Some(start) = self.start {
            command = command.arg("-S").arg(start.to_string());
        }
        if let Some(end) = self.end {
            command = command.arg("-E").arg(end.to_string());
        }
        if self.line_flags {
            command = command.arg("-F");
        }
        if self.escape_sequences {
            command = command.arg("-e");
        }
        if self.join_wrapped {
            command = command.arg("-J");
        }
        command
    }
}

/// One end of a capture range.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CaptureBound {
    /// tmux's `-`: as far as the history goes in that direction.
    Limit,
    /// A line number.
    Line(i32),
}

/// One line of a capture, with what tmux knows about it.
///
/// Built by [`Pane::capture_lines`]. The marks come from the OSC 133 sequences
/// a shell emits around its prompt, so they are present only when the pane's
/// shell emits them.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux::{CaptureOptions, CapturedLine};
///
/// let guard = libtmux::test::TestServer::new().await?;
/// let server = guard.server();
/// let session = server.new_session("marked").await?;
/// let pane = session.panes().await?.remove(0);
///
/// if server.capabilities().await?.tmux_version().meets(&libtmux::since::CAPTURE_LINE_FLAGS) {
///     let lines: Vec<CapturedLine> = pane.capture_lines(CaptureOptions::visible()).await?;
///     assert!(lines.iter().all(|line| !line.starts_output || !line.starts_prompt));
/// }
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct CapturedLine {
    /// The line's text, without the flags tmux prefixed to it.
    pub text: TmuxText,
    /// Whether a shell prompt begins on this line.
    pub starts_prompt: bool,
    /// Whether a command's output begins on this line.
    pub starts_output: bool,
    /// Whether the line continues onto the next row rather than ending.
    pub wrapped: bool,
}

impl CapturedLine {
    /// Split one `capture-pane -F` row into its flags and its text.
    ///
    /// tmux writes the flags, then a space, then the line, and writes `-` when
    /// a line has none, so the separator is always present.
    fn parse(row: &TmuxText) -> Self {
        let bytes = row.as_bytes();
        let (flags, text) = bytes
            .iter()
            .position(|byte| *byte == b' ')
            .map_or((bytes, [].as_slice()), |index| {
                (&bytes[..index], &bytes[index + 1..])
            });

        Self {
            starts_prompt: flags.contains(&b'P'),
            starts_output: flags.contains(&b'O'),
            wrapped: flags.contains(&b'W'),
            text: TmuxText::from_bytes(text),
        }
    }
}

impl fmt::Display for CaptureBound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Limit => formatter.write_str("-"),
            Self::Line(line) => write!(formatter, "{line}"),
        }
    }
}

/// Read a pane ID out of an environment value tmux set.
///
/// Absent and malformed are different answers. Nothing set the variable means
/// this process was not started by tmux; a variable that is set and does not
/// name a pane means something else wrote it, or wrote it wrongly, and telling
/// a caller they are not inside tmux sends them to check the wrong thing.
///
/// [`Server::from_env_value`] already draws this line for `TMUX`, which is the
/// same variable family and the same question.
fn parse_env_id(value: Option<&OsStr>) -> Result<PaneId, Error> {
    let Some(value) = value else {
        return Err(Error::invalid_server_configuration(
            crate::ServerConfigurationErrorKind::NotInsideTmux,
        ));
    };

    value
        .to_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            Error::invalid_server_configuration(
                crate::ServerConfigurationErrorKind::MalformedTmuxVariable,
            )
        })
}
