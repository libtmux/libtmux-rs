//! Reading and writing tmux options and hooks.
//!
//! Options are read one at a time through `show-options -v`, which prints the
//! stored bytes verbatim. The listing form is not used for values: tmux
//! renders them with `args_escape`, which picks bare-with-backslashes, double
//! quotes, or single quotes depending on content, so re-parsing it would be
//! guesswork. Names are read from the listing because a name is plain ASCII.
//!
//! Hooks live in the same option tables in supported tmux releases, so they
//! share this path. A hook is an array option, which is why its name carries
//! an index.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::formats::TmuxText;
use crate::hooks::IndexedHooks;
use crate::hooks::ReplaceMode;
use crate::internal::core::Core;
use crate::options::OptionValue;
use crate::{Command, CommandChain, Error};

/// Which option table an operation reads or writes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Scope<'target> {
    /// Server options, tmux's `-s`.
    Server,
    /// Global session options, tmux's `-g`.
    GlobalSession,
    /// Global window options, tmux's `-w -g`.
    GlobalWindow,
    /// One session's options.
    Session(&'target str),
    /// One window's options, tmux's `-w`.
    Window(&'target str),
    /// One pane's options, tmux's `-p`.
    Pane(&'target str),
}

impl Scope<'_> {
    /// Apply this scope's flags to an option command.
    fn apply(self, command: Command) -> Command {
        match self {
            Self::Server => command.arg("-s"),
            Self::GlobalSession => command.arg("-g"),
            Self::GlobalWindow => command.arg("-w").arg("-g"),
            Self::Session(target) => command.arg("-t").arg(OsString::from(target)),
            Self::Window(target) => command.arg("-w").arg("-t").arg(OsString::from(target)),
            Self::Pane(target) => command.arg("-p").arg("-t").arg(OsString::from(target)),
        }
    }
}

/// Read one option's exact stored value.
///
/// Absence is reported two different ways because tmux stores two kinds of
/// option. A built-in option always exists, so an unset one prints nothing and
/// exits zero. A user option, whose name begins with `@`, exists only while it
/// is set, so an unset one is simply unknown and tmux fails. Both become
/// `None`; the name shape decides which rule applies, so no error text is
/// parsed.
///
/// An option deliberately set to the empty string cannot be told apart from an
/// unset one, because tmux prints nothing for either.
pub(crate) async fn get(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
) -> Result<Option<TmuxText>, Error> {
    let command = scope
        .apply(Command::new("show-options"))
        .arg("-v")
        .arg(OsString::from(name));
    let result = core.execute(command).await?;

    if !result.success() {
        let failure = Error::refused(
            "show-options",
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            None,
        );

        // A user option that is not set is not merely empty, it is unknown to
        // tmux, so that one failure is the answer `None`.
        //
        // Only that one. The earlier version asked what the caller had named
        // and swallowed every failure for an `@` name, which made a pane that
        // had gone away read as a pane whose option was never set -- tmux says
        // "invalid option: @x" for the first and "no such pane: %1" for the
        // second, in the stderr this already holds.
        if name.starts_with('@')
            && matches!(
                failure,
                Error::OptionRejected {
                    kind: crate::OptionErrorKind::Unknown,
                    ..
                }
            )
        {
            return Ok(None);
        }

        return Err(failure);
    }

    let stdout = result.stdout();
    let value = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    if value.is_empty() {
        return Ok(None);
    }

    Ok(Some(TmuxText::from(value.to_vec())))
}

/// List the option names present at one scope.
///
/// Array options repeat once per index, so a name may carry an `[n]` suffix
/// exactly as tmux writes it.
pub(crate) async fn names(core: &Core, scope: Scope<'_>) -> Result<Vec<String>, Error> {
    let result = core
        .execute(scope.apply(Command::new("show-options")))
        .await?;
    if !result.success() {
        return Err(Error::refused(
            "show-options",
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            None,
        ));
    }

    Ok(result
        .stdout_lossy()
        .lines()
        // Only the name is taken. The rest of the line is tmux's display form,
        // which this module deliberately never re-parses.
        .filter_map(|line| line.split_whitespace().next())
        .map(ToOwned::to_owned)
        .collect())
}

/// Set one option to an exact value.
pub(crate) async fn set(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
    value: impl Into<OsString>,
    append: bool,
) -> Result<(), Error> {
    let mut command = scope.apply(Command::new("set-option"));
    if append {
        command = command.arg("-a");
    }

    run(
        core,
        command
            .arg(OsString::from(name))
            .sensitive_arg(value.into()),
    )
    .await
}

/// Remove one option, restoring whatever it inherits.
pub(crate) async fn unset(core: &Core, scope: Scope<'_>, name: &str) -> Result<(), Error> {
    run(
        core,
        scope
            .apply(Command::new("set-option"))
            .arg("-u")
            .arg(OsString::from(name)),
    )
    .await
}

/// Set one hook to a tmux command.
pub(crate) async fn set_hook(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
    command_text: impl Into<OsString>,
) -> Result<(), Error> {
    run(
        core,
        scope
            .apply(Command::new("set-hook"))
            .arg(OsString::from(name))
            .sensitive_arg(command_text.into()),
    )
    .await
}

/// Remove one hook.
pub(crate) async fn unset_hook(core: &Core, scope: Scope<'_>, name: &str) -> Result<(), Error> {
    run(
        core,
        scope
            .apply(Command::new("set-hook"))
            .arg("-u")
            .arg(OsString::from(name)),
    )
    .await
}

/// Run an option mutation, requiring tmux to accept it.
async fn run(core: &Core, command: Command) -> Result<(), Error> {
    let result = core.execute(command).await?;
    if result.success() {
        return Ok(());
    }

    Err(Error::refused(
        "set-option",
        result.exit_code(),
        result.stderr_lossy().into_owned(),
        None,
    ))
}

/// List the hook slots that are set at one scope, as `name[index]`.
///
/// `show-hooks` prints every hook tmux knows, most of them bare because they
/// hold nothing. A slot that holds something is the one carrying an index, so
/// that is the whole test: no value is read here, for the reason this module
/// gives above.
pub(crate) async fn hook_slots(core: &Core, scope: Scope<'_>) -> Result<Vec<String>, Error> {
    let result = core
        .execute(scope.apply(Command::new("show-hooks")))
        .await?;
    if !result.success() {
        return Err(Error::refused(
            "show-hooks",
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            None,
        ));
    }

    Ok(result
        .stdout_lossy()
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|slot| slot.contains('['))
        .map(ToOwned::to_owned)
        .collect())
}

/// Split a `name[index]` slot into its parts.
pub(crate) fn split_slot(slot: &str) -> Option<(&str, u32)> {
    let (name, rest) = slot.split_once('[')?;
    let index = rest.strip_suffix(']')?.parse().ok()?;
    Some((name, index))
}

/// Read every hook that is set at one scope.
///
/// Names come from the listing and values from `show-options -v`, one slot at
/// a time, for the reason this module gives above: the listing renders a value
/// through `args_escape` and re-parsing that would be guesswork.
pub(crate) async fn hooks(
    core: &Core,
    scope: Scope<'_>,
) -> Result<BTreeMap<String, IndexedHooks>, Error> {
    let mut collected: BTreeMap<String, BTreeMap<u32, TmuxText>> = BTreeMap::new();
    for slot in hook_slots(core, scope).await? {
        let Some((name, index)) = split_slot(&slot) else {
            continue;
        };
        if let Some(value) = get(core, scope, &slot).await? {
            collected
                .entry(name.to_owned())
                .or_default()
                .insert(index, value);
        }
    }

    Ok(collected
        .into_iter()
        .map(|(name, entries)| (name, IndexedHooks::from_entries(entries)))
        .collect())
}

/// Read every index one array option holds.
///
/// Values come back one slot at a time rather than from the listing, for the
/// reason this module gives above: the listing renders a value through
/// `args_escape`, and re-parsing that would be guesswork.
pub(crate) async fn indexed(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
) -> Result<BTreeMap<u32, TmuxText>, Error> {
    let mut entries = BTreeMap::new();
    for slot in slots_of(core, scope, name).await? {
        let Some((_, index)) = split_slot(&slot) else {
            continue;
        };
        if let Some(value) = get(core, scope, &slot).await? {
            entries.insert(index, value);
        }
    }

    Ok(entries)
}

/// List the slots one hook name holds, as `name[index]`.
///
/// Asked for by name rather than taken from the full listing, because tmux
/// answers the two differently: it will not enumerate the hooks set on a
/// window or a pane, but it will list the slots of a hook it is asked about
/// by name at any scope.
async fn slots_of(core: &Core, scope: Scope<'_>, name: &str) -> Result<Vec<String>, Error> {
    let result = core
        .execute(
            scope
                .apply(Command::new("show-options"))
                .arg(OsString::from(name)),
        )
        .await?;
    if !result.success() {
        return Err(Error::refused(
            "show-options",
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            None,
        ));
    }

    Ok(result
        .stdout_lossy()
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|slot| split_slot(slot).is_some_and(|(slot_name, _)| slot_name == name))
        .map(ToOwned::to_owned)
        .collect())
}

/// Read one hook's commands, or `None` when it holds nothing.
pub(crate) async fn hook(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
) -> Result<Option<IndexedHooks>, Error> {
    let mut entries = BTreeMap::new();
    for slot in slots_of(core, scope, name).await? {
        let Some((_, index)) = split_slot(&slot) else {
            continue;
        };
        if let Some(value) = get(core, scope, &slot).await? {
            entries.insert(index, value);
        }
    }

    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(IndexedHooks::from_entries(entries)))
}

/// Read every option set at one scope, decoded by its declared kind.
///
/// One command per option, because a value is read through `show-options -v`
/// for the reason this module gives above. That is the price of getting the
/// stored bytes rather than tmux's display form, and it is why the listing of
/// names is offered separately: a caller who only wants to know what is set
/// does not pay it.
///
/// An array option keeps the indexed name tmux lists it under, so
/// `command-alias[0]` and `command-alias[1]` are separate entries. Its kind is
/// looked up from the name without the index.
pub(crate) async fn typed_all(
    core: &Core,
    scope: Scope<'_>,
) -> Result<BTreeMap<String, OptionValue>, Error> {
    let mut decoded = BTreeMap::new();
    for name in names(core, scope).await? {
        if let Some(value) = get(core, scope, &name).await? {
            let kind_name = split_slot(&name).map_or(name.as_str(), |(base, _)| base);
            decoded.insert(name.clone(), OptionValue::decode(kind_name, value));
        }
    }

    Ok(decoded)
}

/// Write a whole hook at once.
///
/// Sent as one tmux invocation rather than one per index, which costs one
/// process instead of several. That is not atomicity: tmux applies a shared
/// invocation in order and stops at the first refusal, so a rejected entry
/// leaves the ones before it written. It narrows the window, it does not
/// close it.
pub(crate) async fn set_hooks(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
    hooks: &IndexedHooks,
    replace: ReplaceMode,
) -> Result<(), Error> {
    let mut commands = Vec::with_capacity(hooks.len() + 1);
    if replace == ReplaceMode::Replace {
        // Clearing first is what makes this a replacement rather than a
        // merge: an index the caller did not name would otherwise survive.
        commands.push(
            scope
                .apply(Command::new("set-hook"))
                .arg("-u")
                .arg(OsString::from(name)),
        );
    }
    for (index, value) in hooks {
        commands.push(
            scope
                .apply(Command::new("set-hook"))
                .arg(OsString::from(format!("{name}[{index}]")))
                .sensitive_arg(OsString::from(
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )),
        );
    }

    let mut commands = commands.into_iter();
    let Some(first) = commands.next() else {
        return Ok(());
    };
    let result = match commands.next() {
        None => core.execute(first).await?,
        Some(second) => {
            let mut chain = CommandChain::new(first).then(second);
            for command in commands {
                chain = chain.then(command);
            }
            core.execute_chain(chain).await?
        }
    };
    if result.success() {
        return Ok(());
    }

    Err(Error::refused(
        "set-hook",
        result.exit_code(),
        result.stderr_lossy().into_owned(),
        None,
    ))
}
