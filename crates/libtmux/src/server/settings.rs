use std::collections::BTreeMap;
use std::ffi::OsString;

use super::Server;
use crate::formats::TmuxText;
use crate::internal::{environment, options};
use crate::{EnvironmentEntry, Error, IndexedHooks, OptionValue, ReplaceMode, SparseValues};

/// The table tmux keeps an array option in, reached from a `Server` handle.
///
/// tmux's eight array options span three tables: `command-alias` and the
/// terminal ones are server options, `status-format` and `update-environment`
/// session ones, and `pane-colours` belongs to a window and a pane both. This
/// handle offers one family of methods for all of them, so the name has to
/// pick the table. Sending one scope for every name reached the right table
/// only because tmux resolves an option by its name and ignored the flag.
///
/// For an option that is not the server's, the global table is the
/// server-wide default this handle can address; a particular session's or
/// window's copy is reached through that object's own handle.
fn array_scope(name: &str) -> options::Scope<'static> {
    match crate::option_schema(name).map(crate::OptionSchema::scopes) {
        Some(scopes) if scopes.contains(&crate::OptionScope::Server) => options::Scope::Server,
        Some(scopes) if scopes.contains(&crate::OptionScope::Window) => {
            options::Scope::GlobalWindow
        }
        _ => options::Scope::GlobalSession,
    }
}

impl Server {
    /// Read one server option's exact stored value.
    ///
    /// Returns `None` when the option is known but holds no value. tmux
    /// prints nothing in that case, so an option set to the empty string
    /// cannot be told apart from an unset one.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        options::get(&self.core, options::Scope::Server, name).await
    }

    /// List the server option names.
    ///
    /// Values are not included: tmux renders them for display with three
    /// different quoting styles, so re-parsing them would be guesswork. Read
    /// each value with [`Server::get_option`], which returns exact bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the listing.
    pub async fn option_names(&self) -> Result<Vec<String>, Error> {
        options::names(&self.core, options::Scope::Server).await
    }

    /// Read every option set at this server, decoded by its declared kind.
    ///
    /// Costs one tmux command per option, because each value is read as the
    /// bytes tmux stored rather than the form it lists them in. Use
    /// [`Self::option_names`] when only the names are wanted, and
    /// [`Self::typed_option`] for one value.
    ///
    /// An array option keeps the indexed name tmux lists it under, so
    /// `command-alias[0]` and `command-alias[1]` are separate entries.
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
    /// let server = guard.server();
    ///
    /// server.set_option("buffer-limit", "42").await?;
    /// let options = server.options().await?;
    /// assert!(options.contains_key("buffer-limit"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        options::typed_all(&self.core, options::Scope::Server).await
    }

    /// Set one server option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        options::set(&self.core, options::Scope::Server, name, value, false).await
    }

    /// Remove one server option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_option(&self, name: &str) -> Result<(), Error> {
        options::unset(&self.core, options::Scope::Server, name).await
    }

    /// Read one global session option.
    ///
    /// Sessions inherit from this table, so it is where a default belongs.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_global_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        options::get(&self.core, options::Scope::GlobalSession, name).await
    }

    /// Set one global session option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_global_option(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(
            &self.core,
            options::Scope::GlobalSession,
            name,
            value,
            false,
        )
        .await
    }

    /// Set a variable in the server's own environment.
    ///
    /// tmux keeps this and each session's environment in separate stores, and
    /// merges them only when it starts a process. So a name set here is
    /// reported as an unknown variable by
    /// [`Session::environment`](crate::Session::environment) -- reading
    /// a session does not fall back to the server -- while a pane started
    /// afterwards is handed it all the same.
    ///
    /// Where both hold a name, the session's value is the one the process
    /// gets. [`Self::hide_environment`] removes the name from the merge
    /// entirely.
    ///
    /// Panes already running keep the environment they were started with.
    ///
    /// The value is marked sensitive, since an environment carries tokens.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use libtmux::EnvironmentEntry;
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// server.set_environment("EDITOR", "hx").await?;
    /// assert!(matches!(
    ///     server.environment("EDITOR").await?,
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
    /// ));
    ///
    /// // Separate stores: the session has no entry of its own, and reading it
    /// // does not fall back to the server. The value still reaches a process
    /// // the session starts.
    /// let session = server.new_session("separate").await?;
    /// assert_eq!(session.environment("EDITOR").await?, None);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_environment(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        environment::set(&self.core, environment::Scope::Global, name, value.into()).await
    }

    /// Read one variable from the server's environment.
    ///
    /// tmux keeps two different things under a name: a value, and a mark
    /// saying a process started from it must not inherit the name at all.
    /// [`EnvironmentEntry`] keeps them apart, because collapsing both to
    /// absence would hide the second, which a caller sets deliberately with
    /// [`Self::hide_environment`].
    ///
    /// `None` means tmux holds nothing under the name.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached.
    pub async fn environment(&self, name: &str) -> Result<Option<EnvironmentEntry>, Error> {
        environment::get(&self.core, environment::Scope::Global, name).await
    }

    /// Read the server's whole environment.
    ///
    /// Costs one tmux command per variable, for the reason given on
    /// [`Session::environment_all`](crate::Session::environment_all): a value
    /// containing a newline occupies
    /// more than one line of the listing, and a continuation line holding an
    /// `=` cannot be told from the next variable.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
    /// An empty map means the server holds nothing, never that the listing
    /// failed.
    pub async fn environment_all(&self) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
        environment::all(&self.core, environment::Scope::Global).await
    }

    /// Hide a variable from processes tmux starts.
    ///
    /// Different from [`Self::unset_environment`], which deletes the server's
    /// own entry and lets whatever tmux was started with show through. This
    /// keeps an entry and marks it, so a process started afterwards is handed
    /// an environment with the name *absent* even though the tmux server was
    /// started with one. It is what [`EnvironmentEntry::Removed`] reports.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn hide_environment(&self, name: &str) -> Result<(), Error> {
        environment::hide(&self.core, environment::Scope::Global, name).await
    }

    /// Remove a variable from the server's environment.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_environment(&self, name: &str) -> Result<(), Error> {
        environment::unset(&self.core, environment::Scope::Global, name).await
    }

    /// Read every value an array option holds, by index.
    ///
    /// Some tmux options hold a numbered set rather than one value:
    /// `command-alias` and `terminal-overrides` are the common ones, and every
    /// hook is one too. The indices are sparse and tmux keeps the gaps, so
    /// this reports them rather than a list. An empty result means the option
    /// holds nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// // Written far apart on purpose: nothing renumbers, so the gap stays.
    /// server.set_array_option("command-alias", 30, "thirty=display -p 30").await?;
    /// server.set_array_option("command-alias", 35, "five=display -p 35").await?;
    ///
    /// let aliases = server.array_option("command-alias").await?;
    /// assert_eq!(aliases.get(31), None, "the gap is tmux's, and it is kept");
    /// assert_eq!(
    ///     aliases.get(35).map(|value| value.to_string_lossy().into_owned()),
    ///     Some("five=display -p 35".to_owned()),
    /// );
    ///
    /// server.unset_array_option("command-alias", 35).await?;
    /// assert_eq!(server.array_option("command-alias").await?.get(35), None);
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn array_option(&self, name: &str) -> Result<SparseValues<TmuxText>, Error> {
        Ok(SparseValues::from(
            options::indexed(&self.core, array_scope(name), name).await?,
        ))
    }

    /// Write one index of an array option, leaving the others alone.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the option name or the value.
    pub async fn set_array_option(
        &self,
        name: &str,
        index: u32,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(
            &self.core,
            array_scope(name),
            &format!("{name}[{index}]"),
            value,
            false,
        )
        .await
    }

    /// Extend the value already at one index of an array option.
    ///
    /// Appends to that index's value rather than adding an entry, which is
    /// what tmux's `-a` does here.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the option name or the value.
    pub async fn append_array_option(
        &self,
        name: &str,
        index: u32,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(
            &self.core,
            array_scope(name),
            &format!("{name}[{index}]"),
            value,
            true,
        )
        .await
    }

    /// Remove one index of an array option, leaving a gap where it was.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux refuses the option name.
    pub async fn unset_array_option(&self, name: &str, index: u32) -> Result<(), Error> {
        options::unset(&self.core, array_scope(name), &format!("{name}[{index}]")).await
    }

    /// Read one global window option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn get_global_window_option(&self, name: &str) -> Result<Option<TmuxText>, Error> {
        options::get(&self.core, options::Scope::GlobalWindow, name).await
    }

    /// Set one global window option.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_global_window_option(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        options::set(&self.core, options::Scope::GlobalWindow, name, value, false).await
    }

    /// Set one global hook.
    ///
    /// Hooks live in the option tables, so [`Server::get_global_option`] reads
    /// one back under an indexed name such as `after-new-window[0]`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name or command.
    pub async fn set_hook(&self, name: &str, command: impl Into<OsString>) -> Result<(), Error> {
        options::set_hook(&self.core, options::Scope::GlobalSession, name, command).await
    }

    /// Remove one global hook.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name.
    pub async fn unset_hook(&self, name: &str) -> Result<(), Error> {
        options::unset_hook(&self.core, options::Scope::GlobalSession, name).await
    }

    /// Write a whole hook at once.
    ///
    /// [`ReplaceMode::Replace`] clears the hook first, so only what is
    /// written remains; [`ReplaceMode::Merge`] leaves entries at indices the
    /// write does not name.
    ///
    /// Sent as one tmux invocation rather than one per index. That costs one
    /// process instead of several, but it is not atomic: tmux applies a
    /// shared invocation in order and stops at the first refusal, so a
    /// rejected entry leaves the ones before it written.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or any command.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// use std::collections::BTreeMap;
    /// use libtmux::{IndexedHooks, ReplaceMode, TmuxText};
    ///
    /// let guard = libtmux::test::TestServer::new().await?;
    /// let server = guard.server();
    ///
    /// let mut entries = BTreeMap::new();
    /// entries.insert(0, TmuxText::from(b"display-message first".to_vec()));
    /// entries.insert(3, TmuxText::from(b"display-message fourth".to_vec()));
    ///
    /// server
    ///     .set_hooks("alert-bell", &IndexedHooks::from(entries), ReplaceMode::Replace)
    ///     .await?;
    ///
    /// let written = server.hook("alert-bell").await?.expect("the hook is set");
    /// assert_eq!(written.len(), 2);
    /// assert!(written.get(1).is_none(), "the gap is kept");
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_hooks(
        &self,
        name: &str,
        hooks: &IndexedHooks,
        replace: ReplaceMode,
    ) -> Result<(), Error> {
        options::set_hooks(
            &self.core,
            options::Scope::GlobalSession,
            name,
            hooks,
            replace,
        )
        .await
    }

    /// Read every hook set at this server.
    ///
    /// Only hooks holding something are reported: tmux lists every hook name
    /// it knows, and the ones holding nothing are absent here rather than
    /// present and empty.
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
    /// let server = guard.server();
    ///
    /// server.set_hook("alert-bell", "display-message rang").await?;
    /// let hooks = server.hooks().await?;
    /// assert!(hooks.contains_key("alert-bell"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hooks(&self) -> Result<BTreeMap<String, IndexedHooks>, Error> {
        options::hooks(&self.core, options::Scope::GlobalSession).await
    }

    /// Read one hook's commands, or `None` when it holds nothing.
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
    /// let server = guard.server();
    ///
    /// assert!(server.hook("alert-bell").await?.is_none());
    /// server.set_hook("alert-bell", "display-message rang").await?;
    /// assert!(server.hook("alert-bell").await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        options::hook(&self.core, options::Scope::GlobalSession, name).await
    }

    /// Read one server option, decoded according to its declared kind.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn typed_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        Ok(options::get(&self.core, options::Scope::Server, name)
            .await?
            .map(|value| OptionValue::decode(name, value)))
    }

    /// Read one global session option, decoded according to its declared kind.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn typed_global_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        Ok(
            options::get(&self.core, options::Scope::GlobalSession, name)
                .await?
                .map(|value| OptionValue::decode(name, value)),
        )
    }
}
