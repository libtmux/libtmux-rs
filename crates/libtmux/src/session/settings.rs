//! Session option, hook, and environment commands.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::internal::environment;
use crate::internal::options;
use crate::{EnvironmentEntry, Error, IndexedHooks, OptionValue, ReplaceMode};

use super::Session;

impl Session {
    /// List the option names set at this session's scope.
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
    pub fn option_names(&self) -> Result<Vec<String>, Error> {
        let target = self.id().to_string();
        options::names(&self.core, options::Scope::Session(&target))
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
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("opts")?;
    ///
    /// session.set_option("status-left-length", "30")?;
    /// let options = session.options()?;
    /// assert!(options.contains_key("status-left-length"));
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn options(&self) -> Result<BTreeMap<String, OptionValue>, Error> {
        let target = self.id().to_string();
        options::typed_all(&self.core, options::Scope::Session(&target))
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
    pub fn set_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(
            &self.core,
            options::Scope::Session(&target),
            name,
            value,
            false,
        )
    }

    /// Set one option to a typed value, checked before it is sent.
    ///
    /// The value must be the variant [`Self::typed_option`] reads back for
    /// the option: `true` for `mouse`, a number for `history-limit`, and
    /// `"vi"` for the choice `status-keys`. [`OptionValue`] gives the rules,
    /// and the two writes it cannot check. The value is marked sensitive, as
    /// in [`Self::set_option`].
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::OptionValueRefused`] without sending anything
    /// when tmux's option table refuses the value: the wrong variant, a word
    /// outside a choice's set, or a number outside its range.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables, and an error when tmux rejects the
    /// name or value.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use libtmux::{Error, OptionValue};
    ///
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("typed")?;
    ///
    /// session.set_typed_option("history-limit", 5000)?;
    /// assert_eq!(
    ///     session.typed_option("history-limit")?,
    ///     Some(OptionValue::Number(5000)),
    /// );
    ///
    /// // Below the range tmux's table declares, so it is never sent.
    /// let refused = session.set_typed_option("history-limit", -1);
    /// assert!(matches!(refused, Err(Error::OptionValueRefused { .. })));
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_typed_option(&self, name: &str, value: impl Into<OptionValue>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set_typed(
            &self.core,
            options::Scope::Session(&target),
            name,
            value.into(),
        )
    }

    /// Append to one option rather than replacing it.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or value.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub fn append_option(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set(
            &self.core,
            options::Scope::Session(&target),
            name,
            value,
            true,
        )
    }

    /// Remove one option, restoring whatever it inherits.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub fn unset_option(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset(&self.core, options::Scope::Session(&target), name)
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
    pub fn set_hook(&self, name: &str, command: impl Into<OsString>) -> Result<(), Error> {
        let target = self.id().to_string();
        options::set_hook(&self.core, options::Scope::Session(&target), name, command)
    }

    /// Remove one hook.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the hook name.
    ///
    /// Returns [`crate::Error::OptionScopeMismatch`] when tmux keeps the
    /// option in another of its tables.
    pub fn unset_hook(&self, name: &str) -> Result<(), Error> {
        let target = self.id().to_string();
        options::unset_hook(&self.core, options::Scope::Session(&target), name)
    }

    /// Write a whole hook at once.
    ///
    /// [`ReplaceMode::Replace`] clears the hook first, so only what is
    /// written remains; [`ReplaceMode::Merge`] leaves entries at indices the
    /// write does not name.
    ///
    /// Sent as at most two tmux invocations rather than one per index: the
    /// first command alone, then the rest together. That is not atomic: tmux
    /// applies a shared invocation in order and stops at the first refusal,
    /// so a rejected entry leaves the ones before it written.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name or any command.
    ///
    /// # Cancellation
    ///
    /// Under [`ReplaceMode::Replace`] the clear and the entries are one
    /// invocation, so a drop leaves the hook as it was or fully written,
    /// never cleared and unwritten; tmux reports one status for that group,
    /// so a refusal there cannot name which command drew it. Under
    /// [`ReplaceMode::Merge`] the first entry is sent on its own, so a drop
    /// after it leaves that entry written and the rest not.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::collections::BTreeMap;
    /// use libtmux::{IndexedHooks, ReplaceMode, TmuxText};
    ///
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("hooked")?;
    ///
    /// let mut entries = BTreeMap::new();
    /// entries.insert(0, TmuxText::from(b"display-message first".to_vec()));
    /// entries.insert(3, TmuxText::from(b"display-message fourth".to_vec()));
    ///
    /// session
    ///     .set_hooks("alert-bell", &IndexedHooks::from(entries), ReplaceMode::Replace)
    ///     ?;
    ///
    /// let written = session.hook("alert-bell")?.expect("the hook is set");
    /// assert_eq!(written.len(), 2);
    /// assert!(written.get(1).is_none(), "the gap is kept");
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn set_hooks(
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
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("hooked")?;
    ///
    /// session.set_hook("alert-bell", "display-message rang")?;
    /// let hooks = session.hooks()?;
    /// assert!(hooks.contains_key("alert-bell"));
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn hooks(&self) -> Result<BTreeMap<String, IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hooks(&self.core, options::Scope::Session(&target))
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
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("hooked")?;
    ///
    /// assert!(session.hook("alert-bell")?.is_none());
    /// session.set_hook("alert-bell", "display-message rang")?;
    /// assert!(session.hook("alert-bell")?.is_some());
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn hook(&self, name: &str) -> Result<Option<IndexedHooks>, Error> {
        let target = self.id().to_string();
        options::hook(&self.core, options::Scope::Session(&target), name)
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
    pub fn set_environment(&self, name: &str, value: impl Into<OsString>) -> Result<(), Error> {
        environment::set(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
            value.into(),
        )
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
    /// use libtmux::EnvironmentEntry;
    ///
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("read")?;
    ///
    /// assert_eq!(session.environment("EDITOR")?, None);
    ///
    /// session.set_environment("EDITOR", "hx")?;
    /// assert!(matches!(
    ///     session.environment("EDITOR")?,
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"hx",
    /// ));
    ///
    /// session.hide_environment("EDITOR")?;
    /// assert_eq!(
    ///     session.environment("EDITOR")?,
    ///     Some(EnvironmentEntry::Removed),
    /// );
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn environment(&self, name: &str) -> Result<Option<EnvironmentEntry>, Error> {
        environment::get(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
    }

    /// Read the session's whole environment.
    ///
    /// tmux distinguishes a variable it holds a value for from one it has
    /// marked for *removal*, so that a process started in the session does not
    /// inherit it. Both appear in the listing, and [`EnvironmentEntry`] keeps
    /// them apart, exactly as [`Self::environment`] does for a single name.
    ///
    /// Costs one tmux command. The listing is read in tmux's shell form,
    /// which escapes each value, so a value containing a newline or an `=`
    /// is not mistaken for the next variable. Each value reads exactly as
    /// [`Self::environment`] reads it. Names are not escaped: a removed name
    /// holding `;` and a newline lists exactly as two removed names do, and
    /// reads as them.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be reached or refuses the listing,
    /// and [`Error::DecodeListing`] when the listing is not in the shape tmux
    /// prints. An empty map means the session holds nothing, never that the
    /// listing failed.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use libtmux::EnvironmentEntry;
    ///
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("env")?;
    ///
    /// session.set_environment("EDITOR", "vi")?;
    /// session.hide_environment("PAGER")?;
    ///
    /// let environment = session.environment_all()?;
    /// assert!(matches!(
    ///     environment.get("EDITOR"),
    ///     Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"vi",
    /// ));
    /// assert_eq!(environment.get("PAGER"), Some(&EnvironmentEntry::Removed));
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn environment_all(&self) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
        environment::all(&self.core, environment::Scope::Session(self.id().as_ref()))
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
    /// use libtmux::EnvironmentEntry;
    ///
    /// let guard = libtmux::test::TestServer::new()?;
    /// let session = guard.server().new_session("hidden")?;
    ///
    /// session.hide_environment("PAGER")?;
    /// assert_eq!(
    ///     session.environment_all()?.get("PAGER"),
    ///     Some(&EnvironmentEntry::Removed),
    /// );
    ///
    /// guard.shutdown()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn hide_environment(&self, name: &str) -> Result<(), Error> {
        environment::hide(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
    }

    /// Remove an environment variable from the session.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux rejects the name.
    pub fn unset_environment(&self, name: &str) -> Result<(), Error> {
        environment::unset(
            &self.core,
            environment::Scope::Session(self.id().as_ref()),
            name,
        )
    }

    /// Read one option, decoded according to what tmux declares about it.
    ///
    /// A flag comes back as [`OptionValue::Flag`] and a number as
    /// [`OptionValue::Number`], so a caller does not decide for itself that
    /// `on` means one. Everything else, including user options, stays text.
    /// The one way to read a single option; [`OptionValue`] says how reads
    /// and writes fit together.
    ///
    /// `None` means the option holds nothing at this session. A user option,
    /// whose name begins with `@`, exists only while it is set, and a built-in
    /// one always exists, so both report an unset option as `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux does not recognize the option name.
    pub fn typed_option(&self, name: &str) -> Result<Option<OptionValue>, Error> {
        let target = self.id().to_string();
        options::get_typed(&self.core, options::Scope::Session(&target), name)
    }
}
