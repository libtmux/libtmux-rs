//! The implementation behind the environment methods on servers and sessions.
//!
//! The two differ only in which environment they address, so the reading,
//! which is the part with the awkward cases in it, is written once.

use std::collections::BTreeMap;
use std::ffi::OsString;

use crate::internal::core::Core;
use crate::internal::listing;
use crate::{Command, EnvironmentEntry, Error, TmuxText};

/// Which environment a command addresses.
///
/// tmux keeps these genuinely apart rather than layering one over the other:
/// a name set with `-g` is not visible to `show-environment -t`, which
/// reports it as an unknown variable.
#[derive(Clone, Copy)]
pub(crate) enum Scope<'target> {
    /// The server's own environment, tmux's `-g`, which new sessions copy.
    Global,
    /// One session's environment.
    Session(&'target str),
}

impl Scope<'_> {
    /// Point a command at this environment.
    fn apply(self, command: Command) -> Command {
        match self {
            Self::Global => command.arg("-g"),
            Self::Session(target) => command.arg("-t").arg(target),
        }
    }
}

/// Set a variable, so processes started later inherit it.
pub(crate) async fn set(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
    value: OsString,
) -> Result<(), Error> {
    listing::mutate(
        core,
        "set-environment",
        scope
            .apply(Command::new("set-environment"))
            .arg(OsString::from(name))
            // An environment carries tokens, so the value never reaches a log.
            .sensitive_arg(value),
    )
    .await
}

/// Mark a variable, so processes started later are handed it absent.
pub(crate) async fn hide(core: &Core, scope: Scope<'_>, name: &str) -> Result<(), Error> {
    listing::mutate(
        core,
        "set-environment",
        scope
            .apply(Command::new("set-environment"))
            .arg("-r")
            .arg(OsString::from(name)),
    )
    .await
}

/// Delete a variable, letting whatever tmux inherited show through again.
pub(crate) async fn unset(core: &Core, scope: Scope<'_>, name: &str) -> Result<(), Error> {
    listing::mutate(
        core,
        "set-environment",
        scope
            .apply(Command::new("set-environment"))
            .arg("-u")
            .arg(OsString::from(name)),
    )
    .await
}

/// Read one variable back, telling a removal from a value.
///
/// `None` means tmux does not hold the name at all, which is how a
/// continuation line from a multi-line value is discarded.
pub(crate) async fn get(
    core: &Core,
    scope: Scope<'_>,
    name: &str,
) -> Result<Option<EnvironmentEntry>, Error> {
    let result = core
        .execute(
            scope
                .apply(Command::new("show-environment"))
                .arg(OsString::from(name)),
        )
        .await?;
    if !result.success() {
        return Ok(None);
    }

    let stdout = result.stdout();
    let line = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    if line.first() == Some(&b'-') {
        return Ok(Some(EnvironmentEntry::Removed));
    }
    let Some(position) = line.iter().position(|byte| *byte == b'=') else {
        return Ok(None);
    };

    Ok(Some(EnvironmentEntry::Set(TmuxText::from(
        line[position + 1..].to_vec(),
    ))))
}

/// Read the whole environment.
///
/// Costs one tmux command per variable. The listing alone cannot be trusted:
/// a value containing a newline occupies more than one line, and a
/// continuation line holding an `=` is indistinguishable from the next
/// variable. Each name is therefore read back on its own, which also discards
/// the continuation lines, because tmux refuses a name it does not hold.
pub(crate) async fn all(
    core: &Core,
    scope: Scope<'_>,
) -> Result<BTreeMap<String, EnvironmentEntry>, Error> {
    let result = core
        .execute(scope.apply(Command::new("show-environment")))
        .await?;
    if !result.success() {
        return Err(Error::refused(
            "show-environment",
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            None,
        ));
    }

    let candidates: Vec<String> = result
        .stdout_lossy()
        .lines()
        .filter_map(|line| {
            line.strip_prefix('-')
                .map_or_else(|| line.split_once('=').map(|(name, _)| name), Some)
        })
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let mut environment = BTreeMap::new();
    for name in candidates {
        if let Some(entry) = get(core, scope, &name).await? {
            environment.insert(name, entry);
        }
    }

    Ok(environment)
}
