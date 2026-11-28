//! Logical commands and transport-independent tmux encoders.

use std::ffi::{OsStr, OsString};
use std::fmt;

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
        crate::argv_operand(&self.value)
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
/// use libtmux_core::Command;
///
/// let command = Command::new("display-message").arg("hello");
/// assert_eq!(command.summary().argument_count(), 1);
/// ```
#[derive(Clone)]
#[must_use = "a command has no effect until it is dispatched"]
pub struct Command {
    subcommand: CommandArg,
    arguments: Vec<CommandArg>,
    requirement: Option<(&'static str, crate::ReleaseVersion)>,
    refused_intervals: Vec<(&'static str, crate::ReleaseVersion, crate::ReleaseVersion)>,
}

impl Command {
    #[doc(hidden)]
    pub fn new_session(operation: &crate::NewSession, print_format: &str) -> Self {
        let mut command = Self::new("new-session");
        for operand in operation.operands(print_format) {
            command = if operand.kind() == crate::OperandKind::Sensitive {
                command.sensitive_arg(operand.into_os_string())
            } else {
                command.arg(operand.into_os_string())
            };
        }
        command
    }

    /// Start a command with one logical tmux subcommand token.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::Command;
    ///
    /// let command = Command::new("list-sessions");
    /// assert_eq!(command.summary().to_string(), r#""list-sessions""#);
    /// ```
    #[must_use = "a command has no effect until it is dispatched"]
    pub fn new(subcommand: impl Into<OsString>) -> Self {
        Self {
            subcommand: CommandArg::public(subcommand.into()),
            arguments: Vec::new(),
            requirement: None,
            refused_intervals: Vec::new(),
        }
    }

    /// Require a daemon release before dispatching this command.
    ///
    /// Use this when a command starts work whose later cleanup needs a newer
    /// tmux. The requirement travels with the command and its chain; refusal
    /// happens before any member runs. Repeated calls retain the higher floor.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::{Command, ReleaseSuffix, ReleaseVersion};
    ///
    /// let command = Command::new("set-buffer")
    ///     .arg("-b").arg("temporary").sensitive_arg("payload")
    ///     .requires_tmux("temporary buffer cleanup",
    ///         ReleaseVersion::new(3, 4, ReleaseSuffix::FINAL));
    /// assert_eq!(command.summary().sensitive_argument_count(), 1);
    /// ```
    #[must_use = "use the returned command to retain its version requirement"]
    pub fn requires_tmux(mut self, capability: &'static str, needs: crate::ReleaseVersion) -> Self {
        if self
            .requirement
            .is_none_or(|(_, existing)| needs > existing)
        {
            self.requirement = Some((capability, needs));
        }
        self
    }

    #[doc(hidden)]
    pub fn refusing_interval(
        mut self,
        capability: &'static str,
        broken: crate::ReleaseVersion,
        fixed: crate::ReleaseVersion,
    ) -> Self {
        self.refused_intervals.push((capability, broken, fixed));
        self
    }

    /// Append one public logical argument.
    ///
    /// Public arguments appear in bounded, escaped diagnostic summaries.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::Command;
    ///
    /// let command = Command::new("display-message").arg("hello");
    /// assert_eq!(command.summary().public_argument_count(), 1);
    /// ```
    #[must_use = "use the returned command to retain the appended argument"]
    pub fn arg(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(CommandArg::public(argument.into()));
        self
    }

    /// Return the value of the command's `-t` flag, when it has one.
    ///
    /// Used to name the object in a failure. tmux does not always repeat the
    /// target it could not resolve, so this recovers it from the request.
    ///
    /// The first `-t` wins, which is what tmux does: it parses flags before
    /// positionals. A positional that happens to read `-t` can only be
    /// reached after tmux has already taken the flag, so the worst case is a
    /// mislabelled error rather than a wrong one.
    #[doc(hidden)]
    pub fn target(&self) -> Option<&OsStr> {
        let mut arguments = self.arguments.iter();
        while let Some(argument) = arguments.next() {
            if argument.value() == OsStr::new("-t") {
                return arguments.next().map(CommandArg::value);
            }
        }

        None
    }

    /// Render this command as one control-mode line.
    ///
    /// Returns `None` when a token is not valid UTF-8, because control mode
    /// is a text protocol and no escaping in it can carry those bytes.
    #[doc(hidden)]
    #[must_use]
    pub fn control_mode_line(&self) -> Option<String> {
        let mut line = render_control_mode_token(&self.subcommand.value)?;
        for argument in &self.arguments {
            line.push(' ');
            line.push_str(&render_control_mode_token(&argument.value)?);
        }

        Some(line)
    }

    /// Append one sensitive logical argument.
    ///
    /// The value is dispatched exactly but diagnostics use one
    /// length-independent redaction marker.
    ///
    /// # Examples
    ///
    /// ```
    /// use libtmux_core::Command;
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
    #[doc(hidden)]
    pub fn targeting(mut self, target: impl Into<OsString>) -> Self {
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
    /// use libtmux_core::Command;
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
    /// Every token passes through [`crate::argv_operand`], so no argument a
    /// caller supplied can reach tmux as a separator.
    #[doc(hidden)]
    pub fn extend_argv(&self, argv: &mut Vec<OsString>) {
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
/// use libtmux_core::{Command, CommandChain};
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
/// use libtmux_core::{Command, CommandChain};
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
    /// use libtmux_core::{Command, CommandChain};
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
    /// use libtmux_core::{Command, CommandChain};
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
    /// use libtmux_core::{Command, CommandChain};
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
    /// use libtmux_core::{Command, CommandChain};
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

    /// Render the whole chain as one control-mode line.
    ///
    /// tmux parses a line, so the boundary between members is the bare `;`
    /// its own parser reads. A member's literal `;` argument is quoted by
    /// [`Command::control_mode_line`] and stays an argument.
    ///
    /// One line is one `%begin`/`%end` block, so a chain sent this way reports
    /// one outcome for all of it -- the same trade a chained argv makes.
    ///
    /// Returns `None` when any token is not valid UTF-8.
    #[doc(hidden)]
    #[must_use]
    pub fn control_mode_line(&self) -> Option<String> {
        let mut line = self.first.control_mode_line()?;
        for command in &self.rest {
            line.push_str(" ; ");
            line.push_str(&command.control_mode_line()?);
        }

        Some(line)
    }

    /// Render the whole chain as one argv, separators included.
    #[doc(hidden)]
    #[must_use]
    pub fn into_argv(self, global_argv: &[OsString]) -> (Vec<OsString>, usize) {
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

/// Render one token for a control-mode command line.
///
/// Control mode takes a line that tmux parses, not an argv, so a token
/// holding a space or a quote has to survive that parse. Anything outside a
/// conservative safe set is double quoted with `\` and `"` escaped, which is
/// what tmux's own parser undoes.
///
/// A token holding a byte that is not valid UTF-8 cannot be written to a text
/// protocol at all, so this reports that rather than corrupting it.
fn render_control_mode_token(value: &OsStr) -> Option<String> {
    let text = value.to_str()?;
    let safe = !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./=@%:,+".contains(&byte))
        && !opens_a_condition(text);
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

/// Report whether tmux's command parser would read this word as a directive.
///
/// `cmd-parse.y` treats a word starting with `%` as a condition -- `%if`,
/// `%endif` and friends -- unless every character after it is a `%` or a
/// digit, which is what makes a bare pane id a token. Anything else beginning
/// with `%` is a syntax error, so `refresh-client -A %1:off` must be quoted
/// while `list-panes -t %1` must not be.
fn opens_a_condition(text: &str) -> bool {
    text.starts_with('%')
        && !text
            .bytes()
            .all(|byte| byte == b'%' || byte.is_ascii_digit())
}

/// A rendering of a dispatched command that is safe to log.
///
/// Every public token is ASCII escaped. Sensitive arguments are represented by
/// a fixed marker that discloses neither their bytes nor their length.
///
/// # Examples
///
/// ```
/// use libtmux_core::Command;
///
/// let command = Command::new("set-environment")
///     .arg("-g")
///     .arg("TOKEN")
///     .sensitive_arg("hunter2");
/// let summary = command.summary();
/// assert_eq!(summary.argument_count(), 3);
/// assert_eq!(summary.public_argument_count(), 2);
/// assert!(!summary.to_string().contains("hunter2"));
/// assert!(!format!("{summary:?}").contains("hunter2"));
/// ```
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
    /// use libtmux_core::Command;
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
    /// use libtmux_core::Command;
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
    /// use libtmux_core::Command;
    ///
    /// let summary = Command::new("set-environment").sensitive_arg("secret").summary();
    /// assert_eq!(summary.sensitive_argument_count(), 1);
    /// ```
    #[must_use]
    pub const fn sensitive_argument_count(&self) -> usize {
        self.sensitive_argument_count
    }

    /// The escaped subcommand, which is never a sensitive argument.
    #[doc(hidden)]
    #[must_use]
    pub fn subcommand(&self) -> &str {
        &self.subcommand
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
    let bytes = value.as_encoded_bytes();
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

impl Command {
    /// The explicit minimum release recorded by this command.
    #[doc(hidden)]
    #[must_use]
    pub const fn version_requirement(&self) -> Option<(&'static str, crate::ReleaseVersion)> {
        self.requirement
    }

    /// Known daemon intervals which a driver must refuse before dispatch.
    #[doc(hidden)]
    #[must_use]
    pub fn refused_intervals(
        &self,
    ) -> &[(&'static str, crate::ReleaseVersion, crate::ReleaseVersion)] {
        &self.refused_intervals
    }

    /// The original subcommand and argument values for capability checks.
    #[doc(hidden)]
    pub fn logical_words(&self) -> impl Iterator<Item = &OsStr> {
        std::iter::once(self.subcommand.value()).chain(self.arguments.iter().map(CommandArg::value))
    }
}

impl CommandChain {
    /// The logical commands in execution order, always including the first.
    #[doc(hidden)]
    pub fn commands(&self) -> impl Iterator<Item = &Command> {
        std::iter::once(&self.first).chain(&self.rest)
    }
}

#[cfg(test)]
mod tests;
