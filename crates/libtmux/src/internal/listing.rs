//! The shared path from a list command to hydrated snapshots.
//!
//! Every public listing follows the same four steps: build a format plan for
//! the detected tmux version, render its template into a list command, parse
//! the transport bytes, and hydrate rows. Keeping that here means the public
//! handles differ only in the target they scope to.

use std::ffi::{OsStr, OsString};

use crate::error::ListingDecodeError;
use crate::formats::{FormatCodecError, FormatPlan, ListProfile};
use crate::internal::core::Core;
use crate::snapshot::{
    ClientInfo, PaneProjection, SessionInfo, WindowProjection, hydrate_client_infos_from_stdout,
    hydrate_pane_projections_from_stdout, hydrate_session_infos_from_stdout,
    hydrate_window_projections_from_stdout, pane_projection_plan, window_projection_plan,
};
use crate::{Command, Error};

/// How a listing is scoped.
///
/// tmux spells "everything on the server" and "everything under this object"
/// with different flags, so the two are distinguished here rather than by
/// passing an optional target that callers could forget.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Scope<'target> {
    /// Every object on the server, using tmux's `-a` flag.
    Server,
    /// Only objects under one target, using tmux's `-t` flag.
    ///
    /// For `list-panes` the target is a window; tmux resolves a session target
    /// to that session's current window, which is rarely what a caller means.
    Target(&'target str),
    /// Every pane in one session, using tmux's `-s -t` flags.
    ///
    /// `list-panes` needs `-s` to widen a session target from the current
    /// window to the whole session.
    SessionTarget(&'target str),
    /// No scoping flag, for commands that are already server-wide.
    ///
    /// `list-sessions` and `list-clients` cover the whole server and reject
    /// `-a`, so they take neither flag.
    Unscoped,
}

impl<'target> Scope<'target> {
    /// Return the target this scope names, if it names one.
    const fn target(self) -> Option<&'target str> {
        match self {
            Self::Target(target) | Self::SessionTarget(target) => Some(target),
            Self::Server | Self::Unscoped => None,
        }
    }

    /// Apply this scope's flags to a list command.
    fn apply(self, command: Command) -> Command {
        match self {
            Self::Server => command.arg("-a"),
            Self::Target(target) => command.arg("-t").arg(OsString::from(target)),
            Self::SessionTarget(target) => command.arg("-s").arg("-t").arg(OsString::from(target)),
            Self::Unscoped => command,
        }
    }
}

/// A value that may be interpolated into a tmux `-f` predicate.
///
/// tmux documents no escaping for a predicate, so a value that could contain
/// `#`, `}`, or a comma would change what the predicate means. Only values
/// drawn from a validated domain implement this: an id is a sigil followed by
/// digits, and an index is an integer. A name is user-chosen text and is
/// deliberately absent, which is what keeps the rule enforced by the compiler
/// rather than by remembering it.
pub(crate) trait Pushdown {
    /// Render this value as a tmux predicate comparing one format field.
    fn predicate(&self, field: &str) -> String;
}

macro_rules! pushdown_via_display {
    ($($type:ty),+ $(,)?) => {
        $(impl Pushdown for $type {
            fn predicate(&self, field: &str) -> String {
                format!("#{{==:#{{{field}}},{self}}}")
            }
        })+
    };
}

pushdown_via_display!(crate::SessionId, crate::WindowId, crate::PaneId, i32, u32,);

/// Run one list command and return its raw stdout.
async fn list(
    core: &Core,
    list_command: &'static str,
    scope: Scope<'_>,
    filter: Option<&str>,
    template: &str,
) -> Result<Vec<u8>, Error> {
    let mut command = scope.apply(Command::new(list_command));
    // tmux evaluates the predicate per row, so a filtered listing returns only
    // matching rows rather than every row for the caller to scan.
    if let Some(filter) = filter {
        command = command.arg("-f").arg(OsString::from(filter));
    }
    let result = core
        .execute(command.arg("-F").arg(OsString::from(template)))
        .await?;
    if !result.success() {
        let stderr = result.stderr_lossy().into_owned();
        // A server holding no sessions has no current target, and tmux says
        // so even for `-a`, which asks for everything. A server-wide listing
        // has nothing to list; a listing under a target could not resolve it,
        // which the classifier reports as the target being gone.
        if scope.target().is_none() && stderr.trim_end() == crate::error::NO_CURRENT_TARGET {
            return Ok(Vec::new());
        }

        // Anything else is not an empty listing. The lenient accessors turn
        // this back into an empty Vec; the `try_` forms exist so a caller who
        // must not guess gets the reason instead.
        return Err(Error::refused(
            list_command,
            result.exit_code(),
            stderr,
            scope.target().map(OsStr::new),
        ));
    }

    Ok(result.stdout().to_vec())
}

/// Convert a private codec failure into the public listing error.
fn decode_error(list_command: &'static str) -> impl Fn(FormatCodecError) -> Error {
    move |error| Error::DecodeListing {
        list_command,
        detail: ListingDecodeError::new(error),
    }
}

/// List sessions.
pub(crate) async fn sessions(core: &Core, filter: Option<&str>) -> Result<Vec<SessionInfo>, Error> {
    const LIST_COMMAND: &str = "list-sessions";

    let version = core.capabilities().await?.tmux_version().clone();
    let plan = FormatPlan::for_profile(ListProfile::Sessions, &version);
    let stdout = list(core, LIST_COMMAND, Scope::Unscoped, filter, plan.template()).await?;

    hydrate_session_infos_from_stdout(&plan, &stdout).map_err(decode_error(LIST_COMMAND))
}

/// List windows, either server-wide or under one target.
pub(crate) async fn windows(
    core: &Core,
    scope: Scope<'_>,
    filter: Option<&str>,
) -> Result<Vec<WindowProjection>, Error> {
    const LIST_COMMAND: &str = "list-windows";

    let version = core.capabilities().await?.tmux_version().clone();
    let plan = window_projection_plan(&version).map_err(decode_error(LIST_COMMAND))?;
    let stdout = list(core, LIST_COMMAND, scope, filter, plan.template()).await?;

    hydrate_window_projections_from_stdout(core.configuration().identity(), &plan, &stdout)
        .map_err(decode_error(LIST_COMMAND))
}

/// List panes, either server-wide or under one target.
pub(crate) async fn panes(
    core: &Core,
    scope: Scope<'_>,
    filter: Option<&str>,
) -> Result<Vec<PaneProjection>, Error> {
    const LIST_COMMAND: &str = "list-panes";

    let version = core.capabilities().await?.tmux_version().clone();
    let plan = pane_projection_plan(&version).map_err(decode_error(LIST_COMMAND))?;
    let stdout = list(core, LIST_COMMAND, scope, filter, plan.template()).await?;

    hydrate_pane_projections_from_stdout(core.configuration().identity(), &plan, &stdout)
        .map_err(decode_error(LIST_COMMAND))
}

/// List attached clients.
pub(crate) async fn clients(core: &Core, filter: Option<&str>) -> Result<Vec<ClientInfo>, Error> {
    const LIST_COMMAND: &str = "list-clients";

    let version = core.capabilities().await?.tmux_version().clone();
    let plan = FormatPlan::for_profile(ListProfile::Clients, &version);
    let stdout = list(core, LIST_COMMAND, Scope::Unscoped, filter, plan.template()).await?;

    hydrate_client_infos_from_stdout(&plan, &stdout).map_err(decode_error(LIST_COMMAND))
}

/// Run a creating command that prints its new object, and hydrate it.
///
/// tmux's `-P -F` prints the created object through the same format machinery
/// as a listing, so creation costs one round trip rather than a create
/// followed by a lookup.
async fn create_one<T>(
    core: &Core,
    command_name: &'static str,
    build: impl FnOnce(&str) -> Command,
    template: &str,
    hydrate: impl FnOnce(&[u8]) -> Result<Vec<T>, Error>,
) -> Result<T, Error> {
    // The builder places `-P -F` itself, because tmux stops parsing flags at
    // the first positional and these commands end with a shell command.
    let command = build(template);
    let target = command.target().map(OsStr::to_os_string);
    let result = core.execute(command).await?;
    if !result.success() {
        return Err(Error::refused(
            command_name,
            result.exit_code(),
            result.stderr_lossy().into_owned(),
            target.as_deref(),
        ));
    }

    hydrate(result.stdout())?
        .into_iter()
        .next()
        .ok_or(Error::CommandFailed {
            command: command_name,
            exit_code: result.exit_code(),
            stderr: String::from("tmux printed no object for a creating command"),
        })
}

/// Create one session and return its hydrated snapshot.
pub(crate) async fn create_session(
    core: &Core,
    build: impl FnOnce(&str) -> Command,
) -> Result<SessionInfo, Error> {
    let version = core.capabilities().await?.tmux_version().clone();
    let plan = FormatPlan::for_profile(ListProfile::Sessions, &version);
    let template = plan.template().to_owned();

    create_one(core, "new-session", build, &template, |stdout| {
        hydrate_session_infos_from_stdout(&plan, stdout).map_err(decode_error("new-session"))
    })
    .await
}

/// Create one window and return its hydrated projection.
pub(crate) async fn create_window(
    core: &Core,
    build: impl FnOnce(&str) -> Command,
) -> Result<WindowProjection, Error> {
    let version = core.capabilities().await?.tmux_version().clone();
    let plan = window_projection_plan(&version).map_err(decode_error("new-window"))?;
    let template = plan.template().to_owned();
    let identity = core.configuration().identity();

    create_one(core, "new-window", build, &template, |stdout| {
        hydrate_window_projections_from_stdout(identity, &plan, stdout)
            .map_err(decode_error("new-window"))
    })
    .await
}

/// Create one pane and return its hydrated projection.
pub(crate) async fn create_pane(
    core: &Core,
    build: impl FnOnce(&str) -> Command,
) -> Result<PaneProjection, Error> {
    let version = core.capabilities().await?.tmux_version().clone();
    let plan = pane_projection_plan(&version).map_err(decode_error("split-window"))?;
    let template = plan.template().to_owned();
    let identity = core.configuration().identity();

    create_one(core, "split-window", build, &template, |stdout| {
        hydrate_pane_projections_from_stdout(identity, &plan, stdout)
            .map_err(decode_error("split-window"))
    })
    .await
}

/// Run a mutation that returns nothing, requiring tmux to accept it.
pub(crate) async fn mutate(
    core: &Core,
    command_name: &'static str,
    command: Command,
) -> Result<(), Error> {
    let target = command.target().map(OsStr::to_os_string);
    let result = core.execute(command).await?;
    if result.success() {
        return Ok(());
    }

    Err(Error::refused(
        command_name,
        result.exit_code(),
        result.stderr_lossy().into_owned(),
        target.as_deref(),
    ))
}

/// Record a cleanup failure that a scoped operation is about to discard.
///
/// It is discarded because the operation failed first, and that error is the
/// one the caller was pursuing. Without `tracing` the failure is lost, which
/// is the price of returning one error rather than two.
#[cfg_attr(
    not(feature = "tracing"),
    expect(
        unused_variables,
        reason = "the cause has no sink when tracing is disabled"
    )
)]
pub(crate) fn trace_discarded_cleanup(error: &Error) {
    #[cfg(feature = "tracing")]
    tracing::debug!(
        error = %error,
        "a scoped operation discarded a cleanup failure after its body failed",
    );
}
