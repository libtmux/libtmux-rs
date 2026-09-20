//! Pane option and hook commands.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::internal::options;
use crate::{Error, IndexedHooks, OptionValue};

use super::Pane;

impl Pane {
    /// List the option names set at this pane's scope.
    ///
    /// Values are not included: tmux renders them for display with three
    /// different quoting styles, so re-parsing them would be guesswork. Read
    /// each value with [`Self::typed_option`], which decodes the exact bytes.
    ///
    /// Names are `String`, not [`TmuxText`](crate::TmuxText), because tmux's
    /// own are ASCII and every option method takes one as `&str`; a user
    /// option (`@name`) lists cut at any whitespace in its name, and with
    /// U+FFFD for bytes that are not UTF-8, since `show-options` prints names
    /// unframed.
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

    /// Set one option to text, unchecked.
    ///
    /// The value is marked sensitive, so it never reaches `Debug`, an error,
    /// or a tracing span. tmux validates it; [`Self::set_typed_option`]
    /// checks it against tmux's option table first.
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
            options::Scope::Pane(&target),
            name,
            value,
            false,
        )
        .await
    }

    /// Set one option to a typed value, checked before it is sent.
    ///
    /// The value must be the variant [`Self::typed_option`] reads back for
    /// the option; [`OptionValue`] gives the rules, and the two writes it
    /// cannot check. The value is marked sensitive, as in
    /// [`Self::set_option`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::OptionValueRefused`] without sending anything
    /// when tmux's option table refuses the value.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables, and an error when tmux rejects the
    /// name or value.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # async fn example(pane: &libtmux::Pane) -> Result<(), libtmux::Error> {
    /// // A choice takes the word it reads back as, not a flag.
    /// pane.set_typed_option("remain-on-exit", "failed").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn set_typed_option(
        &self,
        name: &str,
        value: impl Into<OptionValue>,
    ) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set_typed(
            &self.core,
            options::Scope::Pane(&target),
            name,
            value.into(),
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
        options::set(&self.core, options::Scope::Pane(&target), name, value, true).await
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
        options::unset(&self.core, options::Scope::Pane(&target), name).await
    }

    /// Set one hook to a tmux command.
    ///
    /// Hooks live in the same option tables, so a hook is an array option and
    /// [`Self::typed_option`] reads it under an indexed name such as
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
        options::set_hook(&self.core, options::Scope::Pane(&target), name, command).await
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
        options::unset_hook(&self.core, options::Scope::Pane(&target), name).await
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
    /// assert!(pane.hook("pane-died").await?.is_none());
    /// pane.set_hook("pane-died", "display-message rang").await?;
    /// assert!(pane.hook("pane-died").await?.is_some());
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

    /// Read one option, decoded according to what tmux declares about it.
    ///
    /// A flag comes back as [`OptionValue::Flag`] and a number as
    /// [`OptionValue::Number`], so a caller does not decide for itself that
    /// `on` means one. Everything else, including user options, stays text.
    /// The one way to read a single option; [`OptionValue`] says how reads
    /// and writes fit together.
    ///
    /// `None` means the option holds nothing at this pane. A user option,
    /// whose name begins with `@`, exists only while it is set, and a built-in
    /// one always exists, so both report an unset option as `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub async fn typed_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        let target = self.id().to_string();
        options::get_typed(&self.core, options::Scope::Pane(&target), name).await
    }
}
