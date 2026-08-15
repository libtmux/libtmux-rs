//! The operations a [`Plan`] can record, grouped as tmux groups its own
//! commands: by the object they are about.
//!
//! Each type is inert: it holds what one tmux command needs and renders to a
//! [`Command`], but reaching tmux is [`Plan`]'s job. The class-level answers a
//! planner needs -- what it creates, whether it may share an invocation, how
//! much damage it does -- are associated constants, so the type is the single
//! place they are stated.
//!
//! [`Plan`]: super::Plan

use std::ffi::OsString;

use super::{
    Chainable, Effects, Op, Operation, PaneSlot, PaneTarget, Part, Safety, Scope, SessionSlot,
    SessionTarget, Slot, WindowSlot, WindowTarget,
};

/// The format a creating operation prints so its ids can be bound.
const SESSION_FORMAT: &str = "#{session_id} #{window_id} #{pane_id}";
const WINDOW_FORMAT: &str = "#{window_id} #{pane_id}";
const PANE_FORMAT: &str = "#{pane_id}";

/// Resolves a slot to the tmux token that addresses it.
type Resolver<'a> = &'a dyn Fn(usize, Part) -> Option<OsString>;

macro_rules! operation {
    (
        $name:ident,
        creates = $creates:ty,
        effects = $effects:expr,
        safety = $safety:expr,
        chainable
    ) => {
        operation!(
            $name,
            creates = $creates,
            effects = $effects,
            safety = $safety
        );
        impl Chainable for $name {}
    };
    (
        $name:ident,
        creates = $creates:ty,
        effects = $effects:expr,
        safety = $safety:expr
    ) => {
        impl Operation for $name {
            type Creates = $creates;
            const EFFECTS: Effects = $effects;
            const SAFETY: Safety = $safety;
        }

        impl From<$name> for Op {
            fn from(operation: $name) -> Self {
                Self::$name(operation)
            }
        }
    };
}

// Declared after the macro, which is what puts it in scope for them.
mod options;
mod panes;
mod sessions;
mod windows;

pub use options::{SetEnvironment, SetOption};
pub use panes::{CapturePane, KillPane, SelectPane, SendKeys, SplitWindow};
pub use sessions::NewSession;
pub use windows::{KillWindow, NewWindow, RenameWindow, SelectLayout, SelectWindow};
