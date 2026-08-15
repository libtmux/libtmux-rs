//! Reading the hooks tmux is holding.
//!
//! A tmux hook is an array option, so one hook name holds a numbered set of
//! commands rather than a single command. The numbers are not a formality:
//! removing the middle of a set leaves a gap, and tmux keeps it, so
//! [`IndexedHooks`] is a sparse map rather than a list. A caller that only
//! wants "the command" asks for [`SparseValues::first`].
//!
//! Hooks are not the only array option -- `command-alias` and
//! `terminal-overrides` are the same shape -- so the container is
//! [`SparseValues`] and [`IndexedHooks`] names the hook-shaped one.

use std::collections::BTreeMap;
use std::collections::btree_map::{Iter, Keys, Values};

use crate::formats::TmuxText;

/// The values one array option holds, by the index tmux stores them under.
///
/// Indices are sparse, and tmux means it. Setting a value without an index
/// writes slot `0`, removing one slot of several leaves the rest where they
/// were, and nothing renumbers, so a `SparseValues` with two entries may hold
/// them at `0` and `30`. Iteration is by ascending index.
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
pub struct SparseValues<T> {
    entries: BTreeMap<u32, T>,
}

/// The commands one hook name holds.
///
/// A hook is an array option whose values are tmux commands, so this is the
/// [`SparseValues`] of that shape rather than a type of its own.
pub type IndexedHooks = SparseValues<TmuxText>;

impl<T> SparseValues<T> {
    pub(crate) fn from_entries(entries: BTreeMap<u32, T>) -> Self {
        Self { entries }
    }

    /// The value stored at one index.
    #[must_use]
    pub fn get(&self, index: u32) -> Option<&T> {
        self.entries.get(&index)
    }

    /// The value at the lowest index, which is what a write without one sets.
    ///
    /// The lowest index rather than index `0`, because a set whose slot `0`
    /// was removed still has a first value.
    #[must_use]
    pub fn first(&self) -> Option<&T> {
        self.entries.values().next()
    }

    /// How many values this option holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this option holds nothing.
    ///
    /// One read back from tmux never does: a name holding nothing is reported
    /// as absent rather than as an empty set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The values and the indices they are stored under, lowest first.
    pub fn iter(&self) -> Iter<'_, u32, T> {
        self.entries.iter()
    }

    /// The indices that hold a value, ascending.
    ///
    /// Worth reading rather than assuming: the gaps are tmux's, and writing
    /// to `len()` would overwrite an entry whenever there is one.
    pub fn indices(&self) -> Keys<'_, u32, T> {
        self.entries.keys()
    }

    /// The values alone, by ascending index.
    pub fn values(&self) -> Values<'_, u32, T> {
        self.entries.values()
    }

    /// The values alone, owned, by ascending index.
    ///
    /// The list form, for a caller that has no use for the indices. It is
    /// lossy about the gaps, which is why it is not how the values are held.
    #[must_use]
    pub fn into_values(self) -> Vec<T> {
        self.entries.into_values().collect()
    }
}

impl<T> From<BTreeMap<u32, T>> for SparseValues<T> {
    fn from(entries: BTreeMap<u32, T>) -> Self {
        Self::from_entries(entries)
    }
}

impl<'values, T> IntoIterator for &'values SparseValues<T> {
    type Item = (&'values u32, &'values T);
    type IntoIter = Iter<'values, u32, T>;

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
