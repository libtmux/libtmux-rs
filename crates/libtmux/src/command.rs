use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

const MAX_DIAGNOSTIC_BYTES: usize = 64;
const REDACTED_ARGUMENT: &str = "<redacted>";
const TRUNCATED_TOKEN: &str = "<truncated>";

/// The one argv token tmux reads as a command boundary.
///
/// tmux treats a bare `;` argv element as a separator and a `\;` element as a
/// literal semicolon, and nothing else distinguishes them. [`Command`] lowers
/// every trailing `;` a caller supplies, so this token can only enter an argv
/// through [`CommandChain`].
const SEPARATOR_TOKEN: &str = ";";

#[derive(Clone, Copy)]
enum ArgumentSensitivity {
    Public,
    Sensitive,
}

#[derive(Clone)]
struct CommandArg {
    value: OsString,
    sensitivity: ArgumentSensitivity,
}

impl CommandArg {
    fn public(value: OsString) -> Self {
        Self {
            value,
            sensitivity: ArgumentSensitivity::Public,
        }
    }

    fn sensitive(value: OsString) -> Self {
        Self {
            value,
            sensitivity: ArgumentSensitivity::Sensitive,
        }
    }

    fn diagnostic(&self) -> SummaryArgument {
        match self.sensitivity {
            ArgumentSensitivity::Public => SummaryArgument::Public(escape_diagnostic(&self.value)),
            ArgumentSensitivity::Sensitive => SummaryArgument::Sensitive,
        }
    }

    fn lower(&self) -> OsString {
        lower_logical_token(&self.value)
    }

    /// Return the argument's own bytes.
    ///
    /// Only used to recover a target from a failed request, which is a
    /// public value; a sensitive argument is never a `-t` target.
    fn value(&self) -> &OsStr {
        &self.value
    }
}

impl fmt::Debug for CommandArg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandArg")
            .field("diagnostic", &self.diagnostic().as_str())
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
enum SummaryArgument {
    Public(String),
    Sensitive,
    /// A command boundary authored by a [`CommandChain`], not by a caller.
    ///
    /// Kept distinct from a public `";"` argument so a diagnostic shows which
    /// semicolons tmux will act on: a separator renders bare, a literal
    /// semicolon renders quoted.
    Separator,
}

impl SummaryArgument {
    fn as_str(&self) -> &str {
        match self {
            Self::Public(value) => value,
            Self::Sensitive => REDACTED_ARGUMENT,
            Self::Separator => SEPARATOR_TOKEN,
        }
    }
}

/// A logical tmux command with classified diagnostic arguments.
///
/// Commands retain operating-system strings so dispatch can preserve arbitrary
/// Unix bytes. Use [`Command::summary`] for a bounded, sanitized diagnostic
/// representation.
///
/// # Examples
///
/// ```
/// use libtmux::Command;
///
/// let command = Command::new("display-message").arg("hello");
/// assert_eq!(command.summary().argument_count(), 1);
/// ```
#[derive(Clone)]
#[must_use = "a command has no effect until it is dispatched"]
pub struct Command {
    subcommand: CommandArg,
    arguments: Vec<CommandArg>,
}

impl Command {
    /// Start a command with one logical tmux subcommand token.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::Command;
    ///
    /// let command = Command::new("list-sessions");
    /// assert_eq!(command.summary().to_string(), r#""list-sessions""#);
    /// ```
    #[must_use = "a command has no effect until it is dispatched"]
    pub fn new(subcommand: impl Into<OsString>) -> Self {
        Self {
            subcommand: CommandArg::public(subcommand.into()),
            arguments: Vec::new(),
        }
    }

    /// Append one public logical argument.
    ///
    /// Public arguments appear in bounded, escaped diagnostic summaries.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::Command;
    ///
    /// let command = Command::new("display-message").arg("hello");
    /// assert_eq!(command.summary().public_argument_count(), 1);
    /// ```
    #[must_use = "use the returned command to retain the appended argument"]
    /// Render this command as one control-mode line.
    ///
    /// Returns `None` when a token is not valid UTF-8, because control mode
    /// is a text protocol and no escaping in it can carry those bytes.
    /// Return the value of the command's `-t` flag, when it has one.
    ///
    /// Used to name the object in a failure. tmux does not always repeat the
    /// target it could not resolve, so this recovers it from the request.
    ///
    /// The first `-t` wins, which is what tmux does: it parses flags before
    /// positionals. A positional that happens to read `-t` can only be
    /// reached after tmux has already taken the flag, so the worst case is a
    /// mislabelled error rather than a wrong one.
    pub(crate) fn target(&self) -> Option<&OsStr> {
        let mut arguments = self.arguments.iter();
        while let Some(argument) = arguments.next() {
            if argument.value() == OsStr::new("-t") {
                return arguments.next().map(CommandArg::value);
            }
        }

        None
    }

    #[cfg(feature = "control-mode")]
    pub(crate) fn control_mode_line(&self) -> Option<String> {
        let mut line = render_control_mode_token(&self.subcommand.value)?;
        for argument in &self.arguments {
            line.push(' ');
            line.push_str(&render_control_mode_token(&argument.value)?);
        }

        Some(line)
    }

    /// Append one public argument.
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(CommandArg::public(argument.into()));
        self
    }

    /// Append one sensitive logical argument.
    ///
    /// The value is dispatched exactly but diagnostics use one
    /// length-independent redaction marker.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::Command;
    ///
    /// let command = Command::new("set-environment")
    ///     .arg("TOKEN")
    ///     .sensitive_arg("secret");
    /// assert_eq!(command.summary().sensitive_argument_count(), 1);
    /// assert!(!command.summary().to_string().contains("secret"));
    /// ```
    #[must_use = "use the returned command to retain the appended sensitive argument"]
    pub fn sensitive_arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(CommandArg::sensitive(argument.into()));
        self
    }

    /// Return this command with `-t target` placed after the subcommand.
    ///
    /// Placed there rather than appended because tmux stops reading flags at
    /// the first positional: a target after one is not a target at all. It is
    /// taken as text, and the command succeeds having done the wrong thing --
    /// `send-keys -- echo hi -t work` types `echohi-tc` into whichever pane
    /// was current.
    pub(crate) fn targeting(mut self, target: impl Into<OsString>) -> Self {
        self.arguments.splice(
            0..0,
            [
                CommandArg::public(OsString::from("-t")),
                CommandArg::public(target.into()),
            ],
        );
        self
    }

    /// Build a bounded, sanitized summary of the logical command.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::Command;
    ///
    /// let summary = Command::new("display-message").arg("value;").summary();
    /// assert_eq!(summary.to_string(), r#""display-message" "value;""#);
    /// ```
    #[must_use]
    pub fn summary(&self) -> CommandSummary {
        CommandSummary::from_parts(
            escape_diagnostic(&self.subcommand.value),
            self.arguments.iter().map(CommandArg::diagnostic).collect(),
        )
    }

    /// Append this command's lowered argv tokens to `argv`.
    ///
    /// Every token passes through [`lower_logical_token`], so no argument a
    /// caller supplied can reach tmux as a separator.
    fn extend_argv(&self, argv: &mut Vec<OsString>) {
        argv.push(self.subcommand.lower());
        argv.extend(self.arguments.iter().map(CommandArg::lower));
    }

    /// Append this command's diagnostic tokens to a chain summary.
    fn extend_summary(&self, arguments: &mut Vec<SummaryArgument>) {
        arguments.push(self.subcommand.diagnostic());
        arguments.extend(self.arguments.iter().map(CommandArg::diagnostic));
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Command")
            .field("summary", &self.summary())
            .finish()
    }
}

/// Several tmux commands dispatched as one `tmux a \; b` invocation.
///
/// tmux reads a bare `;` argv element as a command boundary and a `\;` element
/// as a literal semicolon. [`Command`] lowers every trailing `;` a caller
/// supplies, so a boundary cannot come from an argument; it comes from this
/// type, which owns the separator. That is what makes a chain safe to build
/// from untrusted values.
///
/// A chain is one dispatch: one process, one exit status, one merged stdout.
/// tmux runs the sequence up to the first failure and drops the remainder, and
/// the merged result is the same whichever member failed, so a chain reports
/// one outcome rather than one per command. Use it to cut round trips when the
/// commands succeed or fail as a unit; dispatch separately when you need to
/// know which one failed.
///
/// # Examples
///
/// ```
/// use libtmux::{Command, CommandChain};
///
/// let chain = CommandChain::new(Command::new("send-keys").arg("-t").arg("%1"))
///     .then(Command::new("rename-window").arg("-t").arg("@1").arg("edit"));
///
/// assert_eq!(chain.command_count(), 2);
/// assert_eq!(
///     chain.summary().to_string(),
///     r#""send-keys" "-t" "%1" ; "rename-window" "-t" "@1" "edit""#,
/// );
/// ```
///
/// A literal semicolon stays an argument, and renders quoted so it is not
/// mistaken for the boundary beside it:
///
/// ```
/// use libtmux::{Command, CommandChain};
///
/// let chain = CommandChain::new(Command::new("display-message").arg(";"))
///     .then(Command::new("list-sessions"));
///
/// assert_eq!(
///     chain.summary().to_string(),
///     r#""display-message" ";" ; "list-sessions""#,
/// );
/// ```
///
/// The first command is stored apart from the rest so "a chain is never empty"
/// is a property of the type rather than an invariant to remember, which is
/// what keeps every method here free of a panicking path.
#[derive(Clone)]
#[must_use = "a chain has no effect until it is dispatched"]
pub struct CommandChain {
    first: Command,
    rest: Vec<Command>,
}

impl CommandChain {
    /// Start a chain from its first command.
    ///
    /// Taking the first command here keeps a chain non-empty by construction:
    /// an empty argv would make tmux act as a client rather than run anything.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{Command, CommandChain};
    ///
    /// let chain = CommandChain::new(Command::new("list-sessions"));
    /// assert_eq!(chain.command_count(), 1);
    /// ```
    pub fn new(command: Command) -> Self {
        Self {
            first: command,
            rest: Vec::new(),
        }
    }

    /// Append one command, to run after the previous one succeeds.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{Command, CommandChain};
    ///
    /// let chain = CommandChain::new(Command::new("select-pane").arg("-m"))
    ///     .then(Command::new("select-pane").arg("-M"));
    /// assert_eq!(chain.command_count(), 2);
    /// ```
    pub fn then(mut self, command: Command) -> Self {
        self.rest.push(command);
        self
    }

    /// Return the number of commands in the chain, always at least one.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{Command, CommandChain};
    ///
    /// let chain = CommandChain::new(Command::new("list-panes"));
    /// assert_eq!(chain.command_count(), 1);
    /// ```
    #[must_use]
    pub fn command_count(&self) -> usize {
        1 + self.rest.len()
    }

    /// Build a bounded, sanitized summary of the whole chain.
    ///
    /// Separators render bare and are counted as neither public nor sensitive
    /// arguments. Every other token of every member, including the subcommands
    /// after the first, counts as an argument of the summary.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::{Command, CommandChain};
    ///
    /// let summary = CommandChain::new(Command::new("select-pane").arg("-m"))
    ///     .then(Command::new("select-pane").arg("-M"))
    ///     .summary();
    ///
    /// assert_eq!(summary.to_string(), r#""select-pane" "-m" ; "select-pane" "-M""#);
    /// assert_eq!(summary.public_argument_count(), 3);
    /// ```
    #[must_use]
    pub fn summary(&self) -> CommandSummary {
        let mut arguments: Vec<SummaryArgument> = self
            .first
            .arguments
            .iter()
            .map(CommandArg::diagnostic)
            .collect();
        for command in &self.rest {
            arguments.push(SummaryArgument::Separator);
            command.extend_summary(&mut arguments);
        }

        CommandSummary::from_parts(escape_diagnostic(&self.first.subcommand.value), arguments)
    }

    /// Render the whole chain as one argv, separators included.
    fn into_argv(self, global_argv: &[OsString]) -> (Vec<OsString>, usize) {
        let mut argv = Vec::with_capacity(global_argv.len() + 2 * self.command_count());
        argv.extend_from_slice(global_argv);
        let logical_subcommand_index = argv.len();
        self.first.extend_argv(&mut argv);
        for command in &self.rest {
            argv.push(OsString::from(SEPARATOR_TOKEN));
            command.extend_argv(&mut argv);
        }

        (argv, logical_subcommand_index)
    }
}

impl fmt::Debug for CommandChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandChain")
            .field("summary", &self.summary())
            .field("command_count", &self.command_count())
            .finish()
    }
}

/// A bounded, sanitized diagnostic view of a logical [`Command`].
///
/// Render one token for a control-mode command line.
///
/// Control mode takes a line that tmux parses, not an argv, so a token
/// holding a space or a quote has to survive that parse. Anything outside a
/// conservative safe set is double quoted with `\` and `"` escaped, which is
/// what tmux's own parser undoes.
///
/// A token holding a byte that is not valid UTF-8 cannot be written to a text
/// protocol at all, so this reports that rather than corrupting it.
#[cfg(feature = "control-mode")]
fn render_control_mode_token(value: &OsStr) -> Option<String> {
    use std::os::unix::ffi::OsStrExt as _;

    let text = std::str::from_utf8(value.as_bytes()).ok()?;
    let safe = !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=@%:,+".contains(&byte));
    if safe {
        return Some(text.to_owned());
    }

    let mut rendered = String::with_capacity(text.len() + 2);
    rendered.push('"');
    for character in text.chars() {
        if matches!(character, '\\' | '"') {
            rendered.push('\\');
        }
        rendered.push(character);
    }
    rendered.push('"');

    Some(rendered)
}

/// Every public token is ASCII escaped. Sensitive arguments are represented by
/// a fixed marker that discloses neither their bytes nor their length.
#[derive(Clone, Eq, PartialEq)]
pub struct CommandSummary {
    subcommand: String,
    arguments: Vec<SummaryArgument>,
    public_argument_count: usize,
    sensitive_argument_count: usize,
}

impl CommandSummary {
    /// Build a summary, counting each argument class explicitly.
    fn from_parts(subcommand: String, arguments: Vec<SummaryArgument>) -> Self {
        let mut public_argument_count = 0;
        let mut sensitive_argument_count = 0;
        for argument in &arguments {
            match argument {
                SummaryArgument::Public(_) => public_argument_count += 1,
                SummaryArgument::Sensitive => sensitive_argument_count += 1,
                // A separator is structure, not an argument, so it is counted
                // as neither.
                SummaryArgument::Separator => {}
            }
        }

        Self {
            subcommand,
            arguments,
            public_argument_count,
            sensitive_argument_count,
        }
    }

    /// Return the number of logical arguments after the subcommand.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::Command;
    ///
    /// let summary = Command::new("set-option").arg("-g").arg("mouse").summary();
    /// assert_eq!(summary.argument_count(), 2);
    /// ```
    #[must_use]
    pub const fn argument_count(&self) -> usize {
        self.public_argument_count + self.sensitive_argument_count
    }

    /// Return the number of public logical arguments.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::Command;
    ///
    /// let summary = Command::new("set-option").arg("mouse").summary();
    /// assert_eq!(summary.public_argument_count(), 1);
    /// ```
    #[must_use]
    pub const fn public_argument_count(&self) -> usize {
        self.public_argument_count
    }

    /// Return the number of sensitive logical arguments.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux::Command;
    ///
    /// let summary = Command::new("set-environment").sensitive_arg("secret").summary();
    /// assert_eq!(summary.sensitive_argument_count(), 1);
    /// ```
    #[must_use]
    pub const fn sensitive_argument_count(&self) -> usize {
        self.sensitive_argument_count
    }
}

impl fmt::Display for CommandSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "\"{}\"", self.subcommand)?;
        for argument in &self.arguments {
            match argument {
                SummaryArgument::Public(value) => write!(formatter, " \"{value}\"")?,
                SummaryArgument::Sensitive => write!(formatter, " {REDACTED_ARGUMENT}")?,
                // Bare, so a boundary tmux acts on is distinguishable from a
                // literal `";"` argument, which renders quoted.
                SummaryArgument::Separator => write!(formatter, " {SEPARATOR_TOKEN}")?,
            }
        }
        Ok(())
    }
}

impl fmt::Debug for CommandSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandSummary")
            .field("diagnostic", &self.to_string())
            .field("argument_count", &self.argument_count())
            .field("public_argument_count", &self.public_argument_count)
            .field("sensitive_argument_count", &self.sensitive_argument_count)
            .finish_non_exhaustive()
    }
}

fn escape_diagnostic(value: &OsStr) -> String {
    let bytes = value.as_bytes();
    let mut escaped = String::with_capacity(bytes.len().min(MAX_DIAGNOSTIC_BYTES));

    for &byte in bytes.iter().take(MAX_DIAGNOSTIC_BYTES) {
        match byte {
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            b'\\' => escaped.push_str("\\\\"),
            b'"' => escaped.push_str("\\\""),
            b' '..=b'~' => escaped.push(char::from(byte)),
            _ => push_hex_escape(&mut escaped, byte),
        }
    }
    if bytes.len() > MAX_DIAGNOSTIC_BYTES {
        escaped.push_str(TRUNCATED_TOKEN);
    }
    escaped
}

fn push_hex_escape(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push_str("\\x");
    output.push(char::from(HEX[usize::from(byte >> 4)]));
    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

fn lower_logical_token(value: &OsStr) -> OsString {
    let bytes = value.as_bytes();
    if bytes.last() != Some(&b';') {
        return value.to_os_string();
    }

    let mut lowered = Vec::with_capacity(bytes.len() + 1);
    lowered.extend_from_slice(&bytes[..bytes.len() - 1]);
    lowered.push(b'\\');
    lowered.push(b';');
    OsString::from_vec(lowered)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RequestId(u64);

impl RequestId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

pub(crate) struct CommandRequest {
    request_id: RequestId,
    command: CommandSummary,
    argv: Vec<OsString>,
    logical_subcommand_index: usize,
}

impl CommandRequest {
    pub(crate) fn new(request_id: RequestId, command: Command) -> Self {
        Self::with_global_argv(request_id, &[], command)
    }

    pub(crate) fn with_global_argv(
        request_id: RequestId,
        global_argv: &[OsString],
        command: Command,
    ) -> Self {
        let summary = command.summary();
        let Command {
            subcommand,
            arguments,
        } = command;
        let mut argv = Vec::with_capacity(global_argv.len() + arguments.len() + 1);
        argv.extend_from_slice(global_argv);
        let logical_subcommand_index = argv.len();
        argv.push(subcommand.lower());
        argv.extend(arguments.iter().map(CommandArg::lower));

        Self {
            request_id,
            command: summary,
            argv,
            logical_subcommand_index,
        }
    }

    pub(crate) fn chain_with_global_argv(
        request_id: RequestId,
        global_argv: &[OsString],
        chain: CommandChain,
    ) -> Self {
        let command = chain.summary();
        let (argv, logical_subcommand_index) = chain.into_argv(global_argv);

        Self {
            request_id,
            command,
            argv,
            logical_subcommand_index,
        }
    }

    pub(crate) const fn request_id(&self) -> RequestId {
        self.request_id
    }

    pub(crate) fn summary(&self) -> &CommandSummary {
        &self.command
    }

    pub(crate) fn argv(&self) -> &[OsString] {
        &self.argv
    }

    pub(crate) const fn logical_subcommand_index(&self) -> usize {
        self.logical_subcommand_index
    }
}

impl fmt::Debug for CommandRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandRequest")
            .field("request_id", &self.request_id)
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessStatus {
    success: bool,
    code: Option<i32>,
    signal: Option<i32>,
}

impl ProcessStatus {
    pub(crate) fn from_exit_status(status: ExitStatus) -> Self {
        Self {
            success: status.success(),
            code: status.code(),
            signal: status.signal(),
        }
    }

    pub(crate) const fn success(self) -> bool {
        self.success
    }

    pub(crate) const fn code(self) -> Option<i32> {
        self.code
    }

    pub(crate) const fn signal(self) -> Option<i32> {
        self.signal
    }
}

/// The exact status and output captured for one tmux command.
///
/// A non-zero exit status is returned as data. Output bytes are never trimmed,
/// decoded, or mirrored between streams.
pub struct CommandResult {
    request_id: RequestId,
    command: CommandSummary,
    status: ProcessStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CommandResult {
    pub(crate) fn new(
        request_id: RequestId,
        command: CommandSummary,
        status: ProcessStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    ) -> Self {
        Self {
            request_id,
            command,
            status,
            stdout,
            stderr,
        }
    }

    /// Return the Core-scoped dispatch-request identity.
    ///
    /// The Core allocates this value before validation, so an error can expose
    /// an ID even when no process was spawned. Clones of one [`crate::Server`]
    /// share the allocating Core. The ID is not globally unique, a process ID,
    /// an internal attempt ID, or a control-mode protocol-block ID.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-id.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// assert!(result.request_id() > 0);
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn request_id(&self) -> u64 {
        self.request_id.get()
    }

    /// Return the sanitized logical command summary.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-command.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// assert_eq!(result.command().to_string(), r#""list-sessions""#);
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn command(&self) -> &CommandSummary {
        &self.command
    }

    /// Return stdout exactly as captured.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-stdout.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let _: &[u8] = result.stdout();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Return stderr exactly as captured.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-stderr.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let _: &[u8] = result.stderr();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Consume the result and return its exact stdout and stderr buffers.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-streams.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let (_stdout, _stderr) = result.into_streams();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn into_streams(self) -> (Vec<u8>, Vec<u8>) {
        (self.stdout, self.stderr)
    }

    /// Borrow stdout as UTF-8 without copying or replacement.
    ///
    /// # Errors
    ///
    /// Returns the borrowed decoding error when stdout is not valid UTF-8.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-stdout-utf8.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let _text = result.stdout_utf8()?;
    /// server.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn stdout_utf8(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.stdout)
    }

    /// Borrow stderr as UTF-8 without copying or replacement.
    ///
    /// # Errors
    ///
    /// Returns the borrowed decoding error when stderr is not valid UTF-8.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-stderr-utf8.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let _text = result.stderr_utf8()?;
    /// server.shutdown().await?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn stderr_utf8(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.stderr)
    }

    /// Return a named lossy UTF-8 view of stdout.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-stdout-lossy.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let _text = result.stdout_lossy();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn stdout_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    /// Return a named lossy UTF-8 view of stderr.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-stderr-lossy.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let _text = result.stderr_lossy();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn stderr_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }

    /// Return whether the process exited successfully.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-success.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// let _success = result.success();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn success(&self) -> bool {
        self.status.success()
    }

    /// Return the process exit code, or `None` when it ended by signal.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-code.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        self.status.code()
    }

    /// Return the terminating signal, or `None` for an ordinary exit.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux::Server::builder().socket_path("/tmp/libtmux-result-signal.sock").build()?;
    /// let result = server.cmd(libtmux::Command::new("list-sessions")).await?;
    /// assert!(result.signal().is_none());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn signal(&self) -> Option<i32> {
        self.status.signal()
    }
}

impl fmt::Debug for CommandResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandResult")
            .field("request_id", &self.request_id)
            .field("command", &self.command)
            .field("status", &self.status)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {

    use std::borrow::Cow;
    use std::error::Error as StdError;
    use std::ffi::OsString;
    use std::fmt::Display;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::process::ExitStatusExt;

    use static_assertions::{assert_impl_all, assert_not_impl_any};

    use super::*;

    assert_impl_all!(CommandArg: Send, Sync);
    assert_impl_all!(CommandRequest: Send, Sync);
    assert_impl_all!(CommandResult: Send, Sync);
    assert_impl_all!(RequestId: Send, Sync);
    assert_impl_all!(ProcessStatus: Send, Sync);
    assert_not_impl_any!(CommandResult: Display);
    assert_not_impl_any!(CommandRequest: Clone);

    fn argv_bytes(request: &CommandRequest) -> Vec<Vec<u8>> {
        request
            .argv()
            .iter()
            .map(|argument| argument.as_os_str().as_bytes().to_vec())
            .collect()
    }

    fn exit_status(code: i32) -> ExitStatus {
        ExitStatus::from_raw(code << 8)
    }

    fn command_result(
        command: Command,
        status: ExitStatus,
        stdout: &[u8],
        stderr: &[u8],
    ) -> CommandResult {
        let request = CommandRequest::new(RequestId::new(7), command);
        CommandResult::new(
            request.request_id(),
            request.summary().clone(),
            ProcessStatus::from_exit_status(status),
            stdout.to_vec(),
            stderr.to_vec(),
        )
    }

    #[test]
    fn dispatch_lowering_escapes_only_each_final_semicolon() {
        let cases: &[(&[u8], &[u8])] = &[
            (b";", b"\\;"),
            (b"value;", b"value\\;"),
            (b"a;b", b"a;b"),
            (b"\\;", b"\\\\;"),
            (b"\\\\;", b"\\\\\\;"),
            (b";;", b";\\;"),
            (b"plain", b"plain"),
        ];

        for (logical, physical) in cases {
            let argument = OsString::from_vec(logical.to_vec());
            let request =
                CommandRequest::new(RequestId::new(11), Command::new("cmd").arg(argument));
            assert_eq!(argv_bytes(&request), [b"cmd".to_vec(), physical.to_vec()]);
        }
    }

    #[test]
    fn dispatch_lowering_applies_to_every_argv_position() {
        let request = CommandRequest::new(
            RequestId::new(12),
            Command::new(";")
                .arg("first;")
                .arg("middle;")
                .arg("last;")
                .arg("after"),
        );

        assert_eq!(
            argv_bytes(&request),
            [
                b"\\;".to_vec(),
                b"first\\;".to_vec(),
                b"middle\\;".to_vec(),
                b"last\\;".to_vec(),
                b"after".to_vec(),
            ],
        );
    }

    #[test]
    fn dispatch_lowering_preserves_non_utf8_prefixes() {
        let logical = OsString::from_vec(b"\xffvalue;".to_vec());
        let request = CommandRequest::new(RequestId::new(13), Command::new(logical));

        assert_eq!(argv_bytes(&request), [b"\xffvalue\\;".to_vec()]);
    }

    #[test]
    fn command_summary_remains_logical_after_dispatch_lowering() {
        let request = CommandRequest::new(
            RequestId::new(14),
            Command::new("display-message").arg("value;"),
        );

        assert_eq!(
            request.summary().to_string(),
            r#""display-message" "value;""#
        );
        assert_eq!(
            argv_bytes(&request),
            [b"display-message".to_vec(), b"value\\;".to_vec()]
        );
    }

    #[test]
    fn request_preserves_the_dispatch_id_supplied_by_the_executor_owner() {
        let command = Command::new("display-message").arg("same");
        let cloned = command.clone();
        assert_eq!(command.summary(), cloned.summary());

        let first = CommandRequest::new(RequestId::new(101), command);
        let second = CommandRequest::new(RequestId::new(102), cloned);

        assert_eq!(first.request_id(), RequestId::new(101));
        assert_eq!(second.request_id(), RequestId::new(102));
    }

    #[test]
    fn sensitive_argument_lowering_preserves_bytes_while_diagnostics_redact() {
        let sensitive = OsString::from_vec(b"\xffsentinel-secret;".to_vec());
        let request = CommandRequest::new(
            RequestId::new(103),
            Command::new("set-environment")
                .arg("TOKEN")
                .sensitive_arg(sensitive),
        );

        assert_eq!(
            argv_bytes(&request),
            [
                b"set-environment".to_vec(),
                b"TOKEN".to_vec(),
                b"\xffsentinel-secret\\;".to_vec(),
            ],
        );
        for diagnostic in [
            format!("{request:?}"),
            format!("{:?}", request.summary()),
            request.summary().to_string(),
        ] {
            assert!(!diagnostic.contains("sentinel-secret"));
            assert!(!diagnostic.contains("\\xff"));
        }
    }

    #[test]
    fn process_status_preserves_exit_and_signal_outcomes() {
        let success = ProcessStatus::from_exit_status(exit_status(0));
        let failure = ProcessStatus::from_exit_status(exit_status(7));
        let signal = ProcessStatus::from_exit_status(ExitStatus::from_raw(15));

        assert!(success.success());
        assert_eq!(success.code(), Some(0));
        assert_eq!(success.signal(), None);
        assert!(!failure.success());
        assert_eq!(failure.code(), Some(7));
        assert_eq!(failure.signal(), None);
        assert!(!signal.success());
        assert_eq!(signal.code(), None);
        assert_eq!(signal.signal(), Some(15));
    }

    #[test]
    fn command_results_preserve_exact_output_bytes_and_trailing_blank_lines() {
        let result = command_result(
            Command::new("display-message"),
            exit_status(0),
            b"first\n\n",
            b"warning\n\n",
        );

        assert_eq!(result.request_id(), 7);
        assert_eq!(result.command().to_string(), r#""display-message""#);
        assert_eq!(result.stdout(), b"first\n\n");
        assert_eq!(result.stderr(), b"warning\n\n");
        let (stdout, stderr) = result.into_streams();
        assert_eq!(stdout, b"first\n\n");
        assert_eq!(stderr, b"warning\n\n");
    }

    #[test]
    fn command_results_offer_borrowed_strict_and_named_lossy_views() {
        let valid = command_result(Command::new("show-messages"), exit_status(0), b"ok\n", b"");
        assert_eq!(valid.stdout_utf8().expect("fixture is UTF-8"), "ok\n");
        assert!(matches!(valid.stdout_lossy(), Cow::Borrowed("ok\n")));

        let invalid = command_result(
            Command::new("show-messages"),
            exit_status(0),
            b"before\xffafter",
            b"error\xfe",
        );
        assert_eq!(
            invalid
                .stdout_utf8()
                .expect_err("fixture is not UTF-8")
                .valid_up_to(),
            6,
        );
        assert_eq!(
            invalid
                .stderr_utf8()
                .expect_err("fixture is not UTF-8")
                .valid_up_to(),
            5,
        );
        assert!(matches!(invalid.stdout_lossy(), Cow::Owned(_)));
        assert!(matches!(invalid.stderr_lossy(), Cow::Owned(_)));
    }

    #[test]
    fn borrowed_utf8_errors_never_own_or_debug_rejected_output() {
        let result = command_result(
            Command::new("show-messages"),
            exit_status(0),
            b"sentinel-secret\xff",
            b"",
        );
        let error = result
            .stdout_utf8()
            .expect_err("fixture contains an invalid UTF-8 byte");

        assert!(!format!("{error:?}").contains("sentinel-secret"));
        assert!(!error.to_string().contains("sentinel-secret"));
        assert!(StdError::source(&error).is_none());
    }

    #[test]
    fn nonzero_has_session_output_is_not_mirrored_or_promoted_to_an_error() {
        let result = command_result(
            Command::new("has-session").arg("-t").arg("missing"),
            exit_status(1),
            b"",
            b"can't find session: missing\n",
        );

        assert!(!result.success());
        assert_eq!(result.exit_code(), Some(1));
        assert_eq!(result.stdout(), b"");
        assert_eq!(result.stderr(), b"can't find session: missing\n");
    }

    #[test]
    fn sensitive_arguments_and_output_are_absent_from_private_debug_surfaces() {
        let sensitive = CommandArg::sensitive(OsString::from("sentinel-secret"));
        let command = Command::new("set-environment")
            .arg("TOKEN")
            .sensitive_arg("sentinel-secret");
        let summary = command.summary();
        let result = command_result(
            command.clone(),
            exit_status(7),
            b"sentinel-secret stdout",
            b"sentinel-secret stderr",
        );

        for diagnostic in [
            format!("{sensitive:?}"),
            format!("{command:?}"),
            format!("{summary:?}"),
            summary.to_string(),
            format!("{result:?}"),
        ] {
            assert!(!diagnostic.contains("sentinel-secret"));
        }
        assert!(
            result
                .stdout()
                .windows(15)
                .any(|bytes| bytes == b"sentinel-secret")
        );
        assert!(
            result
                .stderr()
                .windows(15)
                .any(|bytes| bytes == b"sentinel-secret")
        );
        let debug = format!("{result:?}");
        assert!(debug.contains("stdout_len"));
        assert!(debug.contains("stderr_len"));
        assert!(debug.contains("<redacted>"));
    }

    fn chain_request(chain: CommandChain) -> CommandRequest {
        CommandRequest::chain_with_global_argv(RequestId::new(200), &[], chain)
    }

    #[test]
    fn a_chain_separates_its_members_with_a_bare_semicolon_argv_token() {
        let request = chain_request(
            CommandChain::new(Command::new("select-pane").arg("-m"))
                .then(Command::new("send-keys").arg("-t").arg("{marked}")),
        );

        assert_eq!(
            argv_bytes(&request),
            [
                b"select-pane".to_vec(),
                b"-m".to_vec(),
                b";".to_vec(),
                b"send-keys".to_vec(),
                b"-t".to_vec(),
                b"{marked}".to_vec(),
            ],
        );
    }

    #[test]
    fn a_caller_supplied_semicolon_never_becomes_a_chain_separator() {
        // tmux reads a bare `;` element as a boundary and `\;` as a literal.
        // Every caller token is lowered, so the only bare `;` in the argv is
        // the one the chain itself authored.
        let request = chain_request(
            CommandChain::new(Command::new("display-message").arg(";").arg("kill-server;"))
                .then(Command::new("list-sessions")),
        );

        assert_eq!(
            argv_bytes(&request),
            [
                b"display-message".to_vec(),
                b"\\;".to_vec(),
                b"kill-server\\;".to_vec(),
                b";".to_vec(),
                b"list-sessions".to_vec(),
            ],
        );
        let bare_separators = argv_bytes(&request)
            .into_iter()
            .filter(|token| token == b";")
            .count();
        assert_eq!(bare_separators, 1);
    }

    #[test]
    fn a_single_command_chain_matches_that_command_dispatched_alone() {
        let alone = CommandRequest::new(RequestId::new(201), Command::new("list-panes").arg("-a"));
        let chained = chain_request(CommandChain::new(Command::new("list-panes").arg("-a")));

        assert_eq!(argv_bytes(&alone), argv_bytes(&chained));
        assert_eq!(alone.summary().to_string(), chained.summary().to_string());
    }

    #[test]
    fn global_argv_precedes_the_whole_chain_exactly_once() {
        let global = [OsString::from("-S"), OsString::from("/tmp/socket")];
        let request = CommandRequest::chain_with_global_argv(
            RequestId::new(202),
            &global,
            CommandChain::new(Command::new("a")).then(Command::new("b")),
        );

        assert_eq!(
            argv_bytes(&request),
            [
                b"-S".to_vec(),
                b"/tmp/socket".to_vec(),
                b"a".to_vec(),
                b";".to_vec(),
                b"b".to_vec(),
            ],
        );
        assert_eq!(request.logical_subcommand_index(), 2);
    }

    #[test]
    fn a_chain_summary_distinguishes_a_separator_from_a_literal_semicolon() {
        let summary = CommandChain::new(Command::new("display-message").arg(";"))
            .then(Command::new("list-sessions"))
            .summary();

        // The boundary renders bare; the argument renders quoted.
        assert_eq!(
            summary.to_string(),
            r#""display-message" ";" ; "list-sessions""#
        );
        // A separator is structure, so it counts as neither kind of argument.
        assert_eq!(summary.public_argument_count(), 2);
        assert_eq!(summary.sensitive_argument_count(), 0);
        assert_eq!(summary.argument_count(), 2);
    }

    #[test]
    fn chained_sensitive_arguments_dispatch_exactly_and_stay_redacted() {
        let chain = CommandChain::new(
            Command::new("set-environment")
                .arg("TOKEN")
                .sensitive_arg("sentinel-secret"),
        )
        .then(Command::new("list-sessions"));
        let request = chain_request(chain.clone());

        assert_eq!(
            argv_bytes(&request),
            [
                b"set-environment".to_vec(),
                b"TOKEN".to_vec(),
                b"sentinel-secret".to_vec(),
                b";".to_vec(),
                b"list-sessions".to_vec(),
            ],
        );
        assert_eq!(chain.summary().sensitive_argument_count(), 1);
        for diagnostic in [
            format!("{chain:?}"),
            format!("{request:?}"),
            chain.summary().to_string(),
        ] {
            assert!(!diagnostic.contains("sentinel-secret"));
        }
    }

    #[test]
    fn command_arg_public_debug_is_ascii_escaped_and_bounded() {
        let escaped = CommandArg::public(OsString::from_vec(b"line\n\xff".to_vec()));
        let bounded = CommandArg::public(OsString::from_vec(vec![b'a'; 128]));
        let escaped_debug = format!("{escaped:?}");
        let bounded_debug = format!("{bounded:?}");

        assert!(escaped_debug.is_ascii());
        assert!(escaped_debug.contains("\\n"));
        assert!(escaped_debug.contains("\\xff"));
        assert!(bounded_debug.contains("<truncated>"));
        assert!(bounded_debug.len() < 256);
    }
}
