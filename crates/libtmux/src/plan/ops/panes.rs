//! Operations that make or address a pane.
//!
//! `split-window` lives here rather than with the windows: it is addressed at
//! a window but the object it produces, and everything that follows it in a
//! plan, is a pane.

use std::ffi::OsString;
use std::fmt;

use super::{
    Chainable, Effects, Op, Operation, PaneSlot, PaneTarget, Safety, Scope, Slot, WindowTarget,
};
use super::{PANE_FORMAT, Resolver};
use crate::window::assignment;
use crate::{Command, SplitDirection};

/// Split a window, making a pane.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SplitWindow {
    pub(crate) target: WindowTarget,
    vertical: bool,
    /// tmux's `-b`, which puts the new pane before the one being divided.
    ///
    /// Defaulted rather than required, so a plan serialized before this
    /// existed still decodes: `deny_unknown_fields` refuses keys it does not
    /// know, not keys that are absent.
    #[cfg_attr(feature = "serde", serde(default))]
    before: bool,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<crate::plan::wire::Argument>")
    )]
    start_directory: Option<OsString>,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<crate::plan::wire::Argument>")
    )]
    command: Option<OsString>,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::pairs",
            deserialize_with = "crate::plan::wire::parse_pairs"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Vec<(crate::plan::wire::Argument, crate::plan::wire::Argument)>")
    )]
    environment: Vec<(OsString, OsString)>,
    focus: bool,
}

impl SplitWindow {
    /// Split this window, leaving the new pane unfocused.
    #[must_use]
    pub fn new(target: impl Into<WindowTarget>) -> Self {
        Self {
            target: target.into(),
            vertical: true,
            before: false,
            start_directory: None,
            command: None,
            environment: Vec::new(),
            focus: false,
        }
    }

    /// Set a variable in the new pane's environment before it starts.
    #[must_use]
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Split side by side rather than one above the other.
    ///
    /// Exactly [`SplitDirection::Right`], and defined as it so the two cannot
    /// disagree. Kept because it is the spelling this operation shipped with.
    ///
    /// It sets the side as well as the axis, so it is the whole position
    /// rather than half of one: `direction(Above).horizontal()` is `Right`,
    /// not `Left`. Either setter, in either order, leaves one of the four
    /// positions and never a mix of two.
    #[must_use]
    pub const fn horizontal(self) -> Self {
        self.direction(SplitDirection::Right)
    }

    /// Put the new pane on this side of the one being divided.
    ///
    /// The default is [`SplitDirection::Below`], which is tmux's. `Above` and
    /// `Left` need tmux's `-b`, so they are reachable only through this: with
    /// [`SplitWindow::horizontal`] alone a plan could ask for two of the four
    /// positions, while the object API's [`crate::SplitOptions`] has always
    /// offered all four.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::SplitDirection;
    /// use libtmux::plan::{NewSession, Plan, SplitWindow};
    ///
    /// let mut plan = Plan::new();
    /// let session = plan.add(NewSession::new("above"));
    /// plan.add(SplitWindow::new(session.window()).direction(SplitDirection::Above));
    ///
    /// assert_eq!(plan.len(), 2);
    /// ```
    #[must_use]
    pub const fn direction(mut self, direction: SplitDirection) -> Self {
        self.vertical = direction.is_vertical();
        self.before = direction.before();
        self
    }

    /// Start the new pane in this directory.
    ///
    /// tmux expands this as a format, so [`crate::escape_format`] belongs
    /// around text a program did not write.
    #[must_use]
    pub fn start_directory(mut self, directory: impl Into<OsString>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Run this instead of a shell in the new pane.
    #[must_use]
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    /// Leave the new pane focused.
    ///
    /// A focused split is what makes the `{marked}` fold available: the
    /// register the fold marks is the active pane.
    #[must_use]
    pub const fn focus(mut self) -> Self {
        self.focus = true;
        self
    }

    pub(crate) const fn focuses(&self) -> bool {
        self.focus
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        let mut command = Command::new("split-window")
            .arg("-P")
            .arg("-F")
            .arg(PANE_FORMAT)
            .arg("-t")
            .arg(self.target.token(resolve)?)
            .arg(if self.vertical { "-v" } else { "-h" });
        if self.before {
            command = command.arg("-b");
        }
        if !self.focus {
            command = command.arg("-d");
        }
        if let Some(directory) = &self.start_directory {
            command = command.arg("-c").arg(directory.clone());
        }
        for (name, value) in &self.environment {
            command = command.arg("-e").sensitive_arg(assignment(name, value));
        }
        if let Some(shell_command) = &self.command {
            command = command.sensitive_arg(shell_command.clone());
        }
        Some(command)
    }
}

impl fmt::Debug for SplitWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SplitWindow")
            .field("target", &self.target)
            .field("vertical", &self.vertical)
            .field("before", &self.before)
            .field("has_start_directory", &self.start_directory.is_some())
            .field("has_command", &self.command.is_some())
            .field("environment_count", &self.environment.len())
            .field("focus", &self.focus)
            .finish()
    }
}

operation!(
    SplitWindow,
    creates = Slot<PaneSlot>,
    effects = Effects {
        creates: Some(Scope::Pane),
        ..Effects::MUTATING
    },
    safety = Safety::Mutating
);

/// Send text or named keys to a pane.
///
/// [`SendKeys::text`] and [`SendKeys::keys`] cannot both be set: tmux
/// resolves every `send-keys` argument against its key table unless `-l`
/// literalizes it, and `-l` covers every argument of the `send-keys` it sits
/// on. So literal text and named keys cannot share one `send-keys`, though
/// two of them still share one tmux invocation under a folding planner.
/// [`crate::plan::Plan::validate`] rejects a `SendKeys` that carries both,
/// before a plan runs.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SendKeys {
    pub(crate) target: PaneTarget,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Option<crate::plan::wire::Argument>")
    )]
    text: Option<OsString>,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::list",
            deserialize_with = "crate::plan::wire::parse_list"
        )
    )]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Vec<crate::plan::wire::Argument>")
    )]
    keys: Vec<OsString>,
    enter: bool,
}

impl SendKeys {
    /// Address this pane.
    #[must_use]
    pub fn new(target: impl Into<PaneTarget>) -> Self {
        Self {
            target: target.into(),
            text: None,
            keys: Vec::new(),
            enter: false,
        }
    }

    /// Send this literal text.
    ///
    /// Rendered with tmux's `-l` flag, so every byte is typed rather than
    /// looked up in the key table first -- text that happens to name a key,
    /// such as `"Space"`, is typed rather than pressed. `-l` literalizes every
    /// argument of the `send-keys` it is on, so this cannot be combined with
    /// [`SendKeys::keys`]: [`crate::plan::Plan::validate`] rejects that
    /// combination before anything runs.
    #[must_use]
    pub fn text(mut self, text: impl Into<OsString>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Send named keys such as `C-c` or `Escape`.
    ///
    /// Resolved against tmux's key table, which is why this cannot be
    /// combined with [`SendKeys::text`]: literal text needs `-l`, and `-l`
    /// would literalize these too. [`crate::plan::Plan::validate`] rejects
    /// the combination before anything runs.
    #[must_use]
    pub fn keys<K: Into<OsString>>(mut self, keys: impl IntoIterator<Item = K>) -> Self {
        self.keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Follow the text with Enter.
    ///
    /// When literal text is set, Enter is folded into it as a trailing
    /// carriage return rather than sent as a separate named key -- the same
    /// dispatch [`crate::Pane::send_line`] uses to submit a line in one
    /// command. Without text, Enter renders as the named key after
    /// whatever [`SendKeys::keys`] sends.
    #[must_use]
    pub const fn enter(mut self) -> Self {
        self.enter = true;
        self
    }

    /// Whether this cannot render as one `send-keys`.
    ///
    /// Literal text needs `-l`, and `-l` literalizes every argument of the
    /// same `send-keys`, so a named key cannot survive alongside text. Enter
    /// is not a named key here: [`SendKeys::render`] folds it into the
    /// literal payload instead, so it never conflicts.
    pub(crate) fn text_conflicts_with_keys(&self) -> bool {
        self.text.is_some() && !self.keys.is_empty()
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        let mut command = Command::new("send-keys")
            .arg("-t")
            .arg(self.target.token(resolve)?);
        if let Some(text) = &self.text {
            // `-l` literalizes every argument of this `send-keys`. Enter is
            // folded into the payload as a trailing carriage return instead
            // of the named key `Enter`, matching how `Pane::send_line`
            // dispatches a line for the object API. A plan carrying `keys`
            // here as well is rejected by `Plan::validate` before this runs.
            let mut literal = text.clone();
            if self.enter {
                literal.push("\r");
            }
            command = command.arg("-l").arg("--").sensitive_arg(literal);
        } else if !self.keys.is_empty() || self.enter {
            // Everything after `--` is a value, so a key name that starts
            // with a dash is typed rather than read as a flag.
            command = command.arg("--");
            for key in &self.keys {
                command = command.arg(key.clone());
            }
            if self.enter {
                command = command.arg("Enter");
            }
        }
        Some(command)
    }
}

impl fmt::Debug for SendKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SendKeys")
            .field("target", &self.target)
            .field("has_text", &self.text.is_some())
            .field("key_count", &self.keys.len())
            .field("enter", &self.enter)
            .finish()
    }
}

operation!(
    SendKeys,
    creates = (),
    effects = Effects::MUTATING,
    safety = Safety::Mutating,
    chainable
);

/// Make a pane the active one.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SelectPane {
    pub(crate) target: PaneTarget,
}

impl SelectPane {
    /// Focus this pane.
    #[must_use]
    pub fn new(target: impl Into<PaneTarget>) -> Self {
        Self {
            target: target.into(),
        }
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        Some(
            Command::new("select-pane")
                .arg("-t")
                .arg(self.target.token(resolve)?),
        )
    }
}

operation!(
    SelectPane,
    creates = (),
    effects = Effects {
        idempotent: true,
        ..Effects::MUTATING
    },
    safety = Safety::Mutating,
    chainable
);

/// Read a pane's contents.
///
/// Deliberately not [`Chainable`]: its stdout is the answer, and a folded run
/// returns one merged stdout with no per-command boundary, so folding it would
/// mix its lines with its neighbours'.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CapturePane {
    pub(crate) target: PaneTarget,
    escape_sequences: bool,
}

impl CapturePane {
    /// Capture this pane.
    #[must_use]
    pub fn new(target: impl Into<PaneTarget>) -> Self {
        Self {
            target: target.into(),
            escape_sequences: false,
        }
    }

    /// Keep the escape sequences rather than the plain text.
    #[must_use]
    pub const fn escape_sequences(mut self) -> Self {
        self.escape_sequences = true;
        self
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        let mut command = Command::new("capture-pane")
            .arg("-p")
            .arg("-t")
            .arg(self.target.token(resolve)?);
        if self.escape_sequences {
            command = command.arg("-e");
        }
        Some(command)
    }
}

operation!(
    CapturePane,
    creates = (),
    effects = Effects {
        read_only: true,
        idempotent: true,
        reads_output: true,
        ..Effects::MUTATING
    },
    safety = Safety::ReadOnly
);

/// Destroy a pane.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct KillPane {
    pub(crate) target: PaneTarget,
}

impl KillPane {
    /// Kill this pane.
    #[must_use]
    pub fn new(target: impl Into<PaneTarget>) -> Self {
        Self {
            target: target.into(),
        }
    }

    /// The pane this operation will destroy.
    #[must_use]
    pub const fn target(&self) -> &PaneTarget {
        &self.target
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        Some(
            Command::new("kill-pane")
                .arg("-t")
                .arg(self.target.token(resolve)?),
        )
    }
}

operation!(
    KillPane,
    creates = (),
    effects = Effects {
        destructive: true,
        ..Effects::MUTATING
    },
    safety = Safety::Destructive,
    chainable
);
