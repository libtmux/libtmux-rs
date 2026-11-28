//! Operations that make or address a session.

use super::{Effects, Op, Operation, Safety, Scope, SessionSlot, Slot};
pub use libtmux_core::NewSession;

operation!(
    NewSession,
    creates = Slot<SessionSlot>,
    effects = Effects {
        creates: Some(Scope::Session),
        ..Effects::MUTATING
    },
    safety = Safety::Mutating
);
