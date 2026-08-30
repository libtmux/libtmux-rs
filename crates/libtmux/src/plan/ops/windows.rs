//! Operations that make or address a window.

use std::ffi::OsString;
use std::fmt;

use super::{
    Chainable, Effects, Op, Operation, Safety, Scope, SessionTarget, Slot, WindowSlot, WindowTarget,
};
use super::{Resolver, WINDOW_FORMAT};
use crate::Command;
use crate::window::assignment;

/// Create a window in a session.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct NewWindow {
    pub(crate) target: SessionTarget,
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
    name: Option<OsString>,
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
    index: Option<u32>,
    focus: bool,
}

impl NewWindow {
    /// Create a detached window in this session.
    #[must_use]
    pub fn new(target: impl Into<SessionTarget>) -> Self {
        Self {
            target: target.into(),
            name: None,
            start_directory: None,
            command: None,
            environment: Vec::new(),
            index: None,
            focus: false,
        }
    }

    /// Place the window at this index rather than the next free one.
    #[must_use]
    pub const fn index(mut self, index: u32) -> Self {
        self.index = Some(index);
        self
    }

    /// Set a variable in the window's environment before it starts.
    #[must_use]
    pub fn environment(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Leave the new window, and so its pane, active.
    ///
    /// A focused creation is what makes the `{marked}` fold available: the
    /// register the fold marks is the active pane.
    #[must_use]
    pub const fn focus(mut self) -> Self {
        self.focus = true;
        self
    }

    pub(crate) const fn focuses(&self) -> bool {
        self.focus
    }

    /// Name the window.
    ///
    /// tmux expands this as a format, so [`crate::escape_format`] belongs
    /// around text a program did not write.
    #[must_use]
    pub fn name(mut self, name: impl Into<OsString>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Start the window's pane in this directory.
    ///
    /// tmux expands this as a format, so [`crate::escape_format`] belongs
    /// around text a program did not write.
    #[must_use]
    pub fn start_directory(mut self, directory: impl Into<OsString>) -> Self {
        self.start_directory = Some(directory.into());
        self
    }

    /// Run this instead of a shell in the window's pane.
    #[must_use]
    pub fn command(mut self, command: impl Into<OsString>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        // tmux takes the session, not the session's current window, when the
        // target ends in a colon; an index after it asks for that slot.
        let mut token = self.target.token(resolve)?;
        token.push(":");
        if let Some(index) = self.index {
            token.push(index.to_string());
        }
        let mut command = Command::new("new-window")
            .arg("-P")
            .arg("-F")
            .arg(WINDOW_FORMAT)
            .arg("-t")
            .arg(token);
        if !self.focus {
            command = command.arg("-d");
        }
        if let Some(name) = &self.name {
            command = command.arg("-n").arg(name.clone());
        }
        if let Some(directory) = &self.start_directory {
            command = command.arg("-c").arg(directory.clone());
        }
        for (name, value) in &self.environment {
            command = command.arg("-e").sensitive_arg(assignment(name, value));
        }
        // The shell command is positional, so it goes last: tmux stops parsing
        // flags at the first one.
        if let Some(shell_command) = &self.command {
            command = command.sensitive_arg(shell_command.clone());
        }
        Some(command)
    }
}

impl fmt::Debug for NewWindow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewWindow")
            .field("target", &self.target)
            .field("has_name", &self.name.is_some())
            .field("has_start_directory", &self.start_directory.is_some())
            .field("has_command", &self.command.is_some())
            .field("environment_count", &self.environment.len())
            .field("index", &self.index)
            .field("focus", &self.focus)
            .finish()
    }
}

operation!(
    NewWindow,
    creates = Slot<WindowSlot>,
    effects = Effects {
        creates: Some(Scope::Window),
        ..Effects::MUTATING
    },
    safety = Safety::Mutating
);

/// Make a window the active one.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SelectWindow {
    pub(crate) target: WindowTarget,
}

impl SelectWindow {
    /// Focus this window.
    #[must_use]
    pub fn new(target: impl Into<WindowTarget>) -> Self {
        Self {
            target: target.into(),
        }
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        Some(
            Command::new("select-window")
                .arg("-t")
                .arg(self.target.token(resolve)?),
        )
    }
}

operation!(
    SelectWindow,
    creates = (),
    effects = Effects {
        idempotent: true,
        ..Effects::MUTATING
    },
    safety = Safety::Mutating,
    chainable
);

/// Rename a window.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RenameWindow {
    pub(crate) target: WindowTarget,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::argument",
            deserialize_with = "crate::plan::wire::parse_argument"
        )
    )]
    #[cfg_attr(feature = "schema", schemars(with = "crate::plan::wire::Argument"))]
    name: OsString,
}

impl RenameWindow {
    /// Give this window a new name.
    #[must_use]
    pub fn new(target: impl Into<WindowTarget>, name: impl Into<OsString>) -> Self {
        Self {
            target: target.into(),
            name: name.into(),
        }
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        Some(
            Command::new("rename-window")
                .arg("-t")
                .arg(self.target.token(resolve)?)
                .arg("--")
                .arg(self.name.clone()),
        )
    }
}

operation!(
    RenameWindow,
    creates = (),
    effects = Effects {
        idempotent: true,
        ..Effects::MUTATING
    },
    safety = Safety::Mutating,
    chainable
);

/// Destroy a window.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct KillWindow {
    pub(crate) target: WindowTarget,
}

impl KillWindow {
    /// Kill this window.
    #[must_use]
    pub fn new(target: impl Into<WindowTarget>) -> Self {
        Self {
            target: target.into(),
        }
    }

    /// The window this operation will destroy.
    #[must_use]
    pub const fn target(&self) -> &WindowTarget {
        &self.target
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        Some(
            Command::new("kill-window")
                .arg("-t")
                .arg(self.target.token(resolve)?),
        )
    }
}

operation!(
    KillWindow,
    creates = (),
    effects = Effects {
        destructive: true,
        ..Effects::MUTATING
    },
    safety = Safety::Destructive,
    chainable
);

/// Rearrange a window's panes into a named or described layout.
///
/// Applied once the pane count is final: tmux rebalances a layout on the next
/// split, so applying it earlier is work the next split undoes.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(deny_unknown_fields))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SelectLayout {
    pub(crate) target: WindowTarget,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::argument",
            deserialize_with = "crate::plan::wire::parse_argument"
        )
    )]
    #[cfg_attr(feature = "schema", schemars(with = "crate::plan::wire::Argument"))]
    layout: OsString,
}

impl SelectLayout {
    /// Lay this window out.
    #[must_use]
    pub fn new(target: impl Into<WindowTarget>, layout: impl Into<OsString>) -> Self {
        Self {
            target: target.into(),
            layout: layout.into(),
        }
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        Some(
            Command::new("select-layout")
                .arg("-t")
                .arg(self.target.token(resolve)?)
                .arg("--")
                .arg(self.layout.clone()),
        )
    }
}

operation!(
    SelectLayout,
    creates = (),
    effects = Effects {
        idempotent: true,
        ..Effects::MUTATING
    },
    safety = Safety::Mutating,
    chainable
);
