//! Lists of owned tmux observations.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::marker::PhantomData;

use super::{
    ClientSnapshot, PaneSnapshot, SessionSnapshot, WindowSnapshot, hydrate_client_info,
    hydrate_pane_info, hydrate_session_info, hydrate_window_info,
};
use crate::formats::{FormatCodecError, FormatPlan, ListProfile, ParsedRow};
use crate::{Command, CommandChain, CommandKind, CreationNonce, Error, TmuxVersion};

mod sealed {
    pub trait Sealed {}
}

/// An owned session, window, pane or client row that both native clients can read.
///
/// Each snapshot owns its text and retains tmux's field-availability evidence.
/// A later change to tmux leaves a previously returned value intact.
///
/// # Examples
///
/// ```
/// use libtmux_core::{SessionSnapshot, Snapshot};
///
/// fn same_kind<T: Snapshot>(_: &[T]) {}
/// same_kind(&Vec::<SessionSnapshot>::new());
/// ```
pub trait Snapshot: sealed::Sealed + Sized {
    #[doc(hidden)]
    const PROFILE: ListProfile;
    #[doc(hidden)]
    const COMMAND: CommandKind;
    #[doc(hidden)]
    fn decode_rows(plan: &FormatPlan, rows: &[ParsedRow]) -> Result<Vec<Self>, FormatCodecError>;
}

macro_rules! snapshot {
    ($ty:ty, $profile:ident, $cmd:ident, $decode:ident $(, $key:ident)?) => {
        impl sealed::Sealed for $ty {}
        impl Snapshot for $ty {
            const PROFILE: ListProfile = ListProfile::$profile;
            const COMMAND: CommandKind = CommandKind::$cmd;
            fn decode_rows(plan: &FormatPlan, rows: &[ParsedRow]) -> Result<Vec<Self>, FormatCodecError> {
                let values = rows.iter().map(|row| $decode(plan, row)).collect::<Result<Vec<_>, _>>()?;
                $(
                    let mut seen = HashSet::new();
                    let mut values = values;
                    values.retain(|item| seen.insert(item.$key().clone()));
                )?
                Ok(values)
            }
        }
    };
}

snapshot!(
    SessionSnapshot,
    Sessions,
    ListSessions,
    hydrate_session_info
);
snapshot!(
    WindowSnapshot,
    Windows,
    ListWindows,
    hydrate_window_info,
    window_id
);
snapshot!(PaneSnapshot, Panes, ListPanes, hydrate_pane_info, pane_id);
snapshot!(ClientSnapshot, Clients, ListClients, hydrate_client_info);

/// A tmux listing with a version-specific format and a marker identifying its rows.
///
/// Supply fresh random bytes for each request. A mismatched row or failing
/// hook rejects the whole listing. Linked windows and their panes appear once.
///
/// # Examples
///
/// ```
/// use libtmux_core::{CreationNonce, SessionSnapshot, SnapshotList, TmuxVersion};
///
/// let recorded_nonce = [0x42; 16];
/// let version = TmuxVersion::parse_output(b"tmux 3.4\n")?;
/// let request = SnapshotList::<SessionSnapshot>::new(&version, CreationNonce::from_bytes(recorded_nonce));
/// assert!(request.decode(Some(0), b"", 0, false)?.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct SnapshotList<S> {
    version: TmuxVersion,
    plan: FormatPlan,
    marker: String,
    output: PhantomData<fn() -> S>,
}

impl<S: Snapshot> fmt::Debug for SnapshotList<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotList")
            .field("command", &S::COMMAND)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl<S: Snapshot> SnapshotList<S> {
    /// Select the columns supported by the observed tmux version.
    #[must_use]
    pub fn new(version: &TmuxVersion, nonce: CreationNonce) -> Self {
        Self {
            version: version.clone(),
            plan: FormatPlan::for_profile(S::PROFILE, version),
            marker: format!("libtmux-row:{}:", nonce.encoded()),
            output: PhantomData,
        }
    }

    /// Prepare an ordinary non-starting client and check the version in its command queue.
    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        let mut command = Command::new(S::COMMAND.to_string());
        if matches!(S::PROFILE, ListProfile::Windows | ListProfile::Panes) {
            command = command.arg("-a");
        }
        let command = command
            .arg("-F")
            .arg(format!("{}{}", self.marker, self.plan.template()));
        let (argv, _) = CommandChain::new(command).into_argv(&["-N".into()]);
        self.version.fence_ordinary_argv(argv)
    }

    /// Decode complete rows and discard a failed or late listing.
    ///
    /// # Errors
    ///
    /// Returns only status, byte counts or parser coordinates on failure.
    pub fn decode(
        &self,
        status: Option<i32>,
        stdout: &[u8],
        diagnostic_len: usize,
        late: bool,
    ) -> Result<Vec<S>, Error> {
        if status != Some(0) {
            return Err(Error::Command {
                operation: S::COMMAND,
                status,
                diagnostic_len,
                late,
            });
        }
        if late {
            return Err(Error::LateObservation {
                operation: S::COMMAND,
            });
        }
        let rows = self
            .plan
            .parse_marked_rows(stdout, self.marker.as_bytes())?;
        Ok(S::decode_rows(&self.plan, &rows)?)
    }
}
