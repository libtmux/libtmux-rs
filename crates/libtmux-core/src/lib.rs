#![doc = include_str!("../README.md")]

#[cfg(feature = "derive")]
extern crate self as libtmux_core;

mod command;
pub mod control;
#[cfg(feature = "serde")]
mod creation_wire;
mod error;
#[path = "formats/catalog.rs"]
mod format_catalog;
#[doc(hidden)]
pub mod formats;
mod framed;
mod new_session;
mod operand;
pub mod options;
mod ordinary_capture;
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
pub mod snapshot;
#[doc(hidden)]
pub mod supervision;
mod target;
mod text;
mod version;
mod window_link;
#[cfg(feature = "serde")]
mod wire_arg;

#[cfg(feature = "derive")]
pub use libtmux_macros::Filterable;

pub use command::{Command, CommandChain, CommandSummary};
pub use error::{CommandKind, Effect, Error};
#[doc(inline)]
pub use formats::FormatCodecError;
pub use new_session::NewSession;
pub use operand::{Operand, OperandKind, argv_operand};
#[doc(hidden)]
pub use ordinary_capture::OrdinaryCapturePrefix;
pub use ordinary_creation::CreationNonce;
pub use ordinary_read::{OrdinaryGenerationProbe, OrdinaryPaneObservation, OrdinaryPaneProbe};
#[doc(hidden)]
pub use placement::assignment;
pub use placement::{OptionScope, SplitDirection};
pub use receipt::{
    Capture, CreatedSession, ObjectKey, ObservePane, PaneKey, SessionKey, WindowKey,
};
pub use reply::Reply;
#[doc(inline)]
pub use snapshot::list::{Snapshot, SnapshotList};
#[doc(inline)]
pub use snapshot::{
    Availability, ClientSnapshot, PaneProgressState, PaneSnapshot, SessionSnapshot, WindowSnapshot,
};
#[cfg(feature = "query")]
#[doc(inline)]
pub use snapshot::{ClientFields, PaneFields, SessionFields, WindowFields};
pub use target::{
    IdParseError, PaneId, PaneTarget, ServerGeneration, ServerIdentity, SessionId, SessionName,
    SessionNameError, SessionTarget, TmuxArg, WindowId, WindowTarget, escape_format,
};
pub use text::TmuxText;
pub use version::{
    OrdinaryVersionProbe, ReleaseSuffix, ReleaseVersion, TmuxVersion, VersionError, since,
};
pub use window_link::WindowLinkSnapshot;

fn parse_number<T: std::str::FromStr>(digits: &str) -> Option<T> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
