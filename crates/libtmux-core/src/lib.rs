#![doc = include_str!("../README.md")]

#[cfg(feature = "derive")]
extern crate self as libtmux_core;

mod command;
#[cfg(feature = "serde")]
mod creation_wire;
mod error;
mod framed;
mod new_session;
mod operand;
mod ordinary_creation;
mod ordinary_read;
mod placement;
#[cfg(feature = "plan")]
pub mod plan;
#[cfg(feature = "query")]
pub mod query;
mod receipt;
mod reply;
#[doc(hidden)]
pub mod supervision;
mod target;
mod text;
mod version;
#[cfg(feature = "serde")]
mod wire_arg;

#[cfg(feature = "derive")]
pub use libtmux_macros::Filterable;

pub use command::{Command, CommandChain, CommandSummary};
pub use error::{CommandKind, Effect, Error};
pub use new_session::NewSession;
pub use operand::{Operand, OperandKind, argv_operand};
pub use ordinary_creation::CreationNonce;
pub use ordinary_read::{OrdinaryPaneObservation, OrdinaryPaneProbe};
#[doc(hidden)]
pub use placement::assignment;
pub use placement::{OptionScope, SplitDirection};
pub use receipt::{Capture, CreatedSession, ObservePane, PaneKey};
pub use reply::Reply;
pub use target::{
    IdParseError, PaneId, PaneTarget, ServerGeneration, ServerIdentity, SessionId, SessionName,
    SessionNameError, SessionTarget, TmuxArg, WindowId, WindowTarget, escape_format,
};
pub use text::TmuxText;
pub use version::{
    OrdinaryVersionProbe, ReleaseSuffix, ReleaseVersion, TmuxVersion, VersionError, since,
};

fn parse_number<T: std::str::FromStr>(digits: &str) -> Option<T> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
