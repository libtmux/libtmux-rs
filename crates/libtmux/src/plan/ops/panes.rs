//! Operations that make or address a pane.
//!
//! `split-window` lives here rather than with the windows: it is addressed at
//! a window but the object it produces, and everything that follows it in a
//! plan, is a pane.

use std::ffi::OsString;

use super::{
    Chainable, Effects, Op, Operation, PaneSlot, PaneTarget, Safety, Scope, Slot, WindowTarget,
};
use super::{PANE_FORMAT, Resolver};
use crate::Command;
use crate::window::assignment;

/// Split a window, making a pane.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SplitWindow {
    pub(crate) target: WindowTarget,
    vertical: bool,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    start_directory: Option<OsString>,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    command: Option<OsString>,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::pairs",
            deserialize_with = "crate::plan::wire::parse_pairs"
        )
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
    #[must_use]
    pub const fn horizontal(mut self) -> Self {
        self.vertical = false;
        self
    }

    /// Start the new pane in this directory.
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
        if !self.focus {
            command = command.arg("-d");
        }
        if let Some(directory) = &self.start_directory {
            command = command.arg("-c").arg(directory.clone());
        }
        for (name, value) in &self.environment {
            command = command.arg("-e").arg(assignment(name, value));
        }
        if let Some(shell_command) = &self.command {
            command = command.arg(shell_command.clone());
        }
        Some(command)
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
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SendKeys {
    pub(crate) target: PaneTarget,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::optional_argument",
            deserialize_with = "crate::plan::wire::parse_optional_argument"
        )
    )]
    text: Option<OsString>,
    #[cfg_attr(
        feature = "serde",
        serde(
            serialize_with = "crate::plan::wire::list",
            deserialize_with = "crate::plan::wire::parse_list"
        )
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
    #[must_use]
    pub fn text(mut self, text: impl Into<OsString>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Send named keys such as `C-c` or `Escape`.
    #[must_use]
    pub fn keys<K: Into<OsString>>(mut self, keys: impl IntoIterator<Item = K>) -> Self {
        self.keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Follow the text with Enter.
    #[must_use]
    pub const fn enter(mut self) -> Self {
        self.enter = true;
        self
    }

    pub(crate) fn render(&self, resolve: Resolver<'_>) -> Option<Command> {
        let mut command = Command::new("send-keys")
            .arg("-t")
            .arg(self.target.token(resolve)?);
        // Everything after `--` is a value, so text that starts with a dash is
        // typed rather than read as a flag.
        if self.text.is_some() || !self.keys.is_empty() || self.enter {
            command = command.arg("--");
        }
        if let Some(text) = &self.text {
            command = command.arg(text.clone());
        }
        for key in &self.keys {
            command = command.arg(key.clone());
        }
        if self.enter {
            command = command.arg("Enter");
        }
        Some(command)
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
