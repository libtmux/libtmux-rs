use std::borrow::Cow;
use std::ffi::OsString;
use std::fmt;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

pub use libtmux_core::{Command, CommandChain, CommandSummary};

const SEPARATOR_TOKEN: &str = ";";

#[cfg(feature = "control-mode")]
pub(crate) fn require_features(
    command: &Command,
    version: &crate::TmuxVersion,
) -> Result<(), crate::Error> {
    if let Some((capability, needs)) = command.version_requirement() {
        version.require(capability, needs)?;
    }
    for &(capability, broken, fixed) in command.refused_intervals() {
        crate::internal::feature_policy::refuse_interval(version, capability, broken, fixed)?;
    }
    let words: Vec<_> = command.logical_words().collect();
    crate::internal::feature_policy::require(&words, version)
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
    client_flags_end: usize,
    logical_subcommand_index: usize,
    /// The same logical command rendered for a control-mode connection.
    ///
    /// Carried beside the argv rather than derived from it: the argv holds
    /// the server's global flags -- `-S <socket>`, `-f`, `-u` -- which an open
    /// connection has already settled, and its tokens are lowered for
    /// `execve`, where a trailing `;` is escaped as `\;`. Neither survives
    /// re-rendering into a line tmux parses.
    ///
    /// `None` means either that no executor asked for a line or that a token
    /// is not valid UTF-8, which a text protocol cannot carry. The one
    /// executor that reads this field is only handed requests it asked to have
    /// rendered, so for it the two cases are the same: absence is a command
    /// control mode cannot express, and it says so rather than guessing.
    #[cfg(feature = "control-mode")]
    control_line: Option<String>,
    /// Commands in the request, which is how many blocks a connection answers
    /// it with when none fails.
    #[cfg(feature = "control-mode")]
    command_count: usize,
    /// Whether this request runs without a deadline of its own.
    ///
    /// A dispatch is bounded so a caller cannot wait on tmux forever. The
    /// exception is a `wait-for` client, which tmux holds until something
    /// signals or unlocks the channel and cannot withdraw once it is queued:
    /// killing it leaves tmux an entry with no client behind it, which eats
    /// the channel's next signal or its next lock. Such a request ends when
    /// tmux answers or the executor shuts down.
    unbounded: bool,
    /// Whether the client's stdout goes to the null device instead of a pipe.
    ///
    /// tmux 3.1 and older keep a client's stdout open after the client exits,
    /// so a pipe read to its end waits out the deadline. A request whose
    /// answer is on stderr and in the exit status does not need the pipe.
    stdout_discarded: bool,
    requirement: Option<(&'static str, crate::ReleaseVersion)>,
    refused_intervals: Vec<(&'static str, crate::ReleaseVersion, crate::ReleaseVersion)>,
}

impl CommandRequest {
    pub(crate) fn require_features(
        &self,
        version: &crate::TmuxVersion,
    ) -> Result<(), crate::Error> {
        if let Some((capability, needs)) = self.requirement {
            version.require(capability, needs)?;
        }
        for (capability, broken, fixed) in &self.refused_intervals {
            crate::internal::feature_policy::refuse_interval(version, capability, *broken, *fixed)?;
        }
        for command in self.argv[self.logical_subcommand_index..].split(|word| word == ";") {
            crate::internal::feature_policy::require(command, version)?;
        }
        Ok(())
    }

    pub(crate) fn is_client_diagnostic(&self) -> bool {
        matches!(
            &self.argv[self.logical_subcommand_index..],
            [flag] if flag == "-V" || flag == "-h"
        )
    }

    #[cfg(feature = "control-mode")]
    pub(crate) fn require_shared_control(&self) -> Result<(), crate::Error> {
        for command in self.argv[self.logical_subcommand_index..].split(|word| word == ";") {
            crate::control::policy::require_shared(command)?;
        }
        Ok(())
    }

    pub(crate) fn live_only(mut self) -> Self {
        if self.argv[..self.client_flags_end]
            .last()
            .is_none_or(|arg| arg != "-N")
        {
            self.argv
                .insert(self.client_flags_end, OsString::from("-N"));
            self.client_flags_end += 1;
            self.logical_subcommand_index += 1;
        }
        self
    }

    pub(crate) fn for_generation(self, generation: Option<crate::ServerGeneration>) -> Self {
        match generation {
            Some(generation) => self.live_only().prepend_guard(&generation.queue_guard()),
            None => self,
        }
    }

    pub(crate) fn fence_version(self, version: &crate::TmuxVersion) -> Self {
        let guard = Command::new("if-shell")
            .arg("-F")
            .arg(format!("#{{==:#{{version}},{}}}", version.raw()))
            .arg("")
            .arg("libtmux-core-version-changed");
        self.prepend_guard(&guard)
    }

    fn prepend_guard(mut self, guard: &Command) -> Self {
        #[cfg(feature = "control-mode")]
        {
            self.control_line = self.control_line.and_then(|line| {
                guard
                    .control_mode_line()
                    .map(|guard| format!("{guard} ; {line}"))
            });
            self.command_count += 1;
        }
        let mut prefix = Vec::new();
        guard.extend_argv(&mut prefix);
        prefix.push(OsString::from(SEPARATOR_TOKEN));
        let prefix_len = prefix.len();
        self.argv.splice(
            self.logical_subcommand_index..self.logical_subcommand_index,
            prefix,
        );
        self.logical_subcommand_index += prefix_len;
        self
    }

    pub(crate) fn fence_ordinary_version(mut self, version: &crate::TmuxVersion) -> Self {
        let arguments = self.argv.split_off(self.logical_subcommand_index);
        let action_length = arguments.len();
        let guarded = version.fence_ordinary_argv(arguments);
        self.logical_subcommand_index += guarded.len() - action_length;
        self.argv.extend(guarded);
        self
    }

    pub(crate) fn new(request_id: RequestId, command: Command) -> Self {
        Self::with_global_argv(request_id, &[], command)
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "request construction consumes the command at the dispatch boundary"
    )]
    pub(crate) fn with_global_argv(
        request_id: RequestId,
        global_argv: &[OsString],
        command: Command,
    ) -> Self {
        let summary = command.summary();
        let requirement = command.version_requirement();
        let refused_intervals = command.refused_intervals().to_vec();
        let mut argv = Vec::with_capacity(global_argv.len() + command.logical_words().count());
        argv.extend_from_slice(global_argv);
        let logical_subcommand_index = argv.len();
        command.extend_argv(&mut argv);

        Self {
            request_id,
            command: summary,
            argv,
            client_flags_end: logical_subcommand_index,
            logical_subcommand_index,
            #[cfg(feature = "control-mode")]
            control_line: None,
            #[cfg(feature = "control-mode")]
            command_count: 1,
            unbounded: false,
            stdout_discarded: false,
            requirement,
            refused_intervals,
        }
    }

    /// Preserve the physical argv already rendered by a shared core operation.
    pub(crate) fn native_argv(
        request_id: RequestId,
        global_argv: &[OsString],
        arguments: Vec<OsString>,
        summary: CommandSummary,
    ) -> Self {
        let mut argv = Vec::with_capacity(global_argv.len() + arguments.len());
        argv.extend_from_slice(global_argv);
        let logical_subcommand_index =
            argv.len() + usize::from(arguments.first().is_some_and(|word| word == "-N"));
        argv.extend(arguments);
        Self {
            request_id,
            command: summary,
            argv,
            client_flags_end: logical_subcommand_index,
            logical_subcommand_index,
            #[cfg(feature = "control-mode")]
            control_line: None,
            #[cfg(feature = "control-mode")]
            command_count: 1,
            unbounded: false,
            stdout_discarded: false,
            requirement: None,
            refused_intervals: Vec::new(),
        }
    }

    pub(crate) fn chain_with_global_argv(
        request_id: RequestId,
        global_argv: &[OsString],
        chain: CommandChain,
    ) -> Self {
        let command = chain.summary();
        #[cfg(feature = "control-mode")]
        let command_count = chain.command_count();
        let requirement = chain
            .commands()
            .filter_map(Command::version_requirement)
            .max_by_key(|(_, needs)| *needs);
        let refused_intervals = chain
            .commands()
            .flat_map(|command| command.refused_intervals().iter().copied())
            .collect();
        let (argv, logical_subcommand_index) = chain.into_argv(global_argv);

        Self {
            request_id,
            command,
            argv,
            client_flags_end: logical_subcommand_index,
            logical_subcommand_index,
            #[cfg(feature = "control-mode")]
            control_line: None,
            #[cfg(feature = "control-mode")]
            command_count,
            unbounded: false,
            stdout_discarded: false,
            requirement,
            refused_intervals,
        }
    }

    /// Run this request without a deadline of its own.
    pub(crate) const fn without_deadline(mut self) -> Self {
        self.unbounded = true;
        self
    }

    /// Whether this request runs without a deadline of its own.
    pub(crate) const fn is_unbounded(&self) -> bool {
        self.unbounded
    }

    /// Send the client's stdout to the null device.
    pub(crate) const fn discarding_stdout(mut self) -> Self {
        self.stdout_discarded = true;
        self
    }

    /// Whether the client's stdout goes to the null device.
    pub(crate) const fn is_stdout_discarded(&self) -> bool {
        self.stdout_discarded
    }

    /// Attach the control-mode rendering of the command this request carries.
    #[cfg(feature = "control-mode")]
    pub(crate) fn with_control_line(mut self, line: Option<String>) -> Self {
        self.control_line = line;
        self
    }

    /// Select the route before either executor can submit the request.
    #[cfg(feature = "control-mode")]
    pub(crate) fn can_share_control(&self) -> bool {
        self.control_line.is_some() && self.require_shared_control().is_ok()
    }

    /// Take the control-mode line, when one was rendered and representable.
    #[cfg(feature = "control-mode")]
    pub(crate) fn into_control_line(self) -> Option<String> {
        self.control_line
    }

    /// How many commands the request carries.
    #[cfg(feature = "control-mode")]
    pub(crate) const fn command_count(&self) -> usize {
        self.command_count
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

    /// Report an outcome a transport observed without running a process.
    ///
    /// Control mode closes a block with `%end` or `%error` and never with an
    /// exit status, and a test double never had one either. The code is
    /// synthesized so callers that read one see the shape tmux would have
    /// exited with. Already-absent cleanup also succeeds without a process.
    pub(crate) const fn from_block_outcome(succeeded: bool) -> Self {
        Self {
            success: succeeded,
            code: Some(if succeeded { 0 } else { 1 }),
            signal: None,
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
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// use libtmux_tokio::Command;
///
/// let guard = libtmux_tokio::test::TestServer::new().await?;
/// guard.server().new_session("work").await?;
///
/// let result = guard
///     .server()
///     .cmd(Command::new("list-sessions").arg("-F").arg("#{session_name}"))
///     .await?;
///
/// // The exact bytes tmux emitted are kept until a caller asks for text, because
/// // a session name need not be UTF-8.
/// assert_eq!(result.stdout(), b"work\n");
/// assert_eq!(result.stdout_utf8()?, "work\n");
///
/// guard.shutdown().await?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-id.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// assert!(result.request_id() > 0);
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-command.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// assert_eq!(result.command().to_string(), r#""list-sessions""#);
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-stdout.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// let _: &[u8] = result.stdout();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-stderr.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// let _: &[u8] = result.stderr();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-streams.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// let (_stdout, _stderr) = result.into_streams();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-stdout-utf8.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-stderr-utf8.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-stdout-lossy.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// let _text = result.stdout_lossy();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-stderr-lossy.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// let _text = result.stderr_lossy();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-success.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// let _success = result.success();
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
    /// # })?;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub const fn success(&self) -> bool {
        self.status.success()
    }

    /// Classify a nonzero status as a refusal for a named operation.
    ///
    /// Use a fixed operation name without targets or argument values. The
    /// result keeps no original target, so classification uses only what tmux
    /// echoed in stderr. Sensitive command output is withheld from the
    /// returned error.
    ///
    /// Returns `None` when the command succeeded.
    #[must_use]
    pub fn refusal_for(&self, operation: &'static str) -> Option<crate::Error> {
        if self.success() {
            None
        } else {
            Some(crate::Error::from_refused_result(operation, self, None))
        }
    }

    /// Return the process exit code, or `None` when it ended by signal.
    ///
    /// # Examples
    ///
    /// ```
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    /// # runtime.block_on(async {
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-code.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// assert!(result.exit_code().is_some());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
    /// let server = libtmux_tokio::Server::builder().socket_path("/tmp/libtmux-rs-test/result-signal.sock").build()?;
    /// let result = server.cmd(libtmux_tokio::Command::new("list-sessions")).await?;
    /// assert!(result.signal().is_none());
    /// server.shutdown().await?;
    /// # Ok::<(), libtmux_tokio::Error>(())
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
mod tests;
