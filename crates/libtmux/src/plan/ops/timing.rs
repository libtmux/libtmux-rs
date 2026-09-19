//! Operations about when the next one runs rather than about an object.

use std::time::Duration;

use crate::Command;

use super::{Chainable, Effects, Op, Operation, Safety};

/// Wait before the next operation runs.
///
/// Renders `run-shell -d SECONDS` with no command, which every supported tmux
/// accepts: the client waits out the delay and runs nothing. The wait happens
/// in tmux, so a pause folded into a shared invocation still falls between
/// its neighbours, and a plan pauses in the same place whatever its
/// [`Planner`](crate::plan::Planner).
///
/// A shell does not need one to catch typed input: tmux holds it until the
/// pane reads it. A pause is for a program that drops what arrives before it
/// is ready.
///
/// A control-mode connection cannot carry one, because tmux answers a delayed
/// `run-shell` there at once and holds the next command instead:
/// [`Plan::run_over_control_mode`](crate::plan::Plan::run_over_control_mode),
/// and [`Plan::run`](crate::plan::Plan::run) on a handle from
/// `Server::over_control_mode`, refuse a plan holding a pause with
/// `ControlModeErrorKind::BlockingCommand` before sending anything.
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use std::time::Duration;
///
/// use libtmux::PaneId;
/// use libtmux::plan::{Pause, Plan, SendKeys};
///
/// let pane: PaneId = "%1".parse()?;
/// let mut plan = Plan::new();
/// plan.add(SendKeys::new(pane.clone()).text("./start-db").enter());
/// plan.add(Pause::new(Duration::from_millis(1500)));
/// plan.add(SendKeys::new(pane).text("./migrate").enter());
///
/// let pause = plan.preview().remove(1).ok_or("a pause renders")?;
/// assert_eq!(pause.summary().to_string(), r#""run-shell" "-d" "1.5""#);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Pause {
    #[cfg_attr(
        feature = "serde",
        serde(
            rename = "seconds",
            serialize_with = "crate::plan::wire::seconds",
            deserialize_with = "crate::plan::wire::parse_seconds"
        )
    )]
    #[cfg_attr(feature = "schema", schemars(rename = "seconds", with = "f64"))]
    duration: Duration,
}

impl Pause {
    /// Wait `duration` before the next operation.
    #[must_use]
    pub const fn new(duration: Duration) -> Self {
        Self { duration }
    }

    /// How long this operation waits.
    #[must_use]
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) fn render(&self) -> Command {
        // tmux reads the delay with `strtod`, and `f64`'s `Display` never
        // writes an exponent, so any `Duration` arrives as plain seconds.
        Command::new("run-shell")
            .arg("-d")
            .arg(self.duration.as_secs_f64().to_string())
    }
}

operation!(
    Pause,
    creates = (),
    effects = Effects {
        read_only: true,
        idempotent: true,
        ..Effects::MUTATING
    },
    safety = Safety::ReadOnly,
    chainable
);
