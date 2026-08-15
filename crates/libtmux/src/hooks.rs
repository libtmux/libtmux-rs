//! Reading the hooks tmux is holding.
//!
//! A tmux hook is an array option, so one hook name holds a numbered set of
//! commands rather than a single command. The numbers are not a formality:
//! removing the middle of a set leaves a gap, and tmux keeps it, so
//! [`IndexedHooks`] is a sparse map rather than a list. A caller that only
//! wants "the command" asks for [`IndexedHooks::first`].

use std::collections::BTreeMap;
use std::collections::btree_map::Iter;

use crate::formats::TmuxText;

/// The commands one hook name holds, by the index tmux stores them under.
///
/// Indices are sparse. Setting a hook without an index writes slot `0`, and
/// removing one slot of several leaves the rest where they were, so an
/// [`IndexedHooks`] with two entries may hold them at `0` and `3`.
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
/// server.set_hook("after-new-window", "display-message built").await?;
/// let hooks = server.hook("after-new-window").await?.expect("the hook is set");
///
/// assert_eq!(hooks.len(), 1);
/// assert_eq!(
///     hooks.first().map(|value| value.to_string_lossy().into_owned()),
///     Some("display-message built".to_owned()),
/// );
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexedHooks {
    entries: BTreeMap<u32, TmuxText>,
}

impl IndexedHooks {
    pub(crate) fn from_entries(entries: BTreeMap<u32, TmuxText>) -> Self {
        Self { entries }
    }

    /// The command stored at one index.
    #[must_use]
    pub fn get(&self, index: u32) -> Option<&TmuxText> {
        self.entries.get(&index)
    }

    /// The command at the lowest index, which is what a hook set without one
    /// holds.
    #[must_use]
    pub fn first(&self) -> Option<&TmuxText> {
        self.entries.values().next()
    }

    /// How many commands this hook holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this hook holds nothing.
    ///
    /// A hook read back from tmux never does: a name holding nothing is
    /// reported as absent rather than as an empty set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The commands and the indices they are stored under, lowest first.
    pub fn iter(&self) -> Iter<'_, u32, TmuxText> {
        self.entries.iter()
    }
}

impl From<BTreeMap<u32, TmuxText>> for IndexedHooks {
    fn from(entries: BTreeMap<u32, TmuxText>) -> Self {
        Self::from_entries(entries)
    }
}

impl<'hooks> IntoIterator for &'hooks IndexedHooks {
    type Item = (&'hooks u32, &'hooks TmuxText);
    type IntoIter = Iter<'hooks, u32, TmuxText>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Whether writing a hook keeps the entries it does not name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceMode {
    /// Clear the hook first, so only the entries written remain.
    Replace,
    /// Leave entries at indices the write does not name.
    Merge,
}
