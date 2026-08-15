//! What the tools answer with.
//!
//! Every tool returns one of these rather than a string, so the shape it
//! promises is published as an output schema and the value arrives as
//! structured content. An agent reads fields instead of parsing prose, and the
//! doc comments here become the descriptions it reads while doing it.
//!
//! Lists are wrapped in a named object rather than returned bare. The protocol
//! says structured content is an object, and a wrapper leaves somewhere to put
//! a count or a cursor later without changing a shape callers already read.

use serde::Serialize;

use crate::caller::Relation;

/// One session, as the protocol sees it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SessionView {
    /// The `$`-prefixed tmux identity.
    pub id: String,
    /// The session name, absent when tmux reported none.
    pub name: String,
    /// How many windows the session holds.
    pub windows: u32,
    /// Whether any client is attached.
    pub attached: bool,
}

/// One window, as the protocol sees it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct WindowView {
    /// The `@`-prefixed tmux identity.
    pub id: String,
    /// The session this window was reached through.
    pub session_id: String,
    /// The window's index within that session.
    pub index: i32,
    /// The window name.
    pub name: String,
    /// How many panes the window holds.
    pub panes: u32,
    /// Whether this is the session's active window.
    pub active: bool,
    /// Whether more than one session links this window.
    pub linked: bool,
}

/// One pane, as the protocol sees it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PaneView {
    /// The `%`-prefixed tmux identity.
    pub id: String,
    /// The window that contains the pane.
    pub window_id: String,
    /// The command currently running.
    pub command: Option<String>,
    /// The pane's working directory.
    pub path: Option<String>,
    /// Whether this is the window's active pane.
    pub active: bool,
    /// Whether this is the pane the MCP server itself runs in.
    ///
    /// `self` only on a confirmed match of both socket and pane id; `other`
    /// for every pane that is not, including one this crate cannot prove
    /// either way; `unknown` when the server is not running inside tmux, so
    /// the question has no answer.
    pub caller: Relation,
}

/// Every session on the server.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Sessions {
    /// The sessions, in tmux's own order.
    pub sessions: Vec<SessionView>,
}

/// Every window asked for.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Windows {
    /// The windows, in tmux's own order.
    pub windows: Vec<WindowView>,
}

/// Every pane asked for.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Panes {
    /// The panes, in tmux's own order.
    pub panes: Vec<PaneView>,
}

/// One pane inside a described window.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BranchPane {
    /// The `%`-prefixed tmux identity.
    pub id: String,
    /// The command currently running.
    pub command: Option<String>,
    /// Whether this is the window's active pane.
    pub active: bool,
}

/// One window inside a described session.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct BranchWindow {
    /// The `@`-prefixed tmux identity.
    pub id: String,
    /// The window's index within its session.
    pub index: i32,
    /// The window name.
    pub name: String,
    /// Whether this is the session's active window.
    pub active: bool,
    /// Whether more than one session links this window.
    pub linked: bool,
    /// The panes it holds.
    pub panes: Vec<BranchPane>,
}

/// One session with everything under it.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Branch {
    /// The `$`-prefixed tmux identity.
    pub id: String,
    /// The session name.
    pub name: String,
    /// Whether any client is attached.
    pub attached: bool,
    /// The windows it holds.
    pub windows: Vec<BranchWindow>,
}

/// What this process is attached to.
///
/// Answered by the `tmux://server` resource. The socket is what makes a pane
/// id mean anything: `%1` names a different pane on every tmux server, so a
/// reader comparing ids across two of these has to compare sockets first.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServerView {
    /// The socket this server talks to, as tmux reports it.
    pub socket: Option<String>,
    /// The pane this process runs in, when tmux started it in one.
    pub caller_pane: Option<String>,
    /// How many sessions are on the server.
    pub sessions: usize,
}

/// The whole hierarchy, in one answer.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Tree {
    /// Every session, with its windows and their panes.
    pub sessions: Vec<Branch>,
}

/// What a pane is showing.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Capture {
    /// The pane that was read.
    pub pane: String,
    /// The text, with lines separated by newlines.
    pub text: String,
    /// How many lines that is.
    pub lines: usize,
}

/// A pane's contents and the state a capture leaves out.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Snapshot {
    /// The pane itself.
    pub pane: PaneView,
    /// How wide it is, in columns.
    pub width: u32,
    /// How tall it is, in rows.
    pub height: u32,
    /// Where the cursor sits, counting from the left.
    pub cursor_x: Option<u32>,
    /// Where the cursor sits, counting from the top.
    pub cursor_y: Option<u32>,
    /// Whether the pane is in a mode, where keys navigate rather than type.
    pub in_mode: bool,
    /// Which mode, when it is in one.
    pub mode: Option<String>,
    /// How far it is scrolled back, when it is in copy mode.
    pub scroll_position: Option<i32>,
    /// Whether the process in it has ended.
    pub dead: bool,
    /// What the pane is showing.
    pub content: String,
    /// How many lines of it are reported.
    pub lines: usize,
    /// How many older lines were dropped to honour `max_lines`.
    pub dropped: usize,
}

/// One line of one pane that matched a search.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MatchView {
    /// The pane the line was found in.
    pub pane: String,
    /// The window that pane belongs to.
    pub window_id: String,
    /// Which line of the capture matched, counting from the top.
    pub line: usize,
    /// The line itself.
    pub text: String,
}

/// Where a pattern was found.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Matches {
    /// Every matching line, in the order the panes were read.
    pub matches: Vec<MatchView>,
    /// How many panes were read to answer this.
    pub panes_searched: usize,
    /// Whether the match ceiling was reached, so there may be more.
    pub capped: bool,
}

/// What a pane wrote since a cursor.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Since {
    /// The pane that was read.
    pub pane: String,
    /// The text, with escape sequences removed.
    pub text: String,
    /// The cursor to pass back next time.
    pub cursor: String,
    /// Whether output between the previous cursor and this text was lost.
    pub missed: bool,
    /// Whether the pane has stopped writing for good.
    pub closed: bool,
    /// Whether this is the first answer, which reports the visible screen
    /// rather than what is new.
    pub first: bool,
}

/// What a pane produced while it was watched.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Watch {
    /// The pane that was watched.
    pub pane: String,
    /// What the pane wrote, with terminal escapes left in place.
    pub output: String,
    /// How many bytes arrived.
    pub bytes: usize,
    /// Why watching stopped.
    pub stopped: String,
}

/// The value of one tmux option.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct OptionValue {
    /// The option name.
    pub name: String,
    /// Its value, absent when it has never been set at that scope.
    pub value: Option<String>,
}

/// An option that was written.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct OptionSet {
    /// The option name.
    pub name: String,
    /// The scope it was written at.
    pub scope: String,
}

/// How a wait on a channel finished.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ChannelWait {
    /// The channel that was waited on.
    pub channel: String,
    /// `signalled` or `deadline`.
    pub outcome: String,
}

/// A channel that was signalled.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ChannelSignal {
    /// The channel that was signalled.
    pub channel: String,
}

/// Keys that were sent.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Sent {
    /// The pane they were sent to.
    pub pane: String,
}

/// A pane's size after being resized.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Size {
    /// The pane that was resized.
    pub pane: String,
    /// How wide it is now, in columns.
    pub width: u32,
    /// How tall it is now, in rows.
    pub height: u32,
}

/// An object that was renamed.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Renamed {
    /// The id of what was renamed.
    pub id: String,
    /// Its new name.
    pub name: String,
}

/// An object that was destroyed.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Killed {
    /// The id of what was destroyed.
    pub id: String,
}

/// A server that was stopped.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServerKilled {
    /// Always true; the call fails rather than reporting false.
    pub killed: bool,
}
