use std::env;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::OnceCell;

#[cfg(feature = "control-mode")]
use crate::SessionId;
use crate::command::{CommandChain, CommandRequest, CommandResult, RequestId};
use crate::internal::executor::Executor;
use crate::internal::process::LaunchContext;
#[cfg(feature = "control-mode")]
use crate::internal::process::{PersistentChild, PersistentClients};
use crate::internal::subprocess::SubprocessExecutor;
use crate::internal::wait_for::ChannelWaits;
#[cfg(feature = "control-mode")]
use crate::limits::ControlClientLimits;
use crate::limits::{DispatchLimits, OutputLimits};
use crate::target::endpoint_resolution::{
    EndpointInputs, IdentityError, ResolvedSocketSelector, resolve_server_endpoint,
};
use crate::{
    Command, EngineCapabilities, Error, ServerConfigurationErrorKind, ServerIdentity, TmuxVersion,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) enum SocketSelection {
    Automatic,
    Name(OsString),
    Path(PathBuf),
}

pub(crate) struct BuildContext {
    working_directory: Option<PathBuf>,
    path: Option<OsString>,
    inherited_tmux: Option<OsString>,
    default_socket_path: Option<OsString>,
    default_socket_name: Option<OsString>,
    socket_root: Option<OsString>,
    fallback_socket_root: Option<PathBuf>,
    real_uid: u32,
}

impl BuildContext {
    pub(crate) fn new(
        working_directory: Option<PathBuf>,
        path: Option<OsString>,
        inherited_tmux: Option<OsString>,
        socket_root: Option<OsString>,
        fallback_socket_root: Option<PathBuf>,
        real_uid: u32,
    ) -> Self {
        Self {
            working_directory,
            path,
            inherited_tmux,
            default_socket_path: None,
            default_socket_name: None,
            socket_root,
            fallback_socket_root,
            real_uid,
        }
    }

    pub(crate) fn capture() -> Self {
        Self::new(
            env::current_dir().ok(),
            env::var_os("PATH"),
            env::var_os("TMUX"),
            env::var_os("TMUX_TMPDIR"),
            Path::new("/tmp").canonicalize().ok(),
            rustix::process::getuid().as_raw(),
        )
        .with_defaults(
            env::var_os("LIBTMUX_SOCKET_PATH"),
            env::var_os("LIBTMUX_SOCKET_NAME"),
        )
    }

    pub(crate) fn with_environment(mut self, environment: &[(OsString, Option<OsString>)]) -> Self {
        for (key, value) in environment {
            match key.to_str() {
                Some("PATH") => self.path.clone_from(value),
                Some("TMUX") => self.inherited_tmux.clone_from(value),
                Some("TMUX_TMPDIR") => self.socket_root.clone_from(value),
                Some("LIBTMUX_SOCKET_PATH") => self.default_socket_path.clone_from(value),
                Some("LIBTMUX_SOCKET_NAME") => self.default_socket_name.clone_from(value),
                _ => {}
            }
        }
        self
    }

    fn with_defaults(mut self, path: Option<OsString>, name: Option<OsString>) -> Self {
        self.default_socket_path = path;
        self.default_socket_name = name;
        self
    }
}

#[derive(Clone)]
pub(crate) struct CoreConfiguration {
    identity: ServerIdentity,
    socket_name: Option<OsString>,
    config_file: Option<PathBuf>,
    colors: Option<u16>,
    timeout: Duration,
    global_argv: Vec<OsString>,
    launch: LaunchContext,
    output_limits: OutputLimits,
    dispatch_limits: DispatchLimits,
    #[cfg(feature = "control-mode")]
    control_client_limits: ControlClientLimits,
    #[cfg(feature = "test-support")]
    synchronous_reap_on_supervisor_drop: bool,
}

impl CoreConfiguration {
    /// Replace the budgets this server's dispatches run under.
    pub(crate) const fn with_limits(
        mut self,
        output: OutputLimits,
        dispatch: DispatchLimits,
    ) -> Self {
        self.output_limits = output;
        self.dispatch_limits = dispatch;
        self
    }

    #[cfg(feature = "control-mode")]
    pub(crate) const fn with_control_client_limits(mut self, limits: ControlClientLimits) -> Self {
        self.control_client_limits = limits;
        self
    }

    pub(crate) fn resolve(
        selection: &SocketSelection,
        config_file: Option<PathBuf>,
        colors: Option<u16>,
        executable: OsString,
        timeout: Duration,
        context: BuildContext,
    ) -> Result<Self, ServerConfigurationErrorKind> {
        let working_directory = context
            .working_directory
            .filter(|path| path.is_absolute())
            .ok_or(ServerConfigurationErrorKind::WorkingDirectoryUnavailable)?;

        if !matches!(colors, None | Some(256)) {
            return Err(ServerConfigurationErrorKind::InvalidColorMode);
        }

        let (explicit_path, socket_name) = match selection {
            SocketSelection::Automatic => (None, None),
            SocketSelection::Name(name) => (None, Some(name.as_os_str())),
            SocketSelection::Path(path) => (Some(path.as_os_str()), None),
        };
        let inputs = EndpointInputs::new(
            context.socket_root.as_deref(),
            context.real_uid,
            context.inherited_tmux.as_deref(),
        )
        .with_defaults(
            context.default_socket_path.as_deref(),
            context.default_socket_name.as_deref(),
        )
        .with_captured_fallback_socket_root(context.fallback_socket_root.as_deref());
        let endpoint = resolve_server_endpoint(explicit_path, socket_name, inputs)
            .map_err(|error| map_identity_error(&error))?;
        let identity = endpoint.identity;
        let (socket_name, socket_directory) = match endpoint.selector {
            ResolvedSocketSelector::Path => (None, None),
            ResolvedSocketSelector::Name {
                name, configured, ..
            } => (
                configured.then_some(name),
                identity.socket_path().parent().map(Path::to_path_buf),
            ),
        };
        let mut global_argv = vec![
            OsString::from("-S"),
            identity.socket_path().as_os_str().to_os_string(),
        ];

        let config_file = config_file
            .map(|path| capture_config_path(&path, &working_directory))
            .transpose()?;
        if let Some(path) = &config_file {
            global_argv.push(OsString::from("-f"));
            global_argv.push(path.as_os_str().to_os_string());
        }
        if colors == Some(256) {
            global_argv.push(OsString::from("-2"));
        }
        // tmux replaces every byte outside `0x20..=0x7e` with `_` when it does
        // not believe its client speaks UTF-8, and it decides that from the
        // client process's own `TMUX`, `LANG`, `LC_ALL`, and `LC_CTYPE`. This
        // crate unsets `TMUX` and inherits whatever locale its caller had, so
        // a listing run from a container or a unit file returned `______` for
        // a name and no error. `-u` says it directly instead of hoping.
        global_argv.push(OsString::from("-u"));

        let mut launch = LaunchContext::new(executable).with_current_dir(&working_directory);
        launch = match context.path {
            Some(path) => launch.with_environment("PATH", path),
            None => launch.with_environment_removed("PATH"),
        };
        launch = launch
            .with_environment_removed("TMUX")
            .with_environment_removed("TMUX_PANE")
            .with_environment_removed("TMUX_TMPDIR");

        if let Some(directory) = socket_directory {
            launch = launch.with_socket_directory(directory);
        }
        Ok(Self {
            identity,
            socket_name,
            config_file,
            colors,
            timeout,
            global_argv,
            launch,
            output_limits: OutputLimits::default(),
            dispatch_limits: DispatchLimits::default(),
            #[cfg(feature = "control-mode")]
            control_client_limits: ControlClientLimits::default(),
            #[cfg(feature = "test-support")]
            synchronous_reap_on_supervisor_drop: false,
        })
    }

    pub(crate) fn with_client_environment(
        mut self,
        environment: Vec<(OsString, Option<OsString>)>,
    ) -> Result<Self, ServerConfigurationErrorKind> {
        for (key, value) in environment {
            if key.is_empty()
                || key.as_bytes().contains(&b'=')
                || key.as_bytes().contains(&0)
                || value
                    .as_ref()
                    .is_some_and(|value| value.as_bytes().contains(&0))
            {
                return Err(ServerConfigurationErrorKind::InvalidClientEnvironment);
            }
            self.launch = match value {
                Some(value) => self.launch.with_environment(key, value),
                None => self.launch.with_environment_removed(key),
            };
        }
        self.launch = self
            .launch
            .with_environment_removed("TMUX")
            .with_environment_removed("TMUX_PANE")
            .with_environment_removed("TMUX_TMPDIR");
        Ok(self)
    }

    pub(crate) fn default_timeout() -> Duration {
        DEFAULT_TIMEOUT
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn prevent_server_start(mut self) -> Self {
        self.global_argv.insert(0, OsString::from("-N"));
        self.launch = self.launch.with_environment("TERM", "xterm-256color");
        self.synchronous_reap_on_supervisor_drop = true;
        self
    }

    pub(crate) fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    pub(crate) fn socket_name(&self) -> Option<&OsStr> {
        self.socket_name.as_deref()
    }

    pub(crate) fn config_file(&self) -> Option<&Path> {
        self.config_file.as_deref()
    }

    pub(crate) const fn colors(&self) -> Option<u16> {
        self.colors
    }

    pub(crate) fn executable(&self) -> &OsStr {
        self.launch.executable()
    }

    pub(crate) fn resolved_executable(&self) -> Option<PathBuf> {
        self.launch.resolved_executable()
    }

    pub(crate) const fn timeout(&self) -> Duration {
        self.timeout
    }

    #[cfg(test)]
    pub(crate) fn working_directory(&self) -> &Path {
        self.launch
            .current_dir()
            .expect("resolved Core launch context has a working directory")
    }

    #[cfg(test)]
    pub(crate) fn global_argv(&self) -> &[OsString] {
        &self.global_argv
    }

    #[cfg(test)]
    #[allow(
        clippy::option_option,
        reason = "tests distinguish an absent action from removal and assignment"
    )]
    pub(crate) fn environment_value(&self, key: &OsStr) -> Option<Option<&OsStr>> {
        self.launch.environment_value(key)
    }
}

fn capture_config_path(
    path: &Path,
    working_directory: &Path,
) -> Result<PathBuf, ServerConfigurationErrorKind> {
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return Err(ServerConfigurationErrorKind::InvalidConfigPath);
    }
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(working_directory.join(path))
    }
}

fn map_identity_error(error: &IdentityError) -> ServerConfigurationErrorKind {
    match error {
        IdentityError::EmptySocketPath
        | IdentityError::SocketPathContainsNul
        | IdentityError::RelativeSocketPath => ServerConfigurationErrorKind::InvalidSocketPath,
        IdentityError::ConflictingSelectors => {
            ServerConfigurationErrorKind::ConflictingSocketSelectors
        }
        IdentityError::InvalidSocketName => ServerConfigurationErrorKind::InvalidSocketName,
        IdentityError::NoSocketRoot => ServerConfigurationErrorKind::SocketRootUnavailable,
        IdentityError::MalformedTmux => ServerConfigurationErrorKind::MalformedTmuxVariable,
    }
}

pub(crate) struct Core {
    pub(crate) lifecycle: crate::lifecycle::jobs::Journal,
    pub(crate) lifecycle_lock: tokio::sync::Semaphore,
    configuration: CoreConfiguration,
    executor: Arc<dyn Executor>,
    capabilities: OnceCell<EngineCapabilities>,
    next_request_id: AtomicU64,
    /// The `wait-for` channels this handle has a client on, or a signal for.
    ///
    /// Shared by every clone of a [`crate::Server`], because the clients are
    /// this process's and a channel is one name on the tmux server.
    channel_waits: ChannelWaits,
    #[cfg(feature = "control-mode")]
    persistent_clients: PersistentClients,
    /// PIDs of control clients this process itself spawned with
    /// [`Self::spawn_control`], for as long as each is still running.
    ///
    /// Shared with every [`PersistentChild`] this spawns, which removes its
    /// own entry on drop -- so this reflects processes, not connection
    /// handles, and stays correct across [`crate::control::ControlSender`]
    /// and [`crate::control::ControlEvents`] being dropped independently.
    #[cfg(feature = "control-mode")]
    control_client_pids: Arc<std::sync::Mutex<std::collections::HashSet<u32>>>,
}

impl Core {
    pub(crate) fn new(configuration: CoreConfiguration) -> Self {
        let executor = SubprocessExecutor::new(
            configuration.launch.executable().to_os_string(),
            configuration.timeout,
        )
        .with_launch_context(configuration.launch.clone())
        .with_output_limits(configuration.output_limits)
        .with_dispatch_limits(configuration.dispatch_limits);
        #[cfg(feature = "test-support")]
        let executor = executor.with_synchronous_reap_on_supervisor_drop(
            configuration.synchronous_reap_on_supervisor_drop,
        );
        Self::with_executor(configuration, Arc::new(executor))
    }

    fn with_executor(configuration: CoreConfiguration, executor: Arc<dyn Executor>) -> Self {
        #[cfg(feature = "control-mode")]
        let control_client_limits = configuration.control_client_limits;
        Self {
            lifecycle: crate::lifecycle::jobs::Journal::default(),
            lifecycle_lock: tokio::sync::Semaphore::new(1),
            configuration,
            executor,
            capabilities: OnceCell::new(),
            next_request_id: AtomicU64::new(1),
            channel_waits: ChannelWaits::default(),
            #[cfg(feature = "control-mode")]
            persistent_clients: PersistentClients::new(control_client_limits),
            #[cfg(feature = "control-mode")]
            control_client_pids: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
    }

    /// Build a core that dispatches over an already-attached connection.
    ///
    /// The capabilities are carried across rather than re-probed: the version
    /// probe is `tmux -V`, a client flag and not a command, so it has no
    /// control-mode spelling at all.
    #[cfg(feature = "control-mode")]
    pub(crate) fn over_control_mode(
        &self,
        sender: crate::control::ControlSender,
        capabilities: EngineCapabilities,
    ) -> Self {
        let executor = crate::internal::control_executor::ControlModeExecutor::new(sender);

        Self {
            lifecycle: self.lifecycle.clone(),
            lifecycle_lock: tokio::sync::Semaphore::new(1),
            configuration: self.configuration.clone(),
            executor: Arc::new(executor),
            capabilities: OnceCell::new_with(Some(capabilities)),
            next_request_id: AtomicU64::new(1),
            channel_waits: ChannelWaits::default(),
            persistent_clients: PersistentClients::new(self.configuration.control_client_limits),
            // Shared with the parent: a control client this process spawned is
            // the same process whichever handle dispatches through it.
            control_client_pids: Arc::clone(&self.control_client_pids),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_executor_for_test(executor: Arc<dyn Executor>) -> Self {
        let configuration = CoreConfiguration {
            identity: ServerIdentity::from_socket_path(PathBuf::from(
                "/tmp/libtmux-rs-test/no-such-socket",
            )),
            socket_name: None,
            config_file: None,
            colors: None,
            timeout: DEFAULT_TIMEOUT,
            output_limits: OutputLimits::default(),
            dispatch_limits: DispatchLimits::default(),
            #[cfg(feature = "control-mode")]
            control_client_limits: ControlClientLimits::default(),
            global_argv: Vec::new(),
            launch: LaunchContext::new("tmux").with_current_dir("/"),
            #[cfg(feature = "test-support")]
            synchronous_reap_on_supervisor_drop: false,
        };
        Self::with_executor(configuration, executor)
    }

    fn next_request_id(&self) -> RequestId {
        RequestId::new(self.next_request_id.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) async fn execute(&self, command: Command) -> Result<CommandResult, Error> {
        // Rendered before the command is consumed, and only for the transport
        // that reads a line. A subprocess dispatch pays nothing for it.
        #[cfg(feature = "control-mode")]
        let control_line = self
            .executor
            .renders_control_line()
            .then(|| command.control_mode_line());
        let request = CommandRequest::with_global_argv(
            self.next_request_id(),
            &self.configuration.global_argv,
            command,
        );
        #[cfg(feature = "control-mode")]
        let request = request.with_control_line(control_line.flatten());
        self.executor.execute(request).await
    }

    pub(crate) fn startup_child(&self, nonce: &str) -> Self {
        let mut configuration = self.configuration.clone();
        configuration.launch = configuration
            .launch
            .with_environment("LIBTMUX_LIFECYCLE_NONCE", nonce);
        Self::new(configuration)
    }

    pub(crate) async fn execute_no_start(&self, command: Command) -> Result<CommandResult, Error> {
        #[cfg(feature = "control-mode")]
        let control_line = self
            .executor
            .renders_control_line()
            .then(|| command.control_mode_line());
        let mut arguments = self.configuration.global_argv.clone();
        if !arguments.iter().any(|arg| arg == "-N") {
            arguments.insert(0, OsString::from("-N"));
        }
        let request = CommandRequest::with_global_argv(self.next_request_id(), &arguments, command);
        #[cfg(feature = "control-mode")]
        let request = request.with_control_line(control_line.flatten());
        self.executor.execute(request).await
    }

    /// Start a dispatch with no deadline of its own, and name it.
    ///
    /// For `wait-for` only, where a client killed part-way stays in tmux's
    /// waiter or locker list: this one ends when tmux answers or the executor
    /// shuts down. The caller bounds its own wait and names this dispatch when
    /// it gives up, so the id comes back with the future.
    pub(crate) fn dispatch_without_deadline(
        &self,
        command: Command,
    ) -> (RequestId, crate::internal::executor::DispatchFuture) {
        #[cfg(feature = "control-mode")]
        let control_line = self
            .executor
            .renders_control_line()
            .then(|| command.control_mode_line());
        let request = CommandRequest::with_global_argv(
            self.next_request_id(),
            &self.configuration.global_argv,
            command,
        )
        .without_deadline();
        #[cfg(feature = "control-mode")]
        let request = request.with_control_line(control_line.flatten());
        let request_id = request.request_id();
        (request_id, self.executor.execute(request))
    }

    /// The `wait-for` channels this handle has a client on, or a signal for.
    pub(crate) const fn channel_waits(&self) -> &ChannelWaits {
        &self.channel_waits
    }

    /// Whether this handle dispatches over a connection someone else holds.
    ///
    /// A connection runs one command at a time, so a command that blocks in
    /// tmux blocks the connection; the callers that would are the ones that
    /// ask.
    #[cfg(feature = "control-mode")]
    pub(crate) fn routes_over_control_mode(&self) -> bool {
        self.executor.renders_control_line()
    }

    pub(crate) async fn execute_chain(&self, chain: CommandChain) -> Result<CommandResult, Error> {
        #[cfg(feature = "control-mode")]
        let control_line = self
            .executor
            .renders_control_line()
            .then(|| chain.control_mode_line());
        let request = CommandRequest::chain_with_global_argv(
            self.next_request_id(),
            &self.configuration.global_argv,
            chain,
        );
        #[cfg(feature = "control-mode")]
        let request = request.with_control_line(control_line.flatten());
        self.executor.execute(request).await
    }

    pub(crate) async fn execute_chain_no_start(
        &self,
        chain: CommandChain,
    ) -> Result<CommandResult, Error> {
        #[cfg(feature = "control-mode")]
        let control_line = self
            .executor
            .renders_control_line()
            .then(|| chain.control_mode_line());
        let mut arguments = self.configuration.global_argv.clone();
        if !arguments.iter().any(|arg| arg == "-N") {
            arguments.insert(0, OsString::from("-N"));
        }
        let request =
            CommandRequest::chain_with_global_argv(self.next_request_id(), &arguments, chain);
        #[cfg(feature = "control-mode")]
        let request = request.with_control_line(control_line.flatten());
        self.executor.execute(request).await
    }

    async fn probe_capabilities(&self) -> Result<EngineCapabilities, Error> {
        let request = CommandRequest::new(self.next_request_id(), Command::new("-V"));
        let result = self.executor.execute(request).await?;
        if !result.success() {
            return Err(Error::version_probe_failed(
                result.request_id(),
                result.command().clone(),
                result.exit_code(),
                result.signal(),
            ));
        }
        let version = TmuxVersion::parse_output(result.stdout())?;
        version.ensure_supported()?;
        Ok(EngineCapabilities::from_tmux_version(version))
    }

    pub(crate) async fn capabilities(&self) -> Result<&EngineCapabilities, Error> {
        self.capabilities
            .get_or_try_init(|| self.probe_capabilities())
            .await
    }

    #[cfg(feature = "control-mode")]
    pub(crate) async fn spawn_control(
        &self,
        session: &SessionId,
    ) -> Result<PersistentChild, Error> {
        let mut global_argv = self.configuration.global_argv.clone();
        global_argv.push(OsString::from("-C"));
        let request = CommandRequest::with_global_argv(
            self.next_request_id(),
            &global_argv,
            // Opening an observer must not copy this process's environment
            // into the session it is inspecting.
            Command::new("attach")
                .arg("-E")
                .arg("-t")
                .arg(session.to_string()),
        );
        let reservation = self
            .persistent_clients
            .reserve(
                request.request_id(),
                request.summary().clone(),
                tokio::time::Instant::now().checked_add(self.configuration.timeout),
            )
            .await?;
        PersistentChild::spawn(
            &self.configuration.launch,
            &request,
            reservation,
            Arc::clone(&self.control_client_pids),
        )
    }

    /// Report whether this process itself spawned the control client with
    /// this pid, and it is still running.
    ///
    /// For a caller telling a human's attached client apart from a control
    /// connection this same process opened to watch or wait on the server:
    /// [`Self::spawn_control`] is the only thing that adds an entry.
    #[cfg(feature = "control-mode")]
    pub(crate) fn owns_control_client(&self, pid: u32) -> bool {
        self.control_client_pids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&pid)
    }

    pub(crate) async fn shutdown(&self) -> Result<(), Error> {
        #[cfg(feature = "control-mode")]
        {
            let (executor, ()) =
                tokio::join!(self.executor.shutdown(), self.persistent_clients.shutdown());
            executor
        }
        #[cfg(not(feature = "control-mode"))]
        {
            self.executor.shutdown().await
        }
    }

    pub(crate) fn configuration(&self) -> &CoreConfiguration {
        &self.configuration
    }

    pub(crate) fn require_same_server(
        &self,
        other: &ServerIdentity,
        operation: &'static str,
    ) -> Result<(), Error> {
        if self.configuration.identity() == other {
            Ok(())
        } else {
            Err(Error::server_mismatch(operation))
        }
    }
}

#[cfg(test)]
mod tests {

    use std::error::Error as StdError;
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::time::{Duration, Instant};

    use super::{BuildContext, Core, CoreConfiguration, SocketSelection};
    use crate::{Command, Error, ServerConfigurationErrorKind};
    use rustix::io::Errno;

    const CAPTURE_CHILD_ENV: &str = "LIBTMUX_RS_CAPTURE_CONTEXT_CHILD";
    const CAPTURE_CHILD_TEST: &str =
        "internal::core::tests::captured_fallback_ignores_generic_tmpdir_child";

    #[test]
    fn captured_fallback_ignores_generic_tmpdir_child() {
        if std::env::var_os(CAPTURE_CHILD_ENV).is_none() {
            return;
        }
        let captured = BuildContext::capture();
        assert_eq!(
            captured.fallback_socket_root.as_deref(),
            Some(
                Path::new("/tmp")
                    .canonicalize()
                    .expect("system temporary root exists")
                    .as_path()
            )
        );
    }

    #[test]
    fn captured_fallback_is_independent_of_generic_tmpdir() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let status = process::Command::new(std::env::current_exe().expect("test executable"))
            .arg("--exact")
            .arg(CAPTURE_CHILD_TEST)
            .arg("--nocapture")
            .env(CAPTURE_CHILD_ENV, "1")
            .env("TMPDIR", directory.path())
            .status()
            .expect("capture helper starts");
        assert!(status.success(), "capture helper succeeds");
    }

    fn context(
        cwd: Option<&Path>,
        path: Option<&OsStr>,
        tmux: Option<&OsStr>,
        socket_root: Option<&OsStr>,
        fallback_root: Option<&Path>,
    ) -> BuildContext {
        BuildContext::new(
            cwd.map(Path::to_path_buf),
            path.map(OsStr::to_os_string),
            tmux.map(OsStr::to_os_string),
            socket_root.map(OsStr::to_os_string),
            fallback_root.map(Path::to_path_buf),
            1000,
        )
    }

    fn resolve(
        selection: &SocketSelection,
        config_file: Option<PathBuf>,
        colors: Option<u16>,
        context: BuildContext,
    ) -> Result<CoreConfiguration, ServerConfigurationErrorKind> {
        CoreConfiguration::resolve(
            selection,
            config_file,
            colors,
            OsString::from("tmux"),
            Duration::from_secs(9),
            context,
        )
    }

    fn argv(configuration: &CoreConfiguration) -> Vec<&[u8]> {
        use std::os::unix::ffi::OsStrExt as _;

        configuration
            .global_argv()
            .iter()
            .map(|value| value.as_os_str().as_bytes())
            .collect()
    }

    #[test]
    fn explicit_paths_freeze_cwd_and_remove_unrelated_tmux_context() {
        let configuration = resolve(
            &SocketSelection::Path(PathBuf::from("/captured/work/relative-socket;")),
            Some(PathBuf::from("relative-config;")),
            Some(256),
            context(
                Some(Path::new("/captured/work")),
                Some(OsStr::new("/captured/bin")),
                Some(OsStr::new("/live/socket,1,0")),
                Some(OsStr::new("/ignored/root")),
                Some(Path::new("/fallback")),
            ),
        )
        .expect("explicit capture succeeds");

        assert_eq!(
            argv(&configuration),
            [
                b"-S".as_slice(),
                b"/captured/work/relative-socket;".as_slice(),
                b"-f".as_slice(),
                b"/captured/work/relative-config;".as_slice(),
                b"-2".as_slice(),
                b"-u".as_slice(),
            ]
        );
        assert_eq!(
            configuration.working_directory(),
            Path::new("/captured/work")
        );
        assert_eq!(
            configuration.identity().socket_path(),
            Path::new("/captured/work/relative-socket;")
        );
        assert_eq!(
            configuration.environment_value(OsStr::new("PATH")),
            Some(Some(OsStr::new("/captured/bin")))
        );
        assert_eq!(
            configuration.environment_value(OsStr::new("TMUX")),
            Some(None)
        );
        assert_eq!(
            configuration.environment_value(OsStr::new("TMUX_PANE")),
            Some(None)
        );
        assert_eq!(
            configuration.environment_value(OsStr::new("TMUX_TMPDIR")),
            Some(None)
        );
    }

    #[cfg(feature = "test-support")]
    #[test]
    fn test_support_adds_one_no_start_global_option() {
        let configuration = resolve(
            &SocketSelection::Path(PathBuf::from("/tmp/libtmux-rs-test/test-support.sock")),
            Some(PathBuf::from("/tmp/libtmux-test-support.conf")),
            None,
            context(
                Some(Path::new("/captured/work")),
                Some(OsStr::new("/captured/bin")),
                None,
                None,
                Some(Path::new("/fallback")),
            ),
        )
        .expect("test-support configuration resolves")
        .prevent_server_start();

        let arguments = argv(&configuration);
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| **argument == b"-N")
                .count(),
            1,
        );
        assert!(arguments.windows(2).any(|pair| {
            pair == [
                b"-S".as_slice(),
                b"/tmp/libtmux-rs-test/test-support.sock".as_slice(),
            ]
        }));
        assert!(arguments.windows(2).any(|pair| {
            pair == [
                b"-f".as_slice(),
                b"/tmp/libtmux-test-support.conf".as_slice(),
            ]
        }));
    }

    #[test]
    fn named_and_default_sockets_freeze_the_canonical_socket_root() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let root = workspace.path().join("root");
        std::fs::create_dir(&root).expect("socket root exists");
        let canonical = root.canonicalize().expect("socket root canonicalizes");
        let named = resolve(
            &SocketSelection::Name(OsString::from("named;")),
            None,
            None,
            context(
                Some(workspace.path()),
                Some(OsStr::new("/captured/bin")),
                None,
                Some(root.as_os_str()),
                Some(Path::new("/fallback")),
            ),
        )
        .expect("named socket resolves");
        let fallback = resolve(
            &SocketSelection::Automatic,
            None,
            None,
            context(
                Some(workspace.path()),
                None,
                None,
                None,
                Some(Path::new("/fallback")),
            ),
        )
        .expect("default socket uses the captured fallback");

        assert_eq!(
            argv(&named),
            [
                b"-S".as_slice(),
                canonical.join("tmux-1000/named;").as_os_str().as_bytes(),
                b"-u".as_slice()
            ]
        );
        assert_eq!(
            named.identity().socket_path(),
            canonical.join("tmux-1000/named;")
        );
        assert_eq!(
            named.environment_value(OsStr::new("TMUX_TMPDIR")),
            Some(None)
        );
        assert_eq!(named.environment_value(OsStr::new("TMUX_PANE")), Some(None));
        assert_eq!(
            argv(&fallback),
            [
                b"-S".as_slice(),
                b"/fallback/tmux-1000/default".as_slice(),
                b"-u".as_slice()
            ]
        );
        assert_eq!(
            fallback.environment_value(OsStr::new("TMUX_TMPDIR")),
            Some(None)
        );
        assert_eq!(
            fallback.identity().socket_path(),
            Path::new("/fallback/tmux-1000/default")
        );
        assert_eq!(fallback.environment_value(OsStr::new("PATH")), Some(None));
        assert_eq!(fallback.environment_value(OsStr::new("TMUX")), Some(None));
        assert_eq!(
            fallback.environment_value(OsStr::new("TMUX_PANE")),
            Some(None)
        );
    }

    #[test]
    fn inherited_endpoint_becomes_explicit_and_clears_child_context() {
        let configuration = resolve(
            &SocketSelection::Automatic,
            None,
            None,
            context(
                Some(Path::new("/captured/work")),
                Some(OsStr::new("/captured/bin")),
                Some(OsStr::new("/captured/work/relative/socket,10,4")),
                Some(OsStr::new("/unrelated/root")),
                Some(Path::new("/fallback")),
            ),
        )
        .expect("inherited endpoint resolves");

        assert_eq!(
            argv(&configuration),
            [
                b"-S".as_slice(),
                b"/captured/work/relative/socket".as_slice(),
                b"-u".as_slice(),
            ]
        );
        assert_eq!(
            configuration.identity().socket_path(),
            Path::new("/captured/work/relative/socket")
        );
        assert_eq!(
            configuration.environment_value(OsStr::new("TMUX")),
            Some(None)
        );
        assert_eq!(
            configuration.environment_value(OsStr::new("TMUX_PANE")),
            Some(None)
        );
        assert_eq!(
            configuration.environment_value(OsStr::new("TMUX_TMPDIR")),
            Some(None)
        );
    }

    #[test]
    fn configuration_owns_defaults_after_the_input_map_changes() {
        let mut environment = std::collections::HashMap::from([
            (
                "LIBTMUX_SOCKET_PATH",
                OsString::from("/tmp/libtmux-rs-dev/before"),
            ),
            ("LIBTMUX_SOCKET_NAME", OsString::from("ignored/invalid")),
        ]);
        let captured = context(
            Some(Path::new("/")),
            None,
            None,
            None,
            Some(Path::new("/tmp")),
        )
        .with_defaults(
            environment.get("LIBTMUX_SOCKET_PATH").cloned(),
            environment.get("LIBTMUX_SOCKET_NAME").cloned(),
        );
        let configuration = resolve(&SocketSelection::Automatic, None, None, captured).unwrap();
        environment.insert(
            "LIBTMUX_SOCKET_PATH",
            OsString::from("/tmp/libtmux-rs-dev/after"),
        );
        environment.remove("LIBTMUX_SOCKET_NAME");
        assert_eq!(
            configuration.identity().socket_path(),
            Path::new("/tmp/libtmux-rs-dev/before")
        );
        assert_eq!(
            argv(&configuration),
            [
                b"-S".as_slice(),
                b"/tmp/libtmux-rs-dev/before".as_slice(),
                b"-u".as_slice()
            ]
        );
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn named_socket_startup_prepares_a_fresh_root_and_checks_permissions() {
        let namespace = Path::new("/tmp/libtmux-rs-dev");
        std::fs::create_dir_all(namespace).unwrap();
        for mode in [None, Some(0o750), Some(0o705)] {
            let root = tempfile::Builder::new()
                .prefix("named-default-")
                .tempdir_in(namespace)
                .unwrap();
            let uid_directory = root
                .path()
                .join(format!("tmux-{}", rustix::process::getuid().as_raw()));
            if let Some(mode) = mode {
                std::fs::create_dir(&uid_directory).unwrap();
                std::fs::set_permissions(&uid_directory, std::fs::Permissions::from_mode(mode))
                    .unwrap();
            }
            let context = BuildContext::new(
                Some(root.path().to_path_buf()),
                std::env::var_os("PATH"),
                None,
                Some(root.path().as_os_str().to_os_string()),
                Some(PathBuf::from("/tmp")),
                rustix::process::getuid().as_raw(),
            );
            let configuration = resolve(
                &SocketSelection::Name(root.path().file_name().unwrap().to_os_string()),
                None,
                None,
                context,
            )
            .unwrap();
            let server = crate::Server::from_core(std::sync::Arc::new(Core::new(configuration)));
            let result = server
                .with_session("example", async |session| {
                    Ok::<_, Error>((session.id().to_string(), server.socket_path().exists()))
                })
                .await;
            let cleanup = server.kill().await;
            let shutdown = server.shutdown().await;
            if mode == Some(0o705) {
                assert!(matches!(
                    result,
                    Err(crate::ScopeError::Creation(Error::Spawn { .. }))
                ));
                assert!(!server.socket_path().exists());
            } else {
                assert!(result.is_ok(), "{result:?}");
                assert!(
                    result.unwrap().1,
                    "tmux did not start on the captured endpoint"
                );
                assert!(cleanup.is_ok(), "{cleanup:?}");
                assert!(uid_directory.is_dir());
            }
            assert!(shutdown.is_ok());
        }
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn removed_named_root_cannot_fall_back_to_another_endpoint() {
        let namespace = Path::new("/tmp/libtmux-rs-dev");
        std::fs::create_dir_all(namespace).unwrap();
        let root = tempfile::Builder::new()
            .prefix("removed-default-")
            .tempdir_in(namespace)
            .unwrap();
        let name = root.path().file_name().unwrap().to_os_string();
        let context = BuildContext::new(
            Some(namespace.to_path_buf()),
            std::env::var_os("PATH"),
            None,
            Some(root.path().as_os_str().to_os_string()),
            Some(PathBuf::from("/tmp")),
            rustix::process::getuid().as_raw(),
        );
        let configuration =
            resolve(&SocketSelection::Name(name.clone()), None, None, context).unwrap();
        let server = crate::Server::from_core(std::sync::Arc::new(Core::new(configuration)));
        let captured = server.socket_path().to_path_buf();
        drop(root);
        let result = server.new_session("example").await;
        server.shutdown().await.unwrap();
        assert!(matches!(result, Err(Error::Spawn { .. })), "{result:?}");
        assert!(!captured.exists());
        assert!(
            !Path::new("/tmp")
                .join(format!("tmux-{}", rustix::process::getuid().as_raw()))
                .join(name)
                .exists()
        );
    }

    #[test]
    fn captured_context_failures_map_to_exact_configuration_kinds() {
        let missing_cwd = resolve(
            &SocketSelection::Path(PathBuf::from("relative")),
            None,
            None,
            context(None, None, None, None, Some(Path::new("/fallback"))),
        );
        assert!(matches!(
            missing_cwd,
            Err(ServerConfigurationErrorKind::WorkingDirectoryUnavailable)
        ));

        let missing_root = resolve(
            &SocketSelection::Automatic,
            None,
            None,
            context(
                Some(Path::new("/captured/work")),
                None,
                None,
                Some(OsStr::new("/missing/root")),
                None,
            ),
        );
        assert!(matches!(
            missing_root,
            Err(ServerConfigurationErrorKind::SocketRootUnavailable)
        ));
    }

    #[tokio::test]
    async fn captured_context_is_applied_to_the_real_subprocess_boundary() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let bin = workspace.path().join("bin");
        std::fs::create_dir(&bin).expect("bin directory exists");
        let executable = bin.join("fake-tmux");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
if [ "${1-}" = __libtmux_fixture_ready__ ]; then
    exit 0
fi
set -eu
pwd
for argument do printf '<%s>\n' "$argument"; done
printf '<PATH=%s>\n' "${PATH-unset}"
printf '<TMUX=%s>\n' "${TMUX-unset}"
printf '<TMUX_PANE=%s>\n' "${TMUX_PANE-unset}"
printf '<TMUX_TMPDIR=%s>\n' "${TMUX_TMPDIR-unset}"
"#,
        )
        .expect("fake executable is writable");
        let mut permissions = std::fs::metadata(&executable)
            .expect("fake executable metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).expect("fake executable is runnable");
        let readiness_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match process::Command::new(&executable)
                .arg("__libtmux_fixture_ready__")
                .status()
            {
                Ok(status) => {
                    assert!(
                        status.success(),
                        "fixture readiness probe exited with {status}"
                    );
                    break;
                }
                Err(source) => {
                    assert_eq!(
                        source.raw_os_error(),
                        Some(Errno::TXTBSY.raw_os_error()),
                        "fixture readiness probe failed"
                    );
                    assert!(
                        Instant::now() < readiness_deadline,
                        "fixture remained busy past the readiness deadline"
                    );
                    std::thread::yield_now();
                }
            }
        }

        let configuration = CoreConfiguration::resolve(
            &SocketSelection::Path(workspace.path().join("socket;")),
            Some(PathBuf::from("config;")),
            Some(256),
            OsString::from("fake-tmux"),
            Duration::from_secs(5),
            context(
                Some(workspace.path()),
                Some(bin.as_os_str()),
                Some(OsStr::new("/live/socket,2,1")),
                Some(OsStr::new("/live/root")),
                Some(Path::new("/fallback")),
            ),
        )
        .expect("captured configuration resolves");
        let core = Core::new(configuration);
        let result = core
            .execute(Command::new("display-message").arg("value;"))
            .await
            .expect("fake subprocess executes");
        let stdout = result.stdout_utf8().expect("fixture output is UTF-8");

        // The working directory is captured canonically, so the expectation
        // has to be too: on macOS a temporary path arrives as `/var/...` and
        // resolves to `/private/var/...`.
        let canonical_workspace = workspace.path().canonicalize().expect("workspace resolves");
        assert_eq!(
            stdout,
            format!(
                "{}\n<-S>\n<{}>\n<-f>\n<{}>\n<-2>\n<-u>\n<display-message>\n<value\\;>\n<PATH={}>\n<TMUX=unset>\n<TMUX_PANE=unset>\n<TMUX_TMPDIR=unset>\n",
                canonical_workspace.display(),
                workspace.path().join("socket;").display(),
                workspace.path().join("config;").display(),
                bin.display(),
            )
        );
        core.shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test]
    async fn removed_captured_cwd_is_not_misreported_as_a_missing_executable() {
        let workspace = tempfile::tempdir().expect("temporary directory");
        let working_directory = workspace.path().join("sentinel-deleted-cwd");
        std::fs::create_dir(&working_directory).expect("captured cwd exists initially");
        let executable = std::env::current_exe().expect("absolute test executable");
        let configuration = CoreConfiguration::resolve(
            &SocketSelection::Path(workspace.path().join("socket")),
            None,
            None,
            executable.into_os_string(),
            Duration::from_secs(5),
            context(
                Some(&working_directory),
                Some(OsStr::new("/captured/bin")),
                None,
                None,
                Some(Path::new("/fallback")),
            ),
        )
        .expect("captured configuration resolves");
        let core = Core::new(configuration);
        std::fs::remove_dir(&working_directory).expect("captured cwd is removed");

        let error = core
            .execute(Command::new("display-message"))
            .await
            .expect_err("spawn fails when its captured cwd disappears");
        assert!(matches!(error, Error::Spawn { .. }));
        let mut source: Option<&dyn StdError> = Some(&error);
        while let Some(current) = source {
            assert!(!current.to_string().contains("sentinel-deleted-cwd"));
            assert!(!format!("{current:?}").contains("sentinel-deleted-cwd"));
            source = current.source();
        }
        core.shutdown().await.expect("shutdown remains clean");
    }
}
