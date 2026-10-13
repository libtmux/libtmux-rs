//! tmux values, decoding, and requests that need only the standard library.
//!
//! ```
//! use libtmux::{CapturePane, PaneId};
//!
//! let pane: PaneId = "%003".parse()?;
//! let request = CapturePane::new(pane).history(20);
//! assert_eq!(request.args(), ["capture-pane", "-p", "-t", "%3", "-S", "-20"]);
//! # Ok::<(), libtmux::IdParseError>(())
//! ```

mod id;
mod text;

use std::fmt;

pub use id::{IdParseError, PaneId};
pub use text::TmuxText;

/// A capture request that can be constructed without a runtime or tmux server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturePane {
    pane: PaneId,
    history: u16,
}

impl CapturePane {
    /// Capture the pane's visible screen.
    pub const fn new(pane: PaneId) -> Self {
        Self { pane, history: 0 }
    }

    /// Include up to this many lines before the visible screen.
    #[must_use]
    pub const fn history(mut self, lines: u16) -> Self {
        self.history = lines;
        self
    }

    /// Render separate argv tokens; these are not a shell command.
    pub fn args(&self) -> Vec<String> {
        vec![
            "capture-pane".into(),
            "-p".into(),
            "-t".into(),
            self.pane.to_string(),
            "-S".into(),
            format!("-{}", self.history),
        ]
    }
}

/// Detached pane data, usable without a Tokio runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneSnapshot {
    /// Native tmux pane identifier.
    pub id: PaneId,
    /// Screen width in cells.
    pub width: u16,
    /// Screen height in cells.
    pub height: u16,
}

/// Arguments for a listing that [`decode_panes`] can read.
pub const LIST_PANES: [&str; 4] = [
    "list-panes",
    "-a",
    "-F",
    "#{pane_id}\t#{pane_width}\t#{pane_height}",
];

/// A malformed pane listing row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeError {
    line: usize,
}

impl DecodeError {
    /// The one-based row that could not be decoded.
    pub const fn line(&self) -> usize {
        self.line
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid pane ID or dimensions on row {}",
            self.line
        )
    }
}

impl std::error::Error for DecodeError {}

/// Decode the output of [`LIST_PANES`] without invoking tmux.
///
/// # Errors
/// Returns [`DecodeError`] for invalid IDs, dimensions, or field counts.
pub fn decode_panes(bytes: &[u8]) -> Result<Vec<PaneSnapshot>, DecodeError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut panes = Vec::new();
    let rows = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    for (index, row) in rows.split(|byte| *byte == b'\n').enumerate() {
        let error = || DecodeError { line: index + 1 };
        let text = std::str::from_utf8(row).map_err(|_| error())?;
        let fields: Vec<_> = text.split('\t').collect();
        let [id, width, height] = fields.as_slice() else {
            return Err(error());
        };
        panes.push(PaneSnapshot {
            id: id.parse().map_err(|_| error())?,
            width: width.parse().map_err(|_| error())?,
            height: height.parse().map_err(|_| error())?,
        });
    }
    Ok(panes)
}
