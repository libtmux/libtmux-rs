#![doc = include_str!("../README.md")]

#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub use libtmux::__fuzz_environment_listing;
#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub use libtmux::__fuzz_format_rows;
#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub use libtmux::__fuzz_parse_key_bindings;
pub use libtmux::Client;
#[cfg(feature = "control-mode")]
pub use libtmux::ControlModeErrorKind;
pub use libtmux::EngineCapabilities;
pub use libtmux::TmuxText;
pub use libtmux::{
    AccessMode, AccessRule, ChannelWait, Chooser, KeyBinding, MenuItem, NewSessionOptions,
    Principal, PromptKind, SessionTree, WindowTree,
};
pub use libtmux::{Availability, PaneProgressState};
pub use libtmux::{CaptureOptions, CapturedLine, Pane, PaneWait};
#[cfg(feature = "query")]
pub use libtmux::{ClientFields, PaneFields, SessionFields, WindowFields};
pub use libtmux::{Command, CommandChain, CommandResult, CommandSummary};
#[cfg(feature = "control-mode")]
pub use libtmux::{ControlClientLimits, ControlLimits};
pub use libtmux::{DispatchLimits, OutputLimits};
pub use libtmux::{EnvironmentEntry, NewWindowOptions, Session, WindowPlacement};
pub use libtmux::{
    Error, ErrorKind, IdParseError, ListingDecodeError, ObjectKind, OptionErrorKind, ScopeError,
    ServerConfigurationErrorKind, ServerGoneKind,
};
pub use libtmux::{IndexedHooks, ReplaceMode, SparseValues};
pub use libtmux::{
    JoinOptions, Layout, LayoutSpec, PaneDirection, PaneSize, ResizeDirection, Respawn, Rotation,
    SplitDirection, SplitOptions, Window,
};
pub use libtmux::{
    OptionKind, OptionSchema, OptionScope, OptionValue, OptionValueRefusal, option_names,
    option_schema,
};
pub use libtmux::{
    PaneId, PaneTarget, ServerGeneration, ServerIdentity, SessionId, SessionName, SessionNameError,
    SessionTarget, TmuxArg, WindowId, WindowTarget, escape_format,
};
pub use libtmux::{ReleaseSuffix, ReleaseVersion, TmuxVersion, since};
#[cfg(feature = "query")]
pub use libtmux::{SessionTreeFields, WindowTreeFields};

#[cfg(feature = "unstable-fuzzing")]
#[doc(hidden)]
pub use libtmux::__fuzz_layout;
#[cfg(feature = "derive")]
pub use libtmux::Filterable;
#[cfg(feature = "control-mode")]
pub use libtmux::control;
#[cfg(feature = "plan")]
pub use libtmux::plan;
#[cfg(feature = "query")]
pub use libtmux::query;
pub use libtmux::{driver, hooks};

mod runtime;
pub use runtime::TokioRuntime;
mod server;
pub use server::{Server, ServerBuilder};

#[cfg(feature = "blocking")]
pub mod blocking;
#[cfg(feature = "test-support")]
pub mod test;
