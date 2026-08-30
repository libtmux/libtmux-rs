//! Session option, hook, and environment commands.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::formats::TmuxText;
use crate::internal::environment;
use crate::internal::options;
use crate::{EnvironmentEntry, Error, IndexedHooks, OptionValue, ReplaceMode};

use super::Session;

impl Session {
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
        options::get(&self.core, options::Scope::Session(&target), name).await
    }

    /// List the option names set at this session's scope.
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
        options::names(&self.core, options::Scope::Session(&target)).await
    }

    /// Read every option set at this session, decoded by its declared kind.
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
    /// let session = guard.server().new_session("opts").await?;
    ///
    /// session.set_option("status-left-length", "30").await?;
    /// let options = session.options().await?;
    /// assert!(options.contains_key("status-left-length"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        let target = self.id().to_string();
        options::typed_all(&self.core, options::Scope::Session(&target)).await
    }

    /// Set one option.
    ///
    /// The value is marked sensitive, so it never reaches `Debug`, an error,
    /// or a tracing span.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub async fn set_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(
            &self.core,
            options::Scope::Session(&target),
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
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub async fn append_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(
            &self.core,
            options::Scope::Session(&target),
            name,
            value,
            true,
        )
        .await
    }

    /// Remove one option, restoring whatever it inherits.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub async fn unset_option(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset(&self.core, options::Scope::Session(&target), name).await
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
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub async fn set_hook(&self, name: &str, command: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set_hook(&self.core, options::Scope::Session(&target), name, command).await
    }

    /// Remove one hook.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub async fn unset_hook(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset_hook(&self.core, options::Scope::Session(&target), name).await
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
    /// let session = guard.server().new_session("hooked").await?;
    ///
    /// let mut entries = BTreeMap::new();
    /// entries.insert(0, TmuxText::from(b"display-message first".to_vec()));
    /// entries.insert(3, TmuxText::from(b"display-message fourth".to_vec()));
    ///
    /// session
    ///     .set_hooks("alert-bell", &IndexedHooks::from(entries), ReplaceMode::Replace)
    ///     .await?;
    ///
    /// let written = session.hook("alert-bell").await?.expect("the hook is set");
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
        let target = self.id().to_string();
        options::set_hooks(
            &self.core,
            options::Scope::Session(&target),
            name,
            hooks,
            replace,
        )
        .await
    }

    /// Read every hook set at this session.
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
    /// let session = guard.server().new_session("hooked").await?;
    ///
    /// session.set_hook("alert-bell", "display-message rang").await?;
    /// let hooks = session.hooks().await?;
    /// assert!(hooks.contains_key("alert-bell"));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hooks(&self) -> Result<BTreeMap<String, IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hooks(&self.core, options::Scope::Session(&target)).await
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
    /// let session = guard.server().new_session("hooked").await?;
    ///
    /// assert!(session.hook("alert-bell").await?.is_none());
    /// session.set_hook("alert-bell", "display-message rang").await?;
    /// assert!(session.hook("alert-bell").await?.is_some());
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hook(&self.core, options::Scope::Session(&target), name).await
    }

    /// Set an environment variable for processes this session starts.
    ///
    /// Existing panes keep the environment they were started with; this
    /// affects what new panes inherit.
    ///
    /// The value is marked sensitive, since an environment carries tokens.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    pub async fn set_environment(
        &self,
        name: &str,
        value: impl Into<OsString>,
    ) -> Result<(), Error> {
        environment::set(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
            value.into(),
        )
        .await
    }

    /// Read one variable from the session's environment.
    ///
    /// tmux keeps two different things under a name: a value, and a mark
    /// saying a process started here must not inherit the name at all.
    /// [`EnvironmentEntry`] keeps them apart, because collapsing both to
    /// absence would hide the second, which a caller sets deliberately with
    /// [`Self::hide_environment`].
    ///
    /// `None` means tmux holds nothing under the name.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached.
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
    /// let session = guard.server().new_session("read").await?;
    ///
    /// assert_eq!(session.environment("EDITOR").await?, None);
    ///
    /// session.set_environment("EDITOR", "hx").await?;
    /// assert!(matches!(
    ///     session.environment("EDITOR").await?,
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
    /// ));
    ///
    /// session.hide_environment("EDITOR").await?;
    /// assert_eq!(
    ///     session.environment("EDITOR").await?,
    ///     Some(EnvironmentEntry::Removed),
    /// );
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn environment(&self, name: &str) -> Result<Option<EnvironmentEntry>, Error> {
        environment::get(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
        .await
    }

    /// Read the session's whole environment.
    ///
    /// tmux distinguishes a variable it holds a value for from one it has
    /// marked for *removal*, so that a process started in the session does not
    /// inherit it. Both appear in the listing, and [`EnvironmentEntry`] keeps
    /// them apart, exactly as [`Self::environment`] does for a single name.
    ///
    /// Costs one tmux command per variable. The listing alone cannot be
    /// trusted: a value containing a newline occupies more than one line, and
    /// a continuation line holding an `=` is indistinguishable from the next
    /// variable. Each name is therefore read back on its own, which also
    /// discards the continuation lines, because tmux refuses a name it does
    /// not hold.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing.
    /// An empty map means the session holds nothing, never that the listing
    /// failed.
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
    /// let session = guard.server().new_session("env").await?;
    ///
    /// session.set_environment("EDITOR", "vi").await?;
    /// session.hide_environment("PAGER").await?;
    ///
    /// let environment = session.environment_all().await?;
    /// assert!(matches!(
    ///     environment.get("EDITOR"),
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"vi",
    /// ));
    /// assert_eq!(environment.get("PAGER"), Some(&EnvironmentEntry::Removed));
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn environment_all(&self) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
        environment::all(&self.core, environment::Scope::Session(self.id().as_ref())).await
    }

    /// Hide a variable from processes started in this session.
    ///
    /// Different from [`Self::unset_environment`], which deletes the session's
    /// own entry and lets whatever tmux inherited show through. This keeps an
    /// entry and marks it, so a process started here is handed an environment
    /// with the name *absent* even though the tmux server has one. It is what
    /// [`EnvironmentEntry::Removed`] reports.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
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
    /// let session = guard.server().new_session("hidden").await?;
    ///
    /// session.hide_environment("PAGER").await?;
    /// assert_eq!(
    ///     session.environment_all().await?.get("PAGER"),
    ///     Some(&EnvironmentEntry::Removed),
    /// );
    ///
    /// guard.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn hide_environment(&self, name: &str) -> Result<(), Error> {
        environment::hide(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
        .await
    }

    /// Remove an environment variable from the session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub async fn unset_environment(&self, name: &str) -> Result<(), Error> {
        environment::unset(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
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
            options::get(&self.core, options::Scope::Session(&target), name)
                .await?
                .map(|value| OptionValue::decode(name, value)),
        )
    }
}
