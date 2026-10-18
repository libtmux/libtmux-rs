#![doc = include_str!("../README.md")]

pub mod control;

mod error;
mod target;
mod text;
mod version;

pub use error::IdParseError;
pub use target::{
    PaneId, PaneTarget, SessionId, SessionName, SessionNameError, SessionTarget, TmuxArg, WindowId,
    WindowTarget, escape_format,
};
pub use text::TmuxText;
pub use version::{ReleaseSuffix, ReleaseVersion};
