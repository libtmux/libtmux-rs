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

/// The selected tmux server and inherited caller context.
///
/// Answered by the `tmux://server` resource. The inherited pane is launch
/// context, not a claim that it belongs to the selected socket.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServerView {
    /// The socket this server talks to, as tmux reports it.
    pub socket: Option<String>,
    /// The pane id inherited at launch, without a selected-socket claim.
    pub inherited_caller_pane: Option<String>,
    /// How many sessions are on the server.
    pub sessions: usize,
}

/// The whole hierarchy, in one answer.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Tree {
    /// Every session, with its windows and their panes.
    pub sessions: Vec<Branch>,
}

/// Whether tmux knew where the last command's output began.
///
/// Reported rather than inferred: an answer that fell back to the whole
/// screen looks exactly like a command that printed a great deal.
#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Marks {
    /// The prompt marks were there, so the text is one command's output.
    Present,
    /// tmux has the marks but this pane has none, because its shell does not
    /// emit OSC 133. fish does; bash and zsh need shell integration. The
    /// visible screen came back instead -- not the history, which would
    /// answer a request for one command with everything the pane ever wrote.
    Absent,
    /// This tmux predates `capture-pane -F`, which arrived in 3.7. The
    /// visible screen came back instead.
    Unsupported,
    /// The caller did not ask for the last command, so nothing was looked up.
    NotAsked,
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
    /// Whether the text is one command's output, and why it is not.
    pub marks: Marks,
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

/// Every background command this server holds.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct JobList {
    /// The jobs, newest first.
    pub jobs: Vec<crate::jobs::JobView>,
}

/// A background command that is no longer retained.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct JobForgotten {
    /// The job that was forgotten.
    pub job: String,
    /// The pane associated with the job.
    pub pane: String,
}

/// One tmux server found on this machine.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServerListing {
    /// The socket path, which is what names the server.
    pub socket: String,
    /// The socket's bare name, as `-L` would take it.
    pub name: Option<String>,
    /// How many sessions it holds, when it answered.
    pub sessions: Option<u32>,
    /// Whether this is the server these tools are bound to.
    pub current: bool,
    /// Why the server could not be described, when it could not.
    pub unreachable: Option<String>,
}

/// Every tmux server this machine appears to be running.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ServerListings {
    /// The servers, the bound one first.
    pub servers: Vec<ServerListing>,
    /// The directories that were searched.
    pub searched: Vec<String>,
}

/// What a tmux format expanded to.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Formatted {
    /// The format as it was given.
    pub format: String,
    /// What tmux expanded it to.
    pub value: String,
    /// The pane it was expanded against, when one was named.
    pub pane: Option<String>,
}

/// One tmux environment entry.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EnvironmentEntry {
    /// The variable name.
    pub name: String,
    /// Its value, absent when the variable is marked for removal.
    pub value: Option<String>,
}

/// A tmux environment, server-wide or for one session.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Environment {
    /// The entries, in tmux's own order.
    pub entries: Vec<EnvironmentEntry>,
    /// The session the environment belongs to, or absent for the server's.
    pub session: Option<String>,
}

/// A variable that was written.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EnvironmentSet {
    /// The variable name.
    pub name: String,
    /// The session it was written for, or absent for the server's.
    pub session: Option<String>,
    /// Whether the variable was removed rather than set.
    pub removed: bool,
}

/// One tmux hook.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Hook {
    /// The hook name, such as `pane-exited`.
    pub name: String,
    /// Its index, when the hook is an array.
    pub index: Option<u32>,
    /// The command tmux runs.
    pub command: String,
}

/// The hooks set at one scope.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Hooks {
    /// The hooks, in tmux's own order.
    pub hooks: Vec<Hook>,
}

/// A pane that is now being piped somewhere.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Piped {
    /// The pane that was piped.
    pub pane: String,
    /// Whether piping is now on.
    pub piping: bool,
}

/// A window whose layout was set.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Layout {
    /// The window that was arranged.
    pub window: String,
    /// The layout it now has, in tmux's own syntax.
    pub layout: String,
}

/// A pane that was cleared, respawned, or retitled.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct PaneChanged {
    /// The pane that changed.
    pub pane: String,
}

/// Text that was pasted into a pane.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Pasted {
    /// The pane it went into.
    pub pane: String,
    /// How many bytes were delivered.
    pub bytes: usize,
}

/// One window that has produced output.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Busy {
    /// The `@`-prefixed window identity.
    pub id: String,
    /// The session it was reached through.
    pub session_id: String,
    /// The window name.
    pub name: String,
    /// When it last wrote, in seconds since the Unix epoch.
    pub activity: i64,
    /// How many panes it holds.
    pub panes: u32,
    /// Whether it is its session's active window.
    pub active: bool,
}

/// Which windows have written, and when to ask from next time.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct Changes {
    /// The windows that wrote, most recent first.
    pub windows: Vec<Busy>,
    /// Pass this back as `since` to hear only about what happens next.
    pub now: i64,
    /// How many windows were considered.
    pub windows_checked: usize,
}
