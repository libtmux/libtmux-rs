use crate::{
    AccessMode, AccessRule, ChannelWait, Chooser, Client, Command, CommandChain, CommandResult,
    EngineCapabilities, EnvironmentEntry, Error, IndexedHooks, KeyBinding, MenuItem,
    NewSessionOptions, OptionValue, Pane, PaneId, PromptKind, ReplaceMode, Server,
    ServerGeneration, ServerIdentity, Session, SessionId, SessionTree, SparseValues, TmuxText,
    Window, WindowId,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

impl Server {
    /// Report whether this process opened the control client with this PID.
    ///
    /// Uses [`libtmux::Server::owns_control_client`] with this server's Tokio driver.
    #[cfg(feature = "control-mode")]
    #[must_use]
    pub fn owns_control_client(&self, pid: u32) -> bool {
        self.0.owns_control_client(pid)
    }

    /// Return the captured structural endpoint identity.
    ///
    /// Uses [`libtmux::Server::identity`] with this server's Tokio driver.
    #[must_use]
    pub fn identity(&self) -> &ServerIdentity {
        self.0.identity()
    }

    /// Return the captured absolute socket path used for identity and dispatch.
    ///
    /// Uses [`libtmux::Server::socket_path`] with this server's Tokio driver.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        self.0.socket_path()
    }

    /// Return the configured named socket selector, if any.
    ///
    /// Uses [`libtmux::Server::socket_name`] with this server's Tokio driver.
    #[must_use]
    pub fn socket_name(&self) -> Option<&OsStr> {
        self.0.socket_name()
    }

    /// Return the captured config path, if configured.
    ///
    /// Uses [`libtmux::Server::config_file`] with this server's Tokio driver.
    #[must_use]
    pub fn config_file(&self) -> Option<&Path> {
        self.0.config_file()
    }

    /// Return the configured tmux color mode.
    ///
    /// Uses [`libtmux::Server::colors`] with this server's Tokio driver.
    #[must_use]
    pub fn colors(&self) -> Option<u16> {
        self.0.colors()
    }

    /// Return the captured tmux executable value.
    ///
    /// Uses [`libtmux::Server::tmux_executable`] with this server's Tokio driver.
    #[must_use]
    pub fn tmux_executable(&self) -> &OsStr {
        self.0.tmux_executable()
    }

    /// Resolve the configured tmux executable from the captured launch context.
    ///
    /// Uses [`libtmux::Server::resolved_tmux_executable`] with this server's Tokio driver.
    #[must_use]
    pub fn resolved_tmux_executable(&self) -> Option<PathBuf> {
        self.0.resolved_tmux_executable()
    }

    /// Return the captured per-command timeout.
    ///
    /// Uses [`libtmux::Server::default_timeout`] with this server's Tokio driver.
    #[must_use]
    pub fn default_timeout(&self) -> Duration {
        self.0.default_timeout()
    }

    /// Detect and share capabilities for the configured tmux executable.
    ///
    /// Uses [`libtmux::Server::capabilities`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::capabilities`] for failure conditions.
    pub async fn capabilities(&self) -> Result<&EngineCapabilities, Error> {
        self.0.capabilities().await
    }

    /// Execute one raw logical tmux command.
    ///
    /// Uses [`libtmux::Server::cmd`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::cmd`] for failure conditions.
    pub async fn cmd(&self, command: Command) -> Result<CommandResult, Error> {
        self.0.cmd(command).await
    }

    /// Validate every planned layout before scripts or tmux mutations.
    ///
    /// Uses [`libtmux::Server::validate_layouts`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::validate_layouts`] for failure conditions.
    pub async fn validate_layouts<'a>(
        &self,
        layouts: impl IntoIterator<Item = (&'a OsStr, usize)>,
    ) -> Result<(), Error> {
        self.0.validate_layouts(layouts).await
    }

    /// Dispatch several commands as one `tmux a \; b` invocation.
    ///
    /// Uses [`libtmux::Server::chain`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::chain`] for failure conditions.
    pub async fn chain(&self, chain: CommandChain) -> Result<CommandResult, Error> {
        self.0.chain(chain).await
    }

    /// Stop accepting clients and wait for every active client process.
    ///
    /// Uses [`libtmux::Server::shutdown`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::shutdown`] for failure conditions.
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.0.shutdown().await
    }

    /// List the sessions that have at least one client attached.
    ///
    /// Uses [`libtmux::Server::attached_sessions`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::attached_sessions`] for failure conditions.
    pub async fn attached_sessions(&self) -> Result<Vec<Session>, Error> {
        self.0.attached_sessions().await
    }

    /// Create a detached session and return it.
    ///
    /// Uses [`libtmux::Server::new_session`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::new_session`] for failure conditions.
    pub async fn new_session(
        &self,
        options: impl Into<NewSessionOptions>,
    ) -> Result<Session, Error> {
        self.0.new_session(options).await
    }

    /// Lock every client on the server.
    ///
    /// Uses [`libtmux::Server::lock_all`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::lock_all`] for failure conditions.
    pub async fn lock_all(&self) -> Result<(), Error> {
        self.0.lock_all().await
    }

    /// Stop the tmux daemon at this endpoint.
    ///
    /// Uses [`libtmux::Server::kill`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::kill`] for failure conditions.
    pub async fn kill(&self) -> Result<(), Error> {
        self.0.kill().await
    }

    /// Store data in a tmux paste buffer.
    ///
    /// Uses [`libtmux::Server::set_buffer`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_buffer`] for failure conditions.
    pub async fn set_buffer(
        &self,
        name: Option<&str>,
        data: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.set_buffer(name, data).await
    }

    /// Fill a paste buffer from a file, letting tmux read it.
    ///
    /// Uses [`libtmux::Server::load_buffer`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::load_buffer`] for failure conditions.
    pub async fn load_buffer(
        &self,
        name: Option<&str>,
        path: impl AsRef<Path>,
    ) -> Result<(), Error> {
        self.0.load_buffer(name, path).await
    }

    /// Write a paste buffer to a file, letting tmux do the writing.
    ///
    /// Uses [`libtmux::Server::save_buffer`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::save_buffer`] for failure conditions.
    pub async fn save_buffer(
        &self,
        name: Option<&str>,
        path: impl AsRef<Path>,
    ) -> Result<(), Error> {
        self.0.save_buffer(name, path).await
    }

    /// Read a paste buffer's exact bytes.
    ///
    /// Uses [`libtmux::Server::buffer`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::buffer`] for failure conditions.
    pub async fn buffer(&self, name: impl AsRef<[u8]>) -> Result<Option<Vec<u8>>, Error> {
        self.0.buffer(name).await
    }

    /// List the paste buffer names.
    ///
    /// Uses [`libtmux::Server::buffer_names`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::buffer_names`] for failure conditions.
    pub async fn buffer_names(&self) -> Result<Vec<TmuxText>, Error> {
        self.0.buffer_names().await
    }

    /// Delete one paste buffer.
    ///
    /// Uses [`libtmux::Server::delete_buffer`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::delete_buffer`] for failure conditions.
    pub async fn delete_buffer(&self, name: impl AsRef<[u8]>) -> Result<(), Error> {
        self.0.delete_buffer(name).await
    }

    /// Bind a key in one key table.
    ///
    /// Uses [`libtmux::Server::bind_key`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::bind_key`] for failure conditions.
    pub async fn bind_key(
        &self,
        table: &str,
        key: &str,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.bind_key(table, key, command).await
    }

    /// Remove a key binding from one key table.
    ///
    /// Uses [`libtmux::Server::unbind_key`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::unbind_key`] for failure conditions.
    pub async fn unbind_key(&self, table: &str, key: &str) -> Result<(), Error> {
        self.0.unbind_key(table, key).await
    }

    /// List key bindings as tmux prints them.
    ///
    /// Uses [`libtmux::Server::key_bindings`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::key_bindings`] for failure conditions.
    pub async fn key_bindings(&self, table: Option<&str>) -> Result<Vec<String>, Error> {
        self.0.key_bindings(table).await
    }

    /// Expand a tmux format string and return the result.
    ///
    /// Uses [`libtmux::Server::format`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::format`] for failure conditions.
    pub async fn format(&self, pane: Option<&Pane>, format: &str) -> Result<TmuxText, Error> {
        self.0.format(pane, format).await
    }

    /// The entries tmux remembers for one kind of prompt.
    ///
    /// Uses [`libtmux::Server::prompt_history`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::prompt_history`] for failure conditions.
    pub async fn prompt_history(&self, kind: PromptKind) -> Result<Vec<TmuxText>, Error> {
        self.0.prompt_history(kind).await
    }

    /// Forget the entries tmux remembers for prompts.
    ///
    /// Uses [`libtmux::Server::clear_prompt_history`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::clear_prompt_history`] for failure conditions.
    pub async fn clear_prompt_history(&self) -> Result<(), Error> {
        self.0.clear_prompt_history().await
    }

    /// The users on this server's access list.
    ///
    /// Uses [`libtmux::Server::access_rules`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::access_rules`] for failure conditions.
    pub async fn access_rules(&self) -> Result<Vec<AccessRule>, Error> {
        self.0.access_rules().await
    }

    /// Let a user attach to this server.
    ///
    /// Uses [`libtmux::Server::grant_access`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::grant_access`] for failure conditions.
    pub async fn grant_access(&self, user: &str, mode: AccessMode) -> Result<(), Error> {
        self.0.grant_access(user, mode).await
    }

    /// Stop a user attaching to this server.
    ///
    /// Uses [`libtmux::Server::revoke_access`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::revoke_access`] for failure conditions.
    pub async fn revoke_access(&self, user: &str) -> Result<(), Error> {
        self.0.revoke_access(user).await
    }

    /// Load a tmux configuration file into this server.
    ///
    /// Uses [`libtmux::Server::source_file`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::source_file`] for failure conditions.
    pub async fn source_file(&self, path: impl Into<PathBuf>) -> Result<(), Error> {
        self.0.source_file(path).await
    }

    /// Create a session, run an operation with it, then kill it.
    ///
    /// Uses [`libtmux::Server::with_session`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::with_session`] for failure conditions.
    ///
    /// # Cancel safety
    ///
    /// See [`libtmux::Server::with_session`] for cancellation and cleanup guarantees.
    pub async fn with_session<T, E>(
        &self,
        options: impl Into<NewSessionOptions>,
        operation: impl AsyncFnOnce(&Session) -> Result<T, E>,
    ) -> Result<T, crate::ScopeError<T, E>> {
        self.0.with_session(options, operation).await
    }

    /// Run a shell command through tmux and collect its output.
    ///
    /// Uses [`libtmux::Server::run_shell`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::run_shell`] for failure conditions.
    ///
    /// # Cancel safety
    ///
    /// See [`libtmux::Server::run_shell`] for cancellation and cleanup guarantees.
    pub async fn run_shell(&self, command: impl Into<OsString>) -> Result<Vec<TmuxText>, Error> {
        self.0.run_shell(command).await
    }

    /// Run a shell command in the background and return immediately.
    ///
    /// Uses [`libtmux::Server::spawn_shell`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::spawn_shell`] for failure conditions.
    pub async fn spawn_shell(&self, command: impl Into<OsString>) -> Result<(), Error> {
        self.0.spawn_shell(command).await
    }

    /// Start a tmux server without creating a session.
    ///
    /// Uses [`libtmux::Server::start`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::start`] for failure conditions.
    pub async fn start(&self) -> Result<(), Error> {
        self.0.start().await
    }

    /// Which tmux daemon is answering on this endpoint.
    ///
    /// Uses [`libtmux::Server::generation`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::generation`] for failure conditions.
    pub async fn generation(&self) -> Result<ServerGeneration, Error> {
        self.0.generation().await
    }

    /// Fail unless the daemon answering is still the one that was captured.
    ///
    /// Uses [`libtmux::Server::require_generation`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::require_generation`] for failure conditions.
    pub async fn require_generation(&self, expected: ServerGeneration) -> Result<(), Error> {
        self.0.require_generation(expected).await
    }

    /// Report whether a tmux daemon is answering at this endpoint.
    ///
    /// Uses [`libtmux::Server::is_alive`] with this server's Tokio driver.
    pub async fn is_alive(&self) -> bool {
        self.0.is_alive().await
    }

    /// Require a tmux daemon to be answering at this endpoint.
    ///
    /// Uses [`libtmux::Server::check_alive`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::check_alive`] for failure conditions.
    pub async fn check_alive(&self) -> Result<(), Error> {
        self.0.check_alive().await
    }

    /// Signal a `wait-for` channel, releasing anything waiting on it.
    ///
    /// Uses [`libtmux::Server::signal_channel`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::signal_channel`] for failure conditions.
    pub async fn signal_channel(&self, channel: &str) -> Result<(), Error> {
        self.0.signal_channel(channel).await
    }

    /// Hold a `wait-for` channel for the length of an operation.
    ///
    /// Uses [`libtmux::Server::with_channel_lock`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::with_channel_lock`] for failure conditions.
    ///
    /// # Cancel safety
    ///
    /// See [`libtmux::Server::with_channel_lock`] for cancellation and cleanup guarantees.
    pub async fn with_channel_lock<T, E>(
        &self,
        channel: &str,
        operation: impl AsyncFnOnce(&Self) -> Result<T, E>,
    ) -> Result<T, crate::ScopeError<T, E>> {
        self.0
            .with_channel_lock(channel, async |_| operation(self).await)
            .await
    }

    /// Lock a `wait-for` channel, blocking later lock attempts on it.
    ///
    /// Uses [`libtmux::Server::lock_channel`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::lock_channel`] for failure conditions.
    ///
    /// # Cancel safety
    ///
    /// See [`libtmux::Server::lock_channel`] for cancellation and cleanup guarantees.
    pub async fn lock_channel(&self, channel: &str) -> Result<(), Error> {
        self.0.lock_channel(channel).await
    }

    /// Unlock a `wait-for` channel.
    ///
    /// Uses [`libtmux::Server::unlock_channel`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::unlock_channel`] for failure conditions.
    pub async fn unlock_channel(&self, channel: &str) -> Result<(), Error> {
        self.0.unlock_channel(channel).await
    }

    /// Wait for a `wait-for` channel to be signalled.
    ///
    /// Uses [`libtmux::Server::wait_for_channel`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::wait_for_channel`] for failure conditions.
    ///
    /// # Cancel safety
    ///
    /// See [`libtmux::Server::wait_for_channel`] for cancellation and cleanup guarantees.
    pub async fn wait_for_channel(
        &self,
        channel: &str,
        within: Duration,
    ) -> Result<ChannelWait, Error> {
        self.0.wait_for_channel(channel, within).await
    }

    /// List every window on the server, preserving any failure.
    ///
    /// Uses [`libtmux::Server::windows`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::windows`] for failure conditions.
    pub async fn windows(&self) -> Result<Vec<Window>, Error> {
        self.0.windows().await
    }

    /// List every pane on the server, preserving any failure.
    ///
    /// Uses [`libtmux::Server::panes`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::panes`] for failure conditions.
    pub async fn panes(&self) -> Result<Vec<Pane>, Error> {
        self.0.panes().await
    }

    /// List every client attached to the server, preserving any failure.
    ///
    /// Uses [`libtmux::Server::clients`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::clients`] for failure conditions.
    pub async fn clients(&self) -> Result<Vec<Client>, Error> {
        self.0.clients().await
    }

    /// Find the session with this exact name.
    ///
    /// Uses [`libtmux::Server::session`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::session`] for failure conditions.
    pub async fn session(&self, name: impl AsRef<[u8]>) -> Result<Option<Session>, Error> {
        self.0.session(name).await
    }

    /// Find the session with this id.
    ///
    /// Uses [`libtmux::Server::session_by_id`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::session_by_id`] for failure conditions.
    pub async fn session_by_id(&self, id: &SessionId) -> Result<Option<Session>, Error> {
        self.0.session_by_id(id).await
    }

    /// Find the window with this id, through the first link that reaches it.
    ///
    /// Uses [`libtmux::Server::window_by_id`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::window_by_id`] for failure conditions.
    pub async fn window_by_id(&self, id: &WindowId) -> Result<Option<Window>, Error> {
        self.0.window_by_id(id).await
    }

    /// Find the pane with this id.
    ///
    /// Uses [`libtmux::Server::pane_by_id`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::pane_by_id`] for failure conditions.
    pub async fn pane_by_id(&self, id: &PaneId) -> Result<Option<Pane>, Error> {
        self.0.pane_by_id(id).await
    }

    /// Find the client attached to this terminal.
    ///
    /// Uses [`libtmux::Server::client`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::client`] for failure conditions.
    pub async fn client(&self, name: impl AsRef<[u8]>) -> Result<Option<Client>, Error> {
        self.0.client(name).await
    }

    /// Fetch the whole hierarchy in three commands.
    ///
    /// Uses [`libtmux::Server::hierarchy`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::hierarchy`] for failure conditions.
    pub async fn hierarchy(&self) -> Result<Vec<SessionTree>, Error> {
        self.0.hierarchy().await
    }

    /// Report whether a session with this exact name exists.
    ///
    /// Uses [`libtmux::Server::has_session`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::has_session`] for failure conditions.
    pub async fn has_session(&self, name: impl AsRef<[u8]>) -> Result<bool, Error> {
        self.0.has_session(name).await
    }

    /// List every session on the server, preserving any failure.
    ///
    /// Uses [`libtmux::Server::sessions`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::sessions`] for failure conditions.
    pub async fn sessions(&self) -> Result<Vec<Session>, Error> {
        self.0.sessions().await
    }

    /// Show a popup over a client, running a command inside it.
    ///
    /// Uses [`libtmux::Server::display_popup`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::display_popup`] for failure conditions.
    pub async fn display_popup(
        &self,
        client: Option<&Client>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.display_popup(client, command).await
    }

    /// Show a menu over a client.
    ///
    /// Uses [`libtmux::Server::display_menu`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::display_menu`] for failure conditions.
    pub async fn display_menu(
        &self,
        client: Option<&Client>,
        title: &str,
        items: impl IntoIterator<Item = MenuItem>,
    ) -> Result<(), Error> {
        self.0.display_menu(client, title, items).await
    }

    /// Open a command prompt on a client.
    ///
    /// Uses [`libtmux::Server::command_prompt`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::command_prompt`] for failure conditions.
    pub async fn command_prompt(
        &self,
        client: Option<&Client>,
        prompt: Option<&str>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.command_prompt(client, prompt, command).await
    }

    /// Open one of tmux's interactive choosers on a client.
    ///
    /// Uses [`libtmux::Server::choose`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::choose`] for failure conditions.
    pub async fn choose(&self, chooser: Chooser, client: Option<&Client>) -> Result<(), Error> {
        self.0.choose(chooser, client).await
    }

    /// Open tmux's window finder for a search string.
    ///
    /// Uses [`libtmux::Server::find_window`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::find_window`] for failure conditions.
    pub async fn find_window(&self, client: Option<&Client>, search: &str) -> Result<(), Error> {
        self.0.find_window(client, search).await
    }

    /// Briefly show each pane's number on a client.
    ///
    /// Uses [`libtmux::Server::display_panes`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::display_panes`] for failure conditions.
    pub async fn display_panes(&self, client: Option<&Client>) -> Result<(), Error> {
        self.0.display_panes(client).await
    }

    /// List key bindings as fields: table, key, command, note and repeat.
    ///
    /// Uses [`libtmux::Server::typed_key_bindings`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::typed_key_bindings`] for failure conditions.
    pub async fn typed_key_bindings(&self, table: Option<&str>) -> Result<Vec<KeyBinding>, Error> {
        self.0.typed_key_bindings(table).await
    }

    /// List the server option names.
    ///
    /// Uses [`libtmux::Server::option_names`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::option_names`] for failure conditions.
    pub async fn option_names(&self) -> Result<Vec<String>, Error> {
        self.0.option_names().await
    }

    /// Read every option set at this server, decoded by its declared kind.
    ///
    /// Uses [`libtmux::Server::options`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::options`] for failure conditions.
    pub async fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        self.0.options().await
    }

    /// Set one server option to text, unchecked.
    ///
    /// Uses [`libtmux::Server::set_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_option`] for failure conditions.
    pub async fn set_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        self.0.set_option(name, value).await
    }

    /// Set one server option to a typed value, checked before it is sent.
    ///
    /// Uses [`libtmux::Server::set_typed_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_typed_option`] for failure conditions.
    pub async fn set_typed_option(
        &self,
        name: &str,
        value: impl Into<OptionValue>,
    ) -> Result<(), Error> {
        self.0.set_typed_option(name, value).await
    }

    /// Remove one server option.
    ///
    /// Uses [`libtmux::Server::unset_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::unset_option`] for failure conditions.
    pub async fn unset_option(&self, name: &str) -> Result<(), Error> {
        self.0.unset_option(name).await
    }

    /// Set one global session option to text, unchecked.
    ///
    /// Uses [`libtmux::Server::set_global_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_global_option`] for failure conditions.
    pub async fn set_global_option(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.set_global_option(name, value).await
    }

    /// Set one global session option after checking its typed value.
    ///
    /// Uses [`libtmux::Server::set_typed_global_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_typed_global_option`] for failure conditions.
    pub async fn set_typed_global_option(
        &self,
        name: &str,
        value: impl Into<OptionValue>,
    ) -> Result<(), Error> {
        self.0.set_typed_global_option(name, value).await
    }

    /// Set a variable in the server's own environment.
    ///
    /// Uses [`libtmux::Server::set_environment`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_environment`] for failure conditions.
    pub async fn set_environment(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.set_environment(name, value).await
    }

    /// Read one variable from the server's environment.
    ///
    /// Uses [`libtmux::Server::environment`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::environment`] for failure conditions.
    pub async fn environment(&self, name: &str) -> Result<Option<EnvironmentEntry>, Error> {
        self.0.environment(name).await
    }

    /// Read the server's whole environment.
    ///
    /// Uses [`libtmux::Server::environment_all`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::environment_all`] for failure conditions.
    pub async fn environment_all(&self) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
        self.0.environment_all().await
    }

    /// Hide a variable from processes tmux starts.
    ///
    /// Uses [`libtmux::Server::hide_environment`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::hide_environment`] for failure conditions.
    pub async fn hide_environment(&self, name: &str) -> Result<(), Error> {
        self.0.hide_environment(name).await
    }

    /// Remove a variable from the server's environment.
    ///
    /// Uses [`libtmux::Server::unset_environment`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::unset_environment`] for failure conditions.
    pub async fn unset_environment(&self, name: &str) -> Result<(), Error> {
        self.0.unset_environment(name).await
    }

    /// Read every value an array option holds, by index.
    ///
    /// Uses [`libtmux::Server::array_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::array_option`] for failure conditions.
    pub async fn array_option(&self, name: &str) -> Result<SparseValues<TmuxText>, Error> {
        self.0.array_option(name).await
    }

    /// Write one index of an array option, leaving the others alone.
    ///
    /// Uses [`libtmux::Server::set_array_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_array_option`] for failure conditions.
    pub async fn set_array_option(
        &self,
        name: &str,
        index: u32,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.set_array_option(name, index, value).await
    }

    /// Extend the value already at one index of an array option.
    ///
    /// Uses [`libtmux::Server::append_array_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::append_array_option`] for failure conditions.
    pub async fn append_array_option(
        &self,
        name: &str,
        index: u32,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.append_array_option(name, index, value).await
    }

    /// Remove one index of an array option, leaving a gap where it was.
    ///
    /// Uses [`libtmux::Server::unset_array_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::unset_array_option`] for failure conditions.
    pub async fn unset_array_option(&self, name: &str, index: u32) -> Result<(), Error> {
        self.0.unset_array_option(name, index).await
    }

    /// Set one global window option to text, unchecked.
    ///
    /// Uses [`libtmux::Server::set_global_window_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_global_window_option`] for failure conditions.
    pub async fn set_global_window_option(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        self.0.set_global_window_option(name, value).await
    }

    /// Set one global window option after checking its typed value.
    ///
    /// Uses [`libtmux::Server::set_typed_global_window_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_typed_global_window_option`] for failure conditions.
    pub async fn set_typed_global_window_option(
        &self,
        name: &str,
        value: impl Into<OptionValue>,
    ) -> Result<(), Error> {
        self.0.set_typed_global_window_option(name, value).await
    }

    /// Set one global hook.
    ///
    /// Uses [`libtmux::Server::set_hook`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_hook`] for failure conditions.
    pub async fn set_hook(&self, name: &str, command: impl Into<OsString>) -> Result<(), Error> {
        self.0.set_hook(name, command).await
    }

    /// Remove one global hook.
    ///
    /// Uses [`libtmux::Server::unset_hook`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::unset_hook`] for failure conditions.
    pub async fn unset_hook(&self, name: &str) -> Result<(), Error> {
        self.0.unset_hook(name).await
    }

    /// Write a whole hook at once.
    ///
    /// Uses [`libtmux::Server::set_hooks`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::set_hooks`] for failure conditions.
    ///
    /// # Cancel safety
    ///
    /// See [`libtmux::Server::set_hooks`] for cancellation and cleanup guarantees.
    pub async fn set_hooks(
        &self,
        name: &str,
        hooks: &IndexedHooks,
        replace: ReplaceMode,
    ) -> Result<(), Error> {
        self.0.set_hooks(name, hooks, replace).await
    }

    /// Read every hook set at this server.
    ///
    /// Uses [`libtmux::Server::hooks`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::hooks`] for failure conditions.
    pub async fn hooks(&self) -> Result<BTreeMap<String, IndexedHooks>, Error> {
        self.0.hooks().await
    }

    /// Read one hook's commands, or `None` when it holds nothing.
    ///
    /// Uses [`libtmux::Server::hook`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::hook`] for failure conditions.
    pub async fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        self.0.hook(name).await
    }

    /// Read one server option, decoded according to its declared kind.
    ///
    /// Uses [`libtmux::Server::typed_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::typed_option`] for failure conditions.
    pub async fn typed_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        self.0.typed_option(name).await
    }

    /// Read one global session option, decoded according to its declared kind.
    ///
    /// Uses [`libtmux::Server::typed_global_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::typed_global_option`] for failure conditions.
    pub async fn typed_global_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        self.0.typed_global_option(name).await
    }

    /// Read one global window option, decoded according to its declared kind.
    ///
    /// Uses [`libtmux::Server::typed_global_window_option`] with this server's Tokio driver.
    ///
    /// # Errors
    ///
    /// See [`libtmux::Server::typed_global_window_option`] for failure conditions.
    pub async fn typed_global_window_option(
        &self,
        name: &str,
    ) -> Result<Option<OptionValue>, Error> {
        self.0.typed_global_window_option(name).await
    }
}
