#![doc = include_str!("../README.md")]

#[cfg(feature = "serde")]
mod creation_wire;
mod error;
mod framed;
mod new_session;
mod operand;
mod receipt;
mod reply;
#[doc(hidden)]
pub mod supervision;
mod target;
mod text;
mod version;

pub use error::{CommandKind, Effect, Error};
pub use new_session::NewSession;
pub use operand::{Operand, OperandKind, argv_operand};
pub use receipt::{Capture, CreatedSession, ObservePane, PaneKey};
pub use reply::Reply;
pub use target::{
    IdParseError, PaneId, PaneTarget, ServerGeneration, ServerIdentity, SessionId, SessionName,
    SessionNameError, SessionTarget, TmuxArg, WindowId, WindowTarget, escape_format,
};
pub use text::TmuxText;
pub use version::{ReleaseSuffix, ReleaseVersion, TmuxVersion, VersionError, since};

fn parse_number<T: std::str::FromStr>(digits: &str) -> Option<T> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
