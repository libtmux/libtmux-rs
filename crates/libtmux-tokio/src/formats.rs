//! Byte-preserving tmux text values.

use crate::version::{ReleaseSuffix, ReleaseVersion};

// The release the catalog was recorded against. Introduction floors at or
// below the supported minimum are history, not live gates.
const CATALOG_BASELINE: ReleaseVersion = ReleaseVersion::new(3, 2, ReleaseSuffix::PATCH_A);

#[cfg(feature = "unstable-fuzzing")]
mod fuzz;
mod plan;
mod row;
mod text;

#[cfg(feature = "unstable-fuzzing")]
pub use fuzz::__fuzz_format_rows;
pub(crate) use plan::{FormatPlan, PlanFieldState, PlanPurpose, TransportDialect};
#[cfg(test)]
use plan::{PlanVersion, for_profile_selection_test};
#[cfg(test)]
pub(crate) use row::{FIELD_SEPARATOR, decode_ascii};
pub(crate) use row::{FormatCodecError, FormatCodecErrorKind, ParsedRow, ParsedSlot, decode_text};
pub(crate) use row::{FormatCodecPhase, split_quoted_rows};
#[cfg(test)]
use row::{QUOTE_SHELL_SPECIALS, encode_like_tmux};
pub use text::TmuxText;

/// Decoder applied to a parsed format slot.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecoderKind {
    /// Accept only ASCII bytes.
    Ascii,
    /// Preserve arbitrary non-NUL bytes.
    Text,
    /// Parse a canonical tmux boolean.
    Bool,
    /// Parse a canonical unsigned 8-bit integer.
    U8,
    /// Parse a canonical unsigned 32-bit integer.
    U32,
    /// Parse a canonical unsigned 64-bit integer.
    U64,
    /// Parse a canonical signed 32-bit integer.
    I32,
    /// Parse canonical signed Unix seconds.
    Timestamp,
    /// Parse a native Session ID.
    SessionId,
    /// Parse a native Window ID.
    WindowId,
    /// Parse a native Pane ID.
    PaneId,
    /// Parse a bounded pane progress percentage.
    PaneProgress,
    /// Parse a closed pane progress state.
    PaneProgressState,
}

/// Tmux object that semantically owns a format field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticOwner {
    /// Server-owned field.
    Server,
    /// Caller-owned list-row field.
    ListRow,
    /// Global mode formatting field.
    Mode,
    /// Paste-buffer field.
    Buffer,
    /// Client-owned field.
    Client,
    /// Client attachment field.
    ClientAttachment,
    /// Client-specific Window view field.
    ClientWindowView,
    /// Session-owned field.
    Session,
    /// Window-owned field.
    Window,
    /// Pane-owned field.
    Pane,
    /// Window-link-owned field.
    WindowLink,
    /// Command metadata field.
    Command,
    /// Configuration metadata field.
    Config,
    /// Copy-mode field.
    CopyMode,
}

/// Tmux context required to resolve a format field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RequiredContext {
    /// No object context.
    None,
    /// Original list format type.
    FormatType,
    /// Caller-owned list row.
    ListRow,
    /// Paste-buffer context.
    Buffer,
    /// Client context.
    Client,
    /// Session context.
    Session,
    /// Window context.
    Window,
    /// Pane context.
    Pane,
    /// Window-link context.
    WindowLink,
    /// Command context.
    Command,
    /// Configuration context.
    Config,
    /// Copy-mode context.
    CopyMode,
}

/// Intrinsic snapshot placement for a modeled field.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InfoPlacement {
    /// Metadata is available only to explicit projections.
    CatalogOnly,
    /// Field belongs to Session snapshots.
    SessionInfo,
    /// Field belongs to Window snapshots.
    WindowInfo,
    /// Field belongs to Pane snapshots.
    PaneInfo,
    /// Field belongs to Client snapshots.
    ClientInfo,
}

/// Which list commands can resolve a field.
///
/// A set of [`ListProfile`], and deliberately not named for it: the two sat
/// one letter apart in this file and a reader took the busy one for the
/// catalogue-only one.
#[allow(
    dead_code,
    reason = "catalog-only metadata is verified by the checked catalog table"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProfileSet(u8);

#[allow(
    dead_code,
    reason = "catalog-only metadata is verified by the checked catalog table"
)]
impl ProfileSet {
    const SESSIONS: u8 = 1;
    const WINDOWS: u8 = 1 << 1;
    const PANES: u8 = 1 << 2;
    const CLIENTS: u8 = 1 << 3;

    /// Admit all four list commands.
    const fn all() -> Self {
        Self(Self::SESSIONS | Self::WINDOWS | Self::PANES | Self::CLIENTS)
    }

    /// Admit only Client listings.
    const fn clients() -> Self {
        Self(Self::CLIENTS)
    }

    /// Admit no list command.
    const fn none() -> Self {
        Self(0)
    }

    /// Return whether this set admits a profile.
    pub(crate) const fn contains(self, profile: ListProfile) -> bool {
        let bit = match profile {
            ListProfile::Sessions => Self::SESSIONS,
            ListProfile::Windows => Self::WINDOWS,
            ListProfile::Panes => Self::PANES,
            ListProfile::Clients => Self::CLIENTS,
        };
        self.0 & bit != 0
    }
}

/// Policy for an empty supported field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmptyPolicy {
    /// Reject an empty field.
    Required,
    /// Interpret an empty field as absent.
    Absent,
    /// Pass an empty field to its decoder.
    Available,
}

/// Trusted static metadata for one tmux format field.
#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FormatDescriptor {
    /// Trusted tmux format name.
    name: &'static str,
    /// Object that semantically owns the field.
    owner: SemanticOwner,
    /// Context tmux needs to resolve the field.
    required_context: RequiredContext,
    /// List commands that can resolve the field.
    profiles: ProfileSet,
    /// First numbered tmux release that provides the field.
    minimum_release: ReleaseVersion,
    /// Primitive byte decoder for the field.
    decoder: DecoderKind,
    /// Later snapshot policy for an empty supported field.
    empty_policy: EmptyPolicy,
    /// Intrinsic snapshot placement.
    placement: InfoPlacement,
}

#[allow(
    dead_code,
    reason = "modelled and tested; only a projection of it is hydrated today"
)]
impl FormatDescriptor {
    /// Return the trusted tmux format name.
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// Return the semantic owner.
    pub(crate) const fn owner(&self) -> SemanticOwner {
        self.owner
    }

    /// Return the required tmux context.
    pub(crate) const fn required_context(&self) -> RequiredContext {
        self.required_context
    }

    /// Return the admitted list profiles.
    pub(crate) const fn profiles(&self) -> ProfileSet {
        self.profiles
    }

    /// Return the first tmux release that provides this field.
    pub(crate) const fn minimum_release(&self) -> ReleaseVersion {
        self.minimum_release
    }

    /// Return the primitive decoder.
    pub(crate) const fn decoder(&self) -> DecoderKind {
        self.decoder
    }

    /// Return the empty-field policy.
    pub(crate) const fn empty_policy(&self) -> EmptyPolicy {
        self.empty_policy
    }

    /// Return the intrinsic snapshot placement.
    pub(crate) const fn placement(&self) -> InfoPlacement {
        self.placement
    }

    /// Construct trusted baseline-era metadata for codec tests.
    #[cfg(test)]
    pub(crate) const fn for_codec_test(name: &'static str, decoder: DecoderKind) -> Self {
        Self {
            name,
            owner: SemanticOwner::Session,
            required_context: RequiredContext::Session,
            profiles: ProfileSet::all(),
            minimum_release: CATALOG_BASELINE,
            decoder,
            empty_policy: EmptyPolicy::Available,
            placement: InfoPlacement::CatalogOnly,
        }
    }
}

pub(crate) use libtmux_core::format_catalog;

macro_rules! catalog_profiles {
    (All) => {
        ProfileSet::all()
    };
    (Clients) => {
        ProfileSet::clients()
    };
    (None) => {
        ProfileSet::none()
    };
}

macro_rules! catalog_floor {
    (V3_2A) => {
        CATALOG_BASELINE
    };
    (V3_3) => {
        ReleaseVersion::new(3, 3, ReleaseSuffix::FINAL)
    };
    (V3_4) => {
        ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL)
    };
    (V3_7) => {
        ReleaseVersion::new(3, 7, ReleaseSuffix::FINAL)
    };
}

macro_rules! define_catalog_descriptor {
    ($static:ident, $name:literal, $owner:ident, $context:ident, $profiles:ident, $floor:ident, $decoder:ident, $empty:ident, $placement:ident) => {
        #[allow(
            dead_code,
            reason = "modelled and tested; only a projection of it is hydrated today"
        )]
        pub(crate) static $static: FormatDescriptor = FormatDescriptor {
            name: $name,
            owner: SemanticOwner::$owner,
            required_context: RequiredContext::$context,
            profiles: catalog_profiles!($profiles),
            minimum_release: catalog_floor!($floor),
            decoder: DecoderKind::$decoder,
            empty_policy: EmptyPolicy::$empty,
            placement: InfoPlacement::$placement,
        };
    };
}

macro_rules! define_format_catalog {
    (
        SessionInfo {
            target: $session_target:literal;
            baseline: ($session_static:ident, $session_field:ident, $session_name:literal, $session_owner:ident, $session_context:ident, $session_profiles:ident, $session_floor:ident, $session_decoder:ident, $session_empty:ident);
            supplements: [$(($session_s_static:ident, $session_s_field:ident, $session_s_name:literal, $session_s_owner:ident, $session_s_context:ident, $session_s_profiles:ident, $session_s_floor:ident, $session_s_decoder:ident, $session_s_empty:ident)),* $(,)?];
        }
        WindowInfo {
            target: $window_target:literal;
            baseline: ($window_static:ident, $window_field:ident, $window_name:literal, $window_owner:ident, $window_context:ident, $window_profiles:ident, $window_floor:ident, $window_decoder:ident, $window_empty:ident);
            supplements: [$(($window_s_static:ident, $window_s_field:ident, $window_s_name:literal, $window_s_owner:ident, $window_s_context:ident, $window_s_profiles:ident, $window_s_floor:ident, $window_s_decoder:ident, $window_s_empty:ident)),* $(,)?];
        }
        PaneInfo {
            target: $pane_target:literal;
            baseline: ($pane_static:ident, $pane_field:ident, $pane_name:literal, $pane_owner:ident, $pane_context:ident, $pane_profiles:ident, $pane_floor:ident, $pane_decoder:ident, $pane_empty:ident);
            supplements: [$(($pane_s_static:ident, $pane_s_field:ident, $pane_s_name:literal, $pane_s_owner:ident, $pane_s_context:ident, $pane_s_profiles:ident, $pane_s_floor:ident, $pane_s_decoder:ident, $pane_s_empty:ident)),* $(,)?];
        }
        ClientInfo {
            target: $client_target:literal;
            baseline: ($client_static:ident, $client_field:ident, $client_name:literal, $client_owner:ident, $client_context:ident, $client_profiles:ident, $client_floor:ident, $client_decoder:ident, $client_empty:ident);
            supplements: [$(($client_s_static:ident, $client_s_field:ident, $client_s_name:literal, $client_s_owner:ident, $client_s_context:ident, $client_s_profiles:ident, $client_s_floor:ident, $client_s_decoder:ident, $client_s_empty:ident)),* $(,)?];
        }
        CatalogOnly {
            fields: [$(($catalog_static:ident, $catalog_field:ident, $catalog_name:literal, $catalog_owner:ident, $catalog_context:ident, $catalog_profiles:ident, $catalog_floor:ident, $catalog_decoder:ident, $catalog_empty:ident)),* $(,)?];
        }
    ) => {
        define_catalog_descriptor!($session_static, $session_name, $session_owner, $session_context, $session_profiles, $session_floor, $session_decoder, $session_empty, SessionInfo);
        $(define_catalog_descriptor!($session_s_static, $session_s_name, $session_s_owner, $session_s_context, $session_s_profiles, $session_s_floor, $session_s_decoder, $session_s_empty, SessionInfo);)*
        define_catalog_descriptor!($window_static, $window_name, $window_owner, $window_context, $window_profiles, $window_floor, $window_decoder, $window_empty, WindowInfo);
        $(define_catalog_descriptor!($window_s_static, $window_s_name, $window_s_owner, $window_s_context, $window_s_profiles, $window_s_floor, $window_s_decoder, $window_s_empty, WindowInfo);)*
        define_catalog_descriptor!($pane_static, $pane_name, $pane_owner, $pane_context, $pane_profiles, $pane_floor, $pane_decoder, $pane_empty, PaneInfo);
        $(define_catalog_descriptor!($pane_s_static, $pane_s_name, $pane_s_owner, $pane_s_context, $pane_s_profiles, $pane_s_floor, $pane_s_decoder, $pane_s_empty, PaneInfo);)*
        define_catalog_descriptor!($client_static, $client_name, $client_owner, $client_context, $client_profiles, $client_floor, $client_decoder, $client_empty, ClientInfo);
        $(define_catalog_descriptor!($client_s_static, $client_s_name, $client_s_owner, $client_s_context, $client_s_profiles, $client_s_floor, $client_s_decoder, $client_s_empty, ClientInfo);)*
        $(define_catalog_descriptor!($catalog_static, $catalog_name, $catalog_owner, $catalog_context, $catalog_profiles, $catalog_floor, $catalog_decoder, $catalog_empty, CatalogOnly);)*

        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static SESSION_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$session_static, $(&$session_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static SESSION_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$session_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static WINDOW_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$window_static, $(&$window_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static WINDOW_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$window_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static PANE_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$pane_static, $(&$pane_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static PANE_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$pane_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static CLIENT_INFO_DESCRIPTORS: &[&FormatDescriptor] = &[&$client_static, $(&$client_s_static),*];
        #[allow(dead_code, reason = "modelled and tested; only a projection of it is hydrated today")]
        pub(crate) static CLIENT_INFO_SUPPLEMENTS: &[&FormatDescriptor] = &[$(&$client_s_static),*];

        #[allow(dead_code, reason = "catalog-only metadata is verified by the checked catalog table")]
        static GROUPED_CATALOG: &[&FormatDescriptor] = &[
            &$session_static, $(&$session_s_static),*,
            &$window_static, $(&$window_s_static),*,
            &$pane_static, $(&$pane_s_static),*,
            &$client_static, $(&$client_s_static),*,
            $(&$catalog_static),*
        ];
    };
}
format_catalog!(define_format_catalog);

/// Tmux list command profile represented by a format plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListProfile {
    /// Session rows.
    Sessions,
    /// Window rows.
    Windows,
    /// Pane rows.
    Panes,
    /// Client rows.
    Clients,
}
#[cfg(test)]
mod tests;
