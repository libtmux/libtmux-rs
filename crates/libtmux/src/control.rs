//! Watching a tmux server over control mode.
//!
//! Every other API in this crate spawns a tmux process per command. Control
//! mode opens one connection and keeps it: commands go down it, and tmux
//! reports what happens on the server as it happens. That is the difference
//! between asking tmux what is true and being told when it changes.
//!
//! Sending and watching are separate handles, so a task can act on what it
//! sees without waiting its turn:
//!
//! ```no_run
//! # async fn watch(server: &libtmux::Server, id: &libtmux::SessionId) -> Result<(), libtmux::Error> {
//! use libtmux::control::{ControlMode, Event};
//!
//! let (commands, mut events) = ControlMode::attach(server, id).await?.split();
//!
//! // Commands travel down the connection, so none of these spawn a process.
//! let listed = commands.send(libtmux::Command::new("list-windows")).await?;
//! assert!(listed.succeeded());
//!
//! while let Some(event) = events.next_event().await {
//!     match event {
//!         Event::Output { pane, bytes } => println!("{pane}: {} bytes", bytes.len()),
//!         Event::Exit => break,
//!         // Reacting to an event by sending a command is the whole point,
//!         // and works here because the sender is not borrowed by the loop.
//!         Event::SessionChanged { .. } => {
//!             commands.send(libtmux::Command::new("list-panes")).await?;
//!         }
//!         other => println!("{other:?}"),
//!     }
//! }
//!
//! // The stream ending says the connection is over; this says why.
//! events.shutdown().await
//! # }
//! ```

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{Command, Error, PaneId, Server, SessionId, TmuxText};

/// Something tmux reported that no command asked for.
///
/// Control mode names many notifications and adds more between releases, so
/// [`Event::Other`] keeps an unrecognized one rather than dropping it. Its
/// name is the tmux notification without the leading `%`.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A pane produced output.
    ///
    /// The bytes are exactly what the pane wrote. tmux escapes only what
    /// would break the line protocol -- bytes below `0x20`, and backslash --
    /// so everything above `0x7f` arrives literally and the line as a whole
    /// is not necessarily UTF-8. That is why this is bytes.
    Output {
        /// The pane that produced it.
        pane: PaneId,
        /// The output bytes.
        bytes: Vec<u8>,
    },
    /// The attached session changed.
    SessionChanged {
        /// The session now attached.
        session: SessionId,
    },
    /// The server is going away, so no further events will arrive.
    Exit,
    /// A notification this crate does not model.
    Other {
        /// The notification name, without its `%`.
        name: String,
        /// The rest of the line.
        rest: TmuxText,
    },
}

/// The outcome of one command sent over control mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockResult {
    number: u64,
    succeeded: bool,
    output: Vec<TmuxText>,
}

impl BlockResult {
    /// Return the block number tmux assigned.
    ///
    /// tmux assigns this, and correlation uses it rather than counting
    /// commands: a command that fails early can leave a caller waiting
    /// forever for a block tmux will never send.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Report whether tmux closed the block with `%end` rather than `%error`.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.succeeded
    }

    /// Return the lines tmux printed inside the block.
    #[must_use]
    pub fn output(&self) -> &[TmuxText] {
        &self.output
    }
}

/// One control-mode connection to a tmux server.
///
/// Sending and receiving are separate handles, reachable through [`split`].
/// That is not decoration: the point of control mode is to act on what you
/// observe, and a single object would need `&mut` for both, so a task awaiting
/// an event could never send the command that event implies.
///
/// [`split`]: ControlMode::split
#[derive(Debug)]
pub struct ControlMode {
    sender: ControlSender,
    events: ControlEvents,
}

impl ControlMode {
    /// Attach to a session in control mode.
    ///
    /// When this returns, tmux has the client attached: anything that changes
    /// the server afterwards is reported. Returning as soon as the process
    /// started would look the same and lose every notification racing the
    /// attach, which is the hardest kind of bug to see.
    ///
    /// # Errors
    ///
    /// Returns an error when tmux cannot be started, does not give the crate
    /// the pipes it asked for, or exits before attaching -- which is what a
    /// session that is already gone looks like.
    pub async fn attach(server: &Server, session: &SessionId) -> Result<Self, Error> {
        let mut command = tokio::process::Command::new(server.tmux_executable());
        command
            .arg("-S")
            .arg(server.socket_path())
            .arg("-C")
            .arg("attach")
            .arg("-t")
            .arg(session.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(Error::control_mode)?;
        let stdin = child.stdin.take().ok_or_else(Error::control_mode_pipes)?;
        let stdout = child.stdout.take().ok_or_else(Error::control_mode_pipes)?;

        let (commands, queue) = mpsc::channel(COMMAND_QUEUE);
        let (events, received) = mpsc::channel(EVENT_QUEUE);
        let (stop, stopped) = watch::channel(());
        let mut connection = Connection {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            line: Vec::new(),
            commands: queue,
            events,
            stopped,
            awaiting: VecDeque::new(),
        };

        // tmux answers the attach with a block of its own. Waiting for it here
        // is what makes the guarantee above true, and it costs nothing: the
        // caller was awaiting this call anyway.
        if !connection.discard_opening_block().await? {
            return Err(Error::control_mode_closed());
        }

        Ok(Self {
            sender: ControlSender { commands },
            events: ControlEvents {
                events: received,
                stop,
                connection: tokio::spawn(connection.run()),
            },
        })
    }

    /// Separate the two halves so they can be used at the same time.
    #[must_use]
    pub fn split(self) -> (ControlSender, ControlEvents) {
        (self.sender, self.events)
    }

    /// Send one command and wait for its result block.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be written as a control-mode
    /// line, or the connection has closed.
    pub async fn send(&self, command: Command) -> Result<BlockResult, Error> {
        self.sender.send(command).await
    }

    /// Return the next notification, or `None` once the connection closes.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.next_event().await
    }

    /// Close the connection and report how it ended.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection failed before it was closed.
    pub async fn shutdown(self) -> Result<(), Error> {
        drop(self.sender);
        self.events.shutdown().await
    }
}

/// Sends commands down a control-mode connection.
///
/// Cheap to clone, and every method takes `&self`, so several tasks can issue
/// commands while another watches events.
#[derive(Clone, Debug)]
pub struct ControlSender {
    commands: mpsc::Sender<Request>,
}

impl ControlSender {
    /// Send one command and wait for its result block.
    ///
    /// A block that tmux closed with `%error` is a result, not an error: it is
    /// reported through [`BlockResult::succeeded`], the same way the process
    /// API keeps a nonzero exit status as data.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be written as a control-mode
    /// line, or the connection has closed.
    pub async fn send(&self, command: Command) -> Result<BlockResult, Error> {
        let line = command
            .control_mode_line()
            .ok_or_else(Error::control_mode_unrepresentable)?;
        let (result, answer) = oneshot::channel();

        self.commands
            .send(Request { line, result })
            .await
            .map_err(|_| Error::control_mode_closed())?;

        answer.await.map_err(|_| Error::control_mode_closed())?
    }

    /// Report whether the connection has closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }
}

/// Receives what tmux reports without being asked.
///
/// This is a [`Stream`], so it composes with `select!`, timeouts, and the rest
/// of the async ecosystem rather than demanding a loop of its own.
///
/// Events are buffered, and a consumer that stops reading eventually stops the
/// connection reading from tmux, which is the backpressure tmux already
/// expects from a slow client. Nothing is dropped; commands wait instead. Drop
/// this handle to opt out of events entirely and the connection runs on.
#[derive(Debug)]
pub struct ControlEvents {
    events: mpsc::Receiver<Event>,
    /// Ends the connection when this handle asks, or when it is dropped.
    stop: watch::Sender<()>,
    connection: tokio::task::JoinHandle<Result<(), Error>>,
}

impl ControlEvents {
    /// Return the next notification, or `None` once the connection closes.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// End the connection and report how it went.
    ///
    /// The stream running out says only that the connection is over. This says
    /// why, which is the difference between a session that ended and a pipe
    /// that broke. It ends the connection outright rather than waiting for the
    /// senders, so it is the same call whether the connection is still healthy
    /// or tmux hung up an hour ago.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection failed before it was closed.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        let _ = self.stop.send(());
        // Draining releases a connection that is parked handing over an event,
        // so it reaches its own shutdown rather than waiting for a reader that
        // is not coming back.
        self.events.close();
        while self.events.recv().await.is_some() {}

        self.connection
            .await
            .map_err(|_| Error::control_mode_closed())?
    }
}

impl Stream for ControlEvents {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Event>> {
        self.events.poll_recv(context)
    }
}

/// How many commands may queue before a sender waits.
const COMMAND_QUEUE: usize = 16;

/// How many events may buffer before the connection stops reading tmux.
const EVENT_QUEUE: usize = 256;

/// What one pane writes, as it writes it.
///
/// Built by [`crate::Pane::stream_output`]. This is a [`Stream`] of the bytes
/// that pane produced, in order, with everything the connection reports about
/// other panes filtered out.
///
/// Each of these owns a control-mode connection, so a caller watching many
/// panes at once is better served by [`ControlEvents`] and one connection.
#[derive(Debug)]
pub struct PaneOutput {
    pane: PaneId,
    events: ControlEvents,
}

impl PaneOutput {
    pub(crate) const fn new(pane: PaneId, events: ControlEvents) -> Self {
        Self { pane, events }
    }

    /// Return the pane being watched.
    #[must_use]
    pub const fn pane(&self) -> &PaneId {
        &self.pane
    }

    /// Return the next chunk this pane wrote, or `None` once it stops.
    ///
    /// A chunk is what tmux chose to report at once, which is not a line and
    /// not a fixed size. Callers wanting lines should buffer.
    pub async fn next_chunk(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.events.next_event().await? {
                Event::Output { pane, bytes } if pane == self.pane => return Some(bytes),
                Event::Exit => return None,
                _ => {}
            }
        }
    }

    /// End the connection and report how it went.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection failed before it was closed.
    pub async fn shutdown(self) -> Result<(), Error> {
        self.events.shutdown().await
    }
}

impl Stream for PaneOutput {
    type Item = Vec<u8>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Vec<u8>>> {
        loop {
            match std::task::ready!(self.events.events.poll_recv(context)) {
                Some(Event::Output { pane, bytes }) if pane == self.pane => {
                    return Poll::Ready(Some(bytes));
                }
                Some(Event::Exit) | None => return Poll::Ready(None),
                Some(_) => {}
            }
        }
    }
}

/// One command waiting for its result block.
#[derive(Debug)]
struct Request {
    line: String,
    result: oneshot::Sender<Result<BlockResult, Error>>,
}

/// What one turn of the connection loop found to do.
enum Step {
    Read(Result<Option<Line>, Error>),
    Send(Option<Request>),
    /// The watching half asked to stop, or went away.
    Unwatched {
        asked: bool,
    },
}

/// The task that owns the pipes and multiplexes both directions.
struct Connection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Bytes of a line that is not complete yet.
    ///
    /// This outlives one read because a cancelled read leaves what it got
    /// here, and the next read continues from it.
    line: Vec<u8>,
    commands: mpsc::Receiver<Request>,
    events: mpsc::Sender<Event>,
    /// Resolves when the watching half asks to stop, or is dropped.
    stopped: watch::Receiver<()>,
    /// Commands whose result block has not arrived yet.
    ///
    /// tmux answers in order and blocks do not nest, so the front of this
    /// queue owns the next block that completes.
    awaiting: VecDeque<oneshot::Sender<Result<BlockResult, Error>>>,
}

impl Connection {
    async fn run(mut self) -> Result<(), Error> {
        let outcome = self.serve().await;

        // Whatever is still waiting will never be answered.
        while let Some(result) = self.awaiting.pop_front() {
            let _ = result.send(Err(Error::control_mode_closed()));
        }
        drop(self.stdin);
        let _ = self.child.wait().await;

        outcome
    }

    async fn serve(&mut self) -> Result<(), Error> {
        // The connection outlives either half on its own: a caller who only
        // watches drops the sender, and a caller who only sends drops the
        // events. It ends when both are gone, when the watcher asks, or when
        // tmux hangs up.
        let mut sending = true;
        let mut watching = true;

        while sending || watching {
            // Unbiased on purpose. Reading first would starve commands under
            // a busy pane, and ordering is the queue's job, not the poll
            // order's.
            let step = tokio::select! {
                line = read_line(&mut self.stdout, &mut self.line) => Step::Read(line),
                request = self.commands.recv(), if sending => Step::Send(request),
                asked = self.stopped.changed(), if watching => Step::Unwatched {
                    asked: asked.is_ok(),
                },
            };

            match step {
                Step::Read(Err(error)) => return Err(error),
                // tmux hung up, or the watcher asked to stop. Either ends the
                // connection whatever the other half is doing.
                Step::Read(Ok(None)) | Step::Unwatched { asked: true } => return Ok(()),
                Step::Read(Ok(Some(line))) => {
                    if !self.dispatch(line).await? {
                        return Ok(());
                    }
                }
                Step::Send(Some(request)) => {
                    if let Err(error) = write_line(&mut self.stdin, &request.line).await {
                        let _ = request.result.send(Err(Error::control_mode_closed()));
                        return Err(error);
                    }
                    self.awaiting.push_back(request.result);
                }
                // Every sender is gone, so no further commands can arrive.
                Step::Send(None) => sending = false,
                // The watching handle was dropped rather than asked to stop,
                // which leaves any sender still working.
                Step::Unwatched { asked: false } => watching = false,
            }
        }

        Ok(())
    }

    /// Consume the block tmux answers an attach with.
    ///
    /// tmux writes this once the client is attached, before it has read
    /// anything from this end, so it replies to nothing. Correlation is by
    /// arrival order, and leaving this block to the serving loop would hand it
    /// to the first command's caller as that command's result -- an empty
    /// success, whatever the command was.
    ///
    /// Reports whether the connection survived to be served.
    async fn discard_opening_block(&mut self) -> Result<bool, Error> {
        loop {
            match read_line(&mut self.stdout, &mut self.line).await? {
                Some(Line::BlockStart(number)) => {
                    self.read_block(number).await?;
                    return Ok(true);
                }
                Some(Line::Event(Event::Exit)) => {
                    self.report(Event::Exit).await;
                    return Ok(false);
                }
                Some(Line::Event(event)) => self.report(event).await,
                Some(Line::Text(_) | Line::BlockEnd { .. }) => {}
                None => return Ok(false),
            }
        }
    }

    /// Act on one protocol line, reporting whether to keep reading.
    async fn dispatch(&mut self, line: Line) -> Result<bool, Error> {
        match line {
            Line::BlockStart(number) => {
                let block = self.read_block(number).await?;
                if let Some(result) = self.awaiting.pop_front() {
                    let _ = result.send(Ok(block));
                }
                Ok(true)
            }
            Line::Event(Event::Exit) => {
                self.report(Event::Exit).await;
                Ok(false)
            }
            Line::Event(event) => {
                self.report(event).await;
                Ok(true)
            }
            // A block terminator with no block open, or output outside one.
            Line::Text(_) | Line::BlockEnd { .. } => Ok(true),
        }
    }

    /// Hand an event to the receiver, if one is still listening.
    ///
    /// A receiver that has gone away is not a reason to stop: commands may
    /// still be in flight, and a caller who only sends is a valid caller.
    async fn report(&self, event: Event) {
        let _ = self.events.send(event).await;
    }

    /// Read to the end of a block that has already begun.
    async fn read_block(&mut self, number: u64) -> Result<BlockResult, Error> {
        let mut output = Vec::new();
        loop {
            match read_line(&mut self.stdout, &mut self.line).await? {
                Some(Line::BlockEnd {
                    number: end,
                    succeeded,
                }) if end == number => {
                    return Ok(BlockResult {
                        number,
                        succeeded,
                        output,
                    });
                }
                Some(Line::Text(text)) => output.push(text),
                Some(Line::Event(event)) => self.report(event).await,
                // Blocks do not nest, so anything else here is not this one.
                Some(Line::BlockStart(_) | Line::BlockEnd { .. }) => {}
                None => return Err(Error::control_mode_closed()),
            }
        }
    }
}

/// Read and classify one protocol line.
///
/// `pending` carries a line across calls. `read_until` appends what it read
/// before it was cancelled, which is what makes this usable in `select!` --
/// `read_line` would lose those bytes, and would also reject the pane output
/// that is not UTF-8.
async fn read_line(
    stdout: &mut BufReader<ChildStdout>,
    pending: &mut Vec<u8>,
) -> Result<Option<Line>, Error> {
    let read = stdout
        .read_until(b'\n', pending)
        .await
        .map_err(Error::control_mode)?;
    if read == 0 && pending.is_empty() {
        return Ok(None);
    }

    // read_until stops at the newline or at end of input, so what is left
    // without one is the last line tmux managed to write.
    let line = Line::parse(pending.strip_suffix(b"\n").unwrap_or(pending));
    pending.clear();

    Ok(Some(line))
}

/// Write one command line to the connection.
async fn write_line(stdin: &mut ChildStdin, line: &str) -> Result<(), Error> {
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(Error::control_mode)?;
    stdin.write_all(b"\n").await.map_err(Error::control_mode)?;
    stdin.flush().await.map_err(Error::control_mode)?;

    Ok(())
}

/// One classified line of the control-mode protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Line {
    BlockStart(u64),
    BlockEnd { number: u64, succeeded: bool },
    Event(Event),
    Text(TmuxText),
}

impl Line {
    fn parse(line: &[u8]) -> Self {
        let text = || Self::Text(TmuxText::from_bytes(line));

        let Some(rest) = line.strip_prefix(b"%") else {
            return text();
        };
        let (name, arguments) = split_once(rest, b' ');
        // Every notification tmux names is ASCII. Anything else is a line
        // that happens to start with a percent, not a notification.
        let Ok(name) = std::str::from_utf8(name) else {
            return text();
        };

        match name {
            // `%begin`, `%end`, and `%error` carry a timestamp, a number, and
            // flags. The number is what correlates a result with its command.
            "begin" | "end" | "error" => {
                let number = std::str::from_utf8(arguments).ok().and_then(|arguments| {
                    arguments
                        .split_whitespace()
                        .nth(1)
                        .and_then(|value| value.parse().ok())
                });

                match (name, number) {
                    ("begin", Some(number)) => Self::BlockStart(number),
                    (_, Some(number)) => Self::BlockEnd {
                        number,
                        succeeded: name == "end",
                    },
                    // A malformed header is text: guessing a number would
                    // correlate a result with the wrong command.
                    (_, None) => text(),
                }
            }
            "output" => {
                let (pane, bytes) = split_once(arguments, b' ');
                std::str::from_utf8(pane)
                    .ok()
                    .and_then(|pane| pane.parse().ok())
                    .map_or_else(text, |pane| {
                        Self::Event(Event::Output {
                            pane,
                            bytes: unescape_output(bytes),
                        })
                    })
            }
            "session-changed" => {
                let (session, _) = split_once(arguments, b' ');
                std::str::from_utf8(session)
                    .ok()
                    .and_then(|session| session.parse().ok())
                    .map_or_else(text, |session| {
                        Self::Event(Event::SessionChanged { session })
                    })
            }
            "exit" => Self::Event(Event::Exit),
            _ => Self::Event(Event::Other {
                name: name.to_owned(),
                rest: TmuxText::from_bytes(arguments),
            }),
        }
    }
}

/// Split at the first occurrence of `byte`, which is not kept.
fn split_once(bytes: &[u8], byte: u8) -> (&[u8], &[u8]) {
    bytes
        .iter()
        .position(|found| *found == byte)
        .map_or((bytes, [].as_slice()), |index| {
            (&bytes[..index], &bytes[index + 1..])
        })
}

/// Undo the escaping tmux applies to `%output`.
///
/// tmux writes a byte below `0x20` as `\ooo` and a backslash as `\\`, and
/// leaves everything else alone -- so a pane emitting Latin-1 or binary
/// produces a line that is not UTF-8. Anything else after a backslash is not
/// an escape tmux produces, so it is kept as written rather than guessed at.
fn unescape_output(source: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(source.len());
    let mut index = 0;

    while index < source.len() {
        if source[index] != b'\\' {
            bytes.push(source[index]);
            index += 1;
            continue;
        }

        match source.get(index + 1..index + 4) {
            Some(digits) if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) => {
                let value = digits
                    .iter()
                    .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                // Three octal digits can exceed one byte; tmux never emits
                // that, and truncating would corrupt rather than refuse.
                if let Ok(byte) = u8::try_from(value) {
                    bytes.push(byte);
                    index += 4;
                    continue;
                }
                bytes.push(source[index]);
                index += 1;
            }
            _ => {
                if source.get(index + 1) == Some(&b'\\') {
                    bytes.push(b'\\');
                    index += 2;
                } else {
                    bytes.push(source[index]);
                    index += 1;
                }
            }
        }
    }

    bytes
}

#[cfg(test)]
mod tests {

    use super::{Event, Line, unescape_output};
    use crate::TmuxText;

    #[test]
    fn block_headers_correlate_by_the_number_tmux_assigns() {
        assert_eq!(
            Line::parse(b"%begin 1786582374 347 0"),
            Line::BlockStart(347)
        );
        assert_eq!(
            Line::parse(b"%end 1786582374 347 0"),
            Line::BlockEnd {
                number: 347,
                succeeded: true,
            },
        );
        assert_eq!(
            Line::parse(b"%error 1786582374 353 1"),
            Line::BlockEnd {
                number: 353,
                succeeded: false,
            },
        );

        // A header without a usable number is text. Guessing one would
        // correlate a result with the wrong command.
        assert!(matches!(Line::parse(b"%begin bad"), Line::Text(_)));
    }

    #[test]
    fn notifications_are_parsed_and_unknown_ones_are_kept() {
        assert_eq!(
            Line::parse(b"%session-changed $0 work"),
            Line::Event(Event::SessionChanged {
                session: "$0".parse().expect("a session id parses"),
            }),
        );
        assert_eq!(Line::parse(b"%exit"), Line::Event(Event::Exit));

        // tmux adds notifications between releases, so an unrecognized one is
        // kept rather than dropped.
        assert_eq!(
            Line::parse(b"%window-renamed @2 build"),
            Line::Event(Event::Other {
                name: "window-renamed".to_owned(),
                rest: TmuxText::from_bytes(*b"@2 build"),
            }),
        );
    }

    #[test]
    fn a_line_is_bytes_because_tmux_does_not_promise_text() {
        // tmux escapes only what would break the line protocol, so a pane
        // emitting Latin-1 or binary produces a line that is not UTF-8.
        // Reading these as a string would fail the whole connection.
        let line = Line::parse(b"%output %0 \xff\xc3(");
        assert_eq!(
            line,
            Line::Event(Event::Output {
                pane: "%0".parse().expect("a pane id parses"),
                bytes: vec![0xff, 0xc3, b'('],
            }),
        );

        // The same holds for a window name inside a notification.
        assert_eq!(
            Line::parse(b"%window-renamed @2 \xff"),
            Line::Event(Event::Other {
                name: "window-renamed".to_owned(),
                rest: TmuxText::from_bytes(*b"@2 \xff"),
            }),
        );
    }

    #[test]
    fn output_escaping_round_trips_the_bytes_tmux_sends() {
        assert_eq!(unescape_output(b"plain"), b"plain");
        // tmux escapes a byte below 0x20 as three octal digits.
        assert_eq!(unescape_output(br"a\015b"), b"a\rb");
        assert_eq!(unescape_output(br"\377"), vec![0xff]);
        // A literal backslash arrives doubled.
        assert_eq!(unescape_output(br"a\\b"), b"a\\b");
        // Anything else after a backslash is not an escape tmux produces, so
        // it is kept rather than guessed at.
        assert_eq!(unescape_output(br"a\zb"), b"a\\zb");
    }
}
