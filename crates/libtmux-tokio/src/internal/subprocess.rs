use std::any::Any;
use std::collections::{HashMap, hash_map::Entry};
use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::process::{Child as StdChild, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, ChildStdout};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{Notify, OwnedSemaphorePermit, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::limits::{DispatchLimits, OutputLimits};

#[cfg(feature = "tracing")]
use tracing::instrument::{Instrument as _, WithSubscriber as _};

use crate::Error;
use crate::command::{CommandRequest, CommandResult, CommandSummary, ProcessStatus, RequestId};
use crate::internal::executor::{DispatchFuture, DispatchSpan, Executor, ShutdownFuture};
use crate::internal::operation::{CollectedPrefix, OperationContext};
use crate::internal::process::{
    LaunchContext, ProcessAdmission, ProcessGroupGuard, validate_request,
};

#[derive(Clone)]
pub(crate) struct SubprocessExecutor {
    configuration: Arc<Configuration>,
    shared: Arc<Shared>,
}

#[derive(Clone)]
struct Configuration {
    launch: LaunchContext,
    timeout: Duration,
    output_limits: OutputLimits,
    #[cfg(feature = "test-support")]
    synchronous_reap_on_supervisor_drop: bool,
    #[cfg(test)]
    hooks: TestHooks,
}

struct Shared {
    lifecycle: Mutex<Lifecycle>,
    empty: Notify,
    /// How many dispatches may hold a tmux client process at once.
    ///
    /// Each dispatch is a process, two pipes, and two reader tasks. Without a
    /// ceiling a caller's own fan-out becomes the machine's problem, and tmux
    /// serializes on the far side regardless, so the extra clients buy
    /// queueing rather than throughput.
    admission: ProcessAdmission,
}

struct Lifecycle {
    accepting: bool,
    entries: HashMap<RequestId, watch::Sender<bool>>,
}

#[derive(Clone)]
struct RequestContext {
    request_id: RequestId,
    command: CommandSummary,
    timeout: Duration,
    deadline: Option<Instant>,
    operation: Option<OperationContext>,
}

impl RequestContext {
    fn request_id(&self) -> u64 {
        self.request_id.get()
    }

    fn interrupted(&self, error: Error) -> Error {
        match &self.operation {
            Some(operation) => operation.interrupted(error),
            None => error,
        }
    }

    fn before_dispatch(&self, error: Error) -> Error {
        if self.operation.is_some() {
            OperationContext::before_dispatch(error)
        } else {
            error
        }
    }

    fn cleanup_failed(&self, primary: Error, cleanup: Error) -> Error {
        if self.operation.is_some() {
            Error::OperationAndCleanup {
                primary: Box::new(primary),
                cleanup: Box::new(cleanup),
            }
        } else {
            cleanup
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReaderFailure {
    Error,
    #[cfg(feature = "tracing")]
    Panic,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PipeRegistrationFailure {
    Stdout,
    Stderr,
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct TestHooks {
    after_reservation_reached: Option<Arc<std::sync::Barrier>>,
    after_reservation_release: Option<Arc<std::sync::Barrier>>,
    after_spawn_reached: Option<Arc<std::sync::Barrier>>,
    after_spawn_release: Option<Arc<std::sync::Barrier>>,
    spawned_pid: Option<Arc<std::sync::atomic::AtomicU32>>,
    pipe_registration_failure: Option<PipeRegistrationFailure>,
    reader_failure: Option<ReaderFailure>,
    reader_failure_release: Option<Arc<Notify>>,
    wait_failure_release: Option<Arc<Notify>>,
    supervisor_failure_reached: Option<Arc<Notify>>,
    supervisor_failure_release: Option<Arc<Notify>>,
}

impl SubprocessExecutor {
    /// Wait for room to run, or say the server is full.
    async fn acquire_permit(
        &self,
        context: &RequestContext,
    ) -> Result<OwnedSemaphorePermit, Error> {
        self.shared
            .admission
            .acquire(context.request_id, &context.command, context.deadline)
            .await
    }

    /// Replace how many dispatches may run at once.
    pub(crate) fn with_dispatch_limits(mut self, limits: DispatchLimits) -> Self {
        self.shared = Arc::new(Shared {
            lifecycle: Mutex::new(Lifecycle {
                accepting: true,
                entries: HashMap::new(),
            }),
            empty: Notify::new(),
            admission: ProcessAdmission::new(limits.max_in_flight, limits.acquire_timeout),
        });
        self
    }

    /// Replace the byte budgets each dispatch's output is read under.
    pub(crate) fn with_output_limits(mut self, limits: OutputLimits) -> Self {
        Arc::make_mut(&mut self.configuration).output_limits = limits;
        self
    }

    pub(crate) fn new(executable: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            configuration: Arc::new(Configuration {
                launch: LaunchContext::new(executable),
                timeout,
                output_limits: OutputLimits::default(),
                #[cfg(feature = "test-support")]
                synchronous_reap_on_supervisor_drop: false,
                #[cfg(test)]
                hooks: TestHooks::default(),
            }),
            shared: Arc::new(Shared {
                lifecycle: Mutex::new(Lifecycle {
                    accepting: true,
                    entries: HashMap::new(),
                }),
                empty: Notify::new(),
                admission: ProcessAdmission::new(DispatchLimits::DEFAULT_IN_FLIGHT, None),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_environment(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        let mut configuration = (*self.configuration).clone();
        configuration.launch = configuration.launch.with_environment(key, value);
        self.configuration = Arc::new(configuration);
        self
    }

    pub(crate) fn with_launch_context(mut self, launch: LaunchContext) -> Self {
        let mut configuration = (*self.configuration).clone();
        configuration.launch = launch;
        self.configuration = Arc::new(configuration);
        self
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn with_synchronous_reap_on_supervisor_drop(mut self, enabled: bool) -> Self {
        let mut configuration = (*self.configuration).clone();
        configuration.synchronous_reap_on_supervisor_drop = enabled;
        self.configuration = Arc::new(configuration);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_test_hooks(mut self, hooks: TestHooks) -> Self {
        let mut configuration = (*self.configuration).clone();
        configuration.hooks = hooks;
        self.configuration = Arc::new(configuration);
        self
    }

    #[cfg(test)]
    pub(crate) fn active_request_count(&self) -> usize {
        lock_lifecycle(&self.shared).entries.len()
    }

    #[cfg(test)]
    fn is_accepting(&self) -> bool {
        lock_lifecycle(&self.shared).accepting
    }

    #[allow(
        clippy::too_many_lines,
        reason = "admission, spawn, and supervisor handoff form one atomic lifecycle sequence"
    )]
    async fn run(self, request: CommandRequest) -> Result<CommandResult, Error> {
        self.run_operation(request, None).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one owner supervises admission, spawn and cleanup"
    )]
    async fn run_operation(
        self,
        request: CommandRequest,
        operation: Option<OperationContext>,
    ) -> Result<CommandResult, Error> {
        let request_id = request.request_id();
        validate_request(&self.configuration.launch, &request)?;
        let context = RequestContext {
            request_id,
            command: request.summary().clone(),
            timeout: self.configuration.timeout,
            deadline: operation.as_ref().map_or_else(
                || {
                    if request.is_unbounded() {
                        None
                    } else {
                        Instant::now().checked_add(self.configuration.timeout)
                    }
                },
                OperationContext::deadline,
            ),
            operation,
        };
        trace_requested(&context);

        // Parked admission leaves ordinary capacity for signal and unlock.
        let parked_permit = request.parked_reservation().cloned();
        let permit = if parked_permit.is_some() {
            None
        } else {
            match self.acquire_permit(&context).await {
                Ok(permit) => Some(permit),
                Err(error) => {
                    trace_failed(&context, &error);
                    return Err(error);
                }
            }
        };

        let (cancellation_sender, cancellation_receiver) = watch::channel(false);
        let admission_error = {
            let mut lifecycle = lock_lifecycle(&self.shared);
            if lifecycle.accepting {
                match lifecycle.entries.entry(context.request_id) {
                    Entry::Vacant(entry) => {
                        entry.insert(cancellation_sender.clone());
                        None
                    }
                    Entry::Occupied(_) => Some(Error::duplicate_request(
                        context.request_id(),
                        context.command.clone(),
                    )),
                }
            } else {
                Some(Error::executor_shutdown(
                    context.request_id(),
                    context.command.clone(),
                ))
            }
        };
        if let Some(error) = admission_error {
            trace_failed(&context, &error);
            return Err(error);
        }
        let registry = RegistryGuard::new(Arc::clone(&self.shared), context.request_id);

        #[cfg(test)]
        wait_at_barriers(
            self.configuration.hooks.after_reservation_reached.as_ref(),
            self.configuration.hooks.after_reservation_release.as_ref(),
        );

        if context
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            drop(registry);
            let error = Error::timeout(
                context.request_id(),
                context.command.clone(),
                context.timeout,
            );
            trace_failed(&context, &error);
            return Err(error);
        }

        let stdout_discarded = request.is_stdout_discarded();
        let mut process = self.configuration.launch.command(request.argv());
        process
            .stdin(Stdio::null())
            .stdout(if stdout_discarded {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stderr(Stdio::piped());

        let caller_cancellation = CancellationGuard::new(cancellation_sender);
        let (result_sender, result_receiver) = oneshot::channel();
        let supervisor = spawn_and_supervise(
            process,
            stdout_discarded,
            Arc::clone(&self.configuration),
            permit,
            parked_permit,
            registry,
            cancellation_receiver,
            context.clone(),
            result_sender,
        );
        #[cfg(feature = "tracing")]
        tokio::spawn(supervisor.in_current_span().with_current_subscriber());
        #[cfg(not(feature = "tracing"))]
        tokio::spawn(supervisor);

        await_result(result_receiver, caller_cancellation, context).await
    }
}

impl Executor for SubprocessExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        DispatchSpan::new(&request, "subprocess").run(self.clone().run(request))
    }

    fn execute_operation(
        &self,
        request: CommandRequest,
        operation: OperationContext,
    ) -> DispatchFuture {
        DispatchSpan::new(&request, "subprocess")
            .run(self.clone().run_operation(request, Some(operation)))
    }

    fn shutdown(&self) -> ShutdownFuture {
        let shared = Arc::clone(&self.shared);
        ShutdownFuture::new(async move {
            {
                let mut lifecycle = lock_lifecycle(&shared);
                lifecycle.accepting = false;
                for sender in lifecycle.entries.values() {
                    let _ = sender.send(true);
                }
            }
            shared.admission.close();

            loop {
                let notified = shared.empty.notified();
                if lock_lifecycle(&shared).entries.is_empty() {
                    return Ok(());
                }
                notified.await;
            }
        })
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "startup transfers the existing registry, admission and result owner together"
)]
async fn spawn_and_supervise(
    mut process: tokio::process::Command,
    stdout_discarded: bool,
    configuration: Arc<Configuration>,
    permit: Option<OwnedSemaphorePermit>,
    parked_permit: Option<Arc<OwnedSemaphorePermit>>,
    registry: RegistryGuard,
    cancellation: watch::Receiver<bool>,
    context: RequestContext,
    result_sender: oneshot::Sender<Result<CommandResult, Error>>,
) {
    let spawn_context = context.clone();
    let spawn_configuration = Arc::clone(&configuration);
    let spawn_cancellation = cancellation.clone();
    let ownership = tokio::task::spawn_blocking(move || {
        if *spawn_cancellation.borrow() {
            return Err(spawn_context.before_dispatch(Error::executor_shutdown(
                spawn_context.request_id(),
                spawn_context.command.clone(),
            )));
        }
        if spawn_context
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(spawn_context.before_dispatch(Error::timeout(
                spawn_context.request_id(),
                spawn_context.command.clone(),
                spawn_context.timeout,
            )));
        }
        if spawn_context.operation.is_some() || parked_permit.is_some() {
            return start_native_child(
                process,
                stdout_discarded,
                &spawn_configuration,
                Reservation {
                    _permit: permit,
                    _parked_permit: parked_permit,
                    registry,
                },
                &spawn_context,
                &spawn_cancellation,
            );
        }
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(source) => {
                let executable_not_found = (source.kind() == io::ErrorKind::NotFound
                    && spawn_configuration
                        .launch
                        .current_dir()
                        .is_none_or(std::path::Path::is_dir))
                    || spawn_configuration.launch.executable_missing_from_path();
                let error = Error::spawn(
                    spawn_context.request_id(),
                    spawn_context.command.clone(),
                    source,
                    executable_not_found,
                );
                return Err(error);
            }
        };
        let process_group = ProcessGroupGuard::new(child.id());
        #[cfg(test)]
        if let (Some(spawned_pid), Some(child_id)) =
            (&spawn_configuration.hooks.spawned_pid, child.id())
        {
            spawned_pid.store(child_id, std::sync::atomic::Ordering::SeqCst);
        }
        let readers = ReaderTasks::spawn(
            child.stdout.take(),
            child.stderr.take(),
            stdout_discarded,
            spawn_configuration.output_limits,
            spawn_context
                .operation
                .as_ref()
                .and_then(OperationContext::stdout_prefix),
            #[cfg(test)]
            &spawn_configuration.hooks,
        );
        let ownership = ChildOwnership {
            process_group,
            child: SupervisedChild::Tokio(child),
            readers,
            #[cfg(feature = "test-support")]
            synchronous_reap_on_drop: spawn_configuration.synchronous_reap_on_supervisor_drop,
            _reservation: Some(Reservation {
                _permit: permit,
                _parked_permit: parked_permit,
                registry,
            }),
        };
        #[cfg(test)]
        wait_at_barriers(
            spawn_configuration.hooks.after_spawn_reached.as_ref(),
            spawn_configuration.hooks.after_spawn_release.as_ref(),
        );
        Ok(ownership)
    })
    .await;
    match ownership {
        Ok(Ok(ownership)) => {
            supervise_outer(
                ownership,
                cancellation,
                context,
                result_sender,
                #[cfg(test)]
                configuration.hooks.clone(),
            )
            .await;
        }
        Ok(Err(error)) => {
            trace_failed(&context, &error);
            let _ = result_sender.send(Err(error));
        }
        Err(_) => {
            let error = context.interrupted(Error::supervisor_lost(
                context.request_id(),
                context.command.clone(),
            ));
            trace_failed(&context, &error);
            let _ = result_sender.send(Err(error));
        }
    }
}

async fn await_result(
    receiver: oneshot::Receiver<Result<CommandResult, Error>>,
    mut cancellation: CancellationGuard,
    context: RequestContext,
) -> Result<CommandResult, Error> {
    let completion = if let Some(deadline) = context
        .operation
        .as_ref()
        .and_then(OperationContext::deadline)
    {
        let latest = deadline
            .checked_add(libtmux_core::supervision::CLEANUP_ALLOWANCE)
            .unwrap_or(deadline);
        if let Ok(completion) = tokio::time::timeout_at(latest, receiver).await {
            completion
        } else {
            let source = context.interrupted(Error::timeout(
                context.request_id(),
                context.command.clone(),
                context.timeout,
            ));
            return Err(Error::OperationCleanupPending {
                source: Box::new(source),
            });
        }
    } else {
        receiver.await
    };
    match completion {
        Ok(result) => {
            cancellation.disarm();
            result
        }
        Err(_) => Err(context.interrupted(Error::supervisor_lost(
            context.request_id(),
            context.command.clone(),
        ))),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "child ownership must cover pipe registration and startup cleanup"
)]
fn start_native_child(
    mut process: tokio::process::Command,
    stdout_discarded: bool,
    configuration: &Configuration,
    reservation: Reservation,
    context: &RequestContext,
    cancellation: &watch::Receiver<bool>,
) -> Result<ChildOwnership, Error> {
    // Registration can fail after spawn, so the child and its reservation
    // must have an independent owner before any pipe enters Tokio.
    let reaper = native_reaper().map_err(|source| {
        context.before_dispatch(Error::spawn(
            context.request_id(),
            context.command.clone(),
            source,
            false,
        ))
    })?;
    let changed = signal(SignalKind::child()).map_err(|source| {
        context.before_dispatch(Error::spawn(
            context.request_id(),
            context.command.clone(),
            source,
            false,
        ))
    })?;
    if *cancellation.borrow() {
        return Err(context.before_dispatch(Error::executor_shutdown(
            context.request_id(),
            context.command.clone(),
        )));
    }
    if context
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(context.before_dispatch(Error::timeout(
            context.request_id(),
            context.command.clone(),
            context.timeout,
        )));
    }
    let child = process.as_std_mut().spawn().map_err(|source| {
        let executable_not_found = (source.kind() == io::ErrorKind::NotFound
            && configuration
                .launch
                .current_dir()
                .is_none_or(std::path::Path::is_dir))
            || configuration.launch.executable_missing_from_path();
        context.before_dispatch(Error::spawn(
            context.request_id(),
            context.command.clone(),
            source,
            executable_not_found,
        ))
    })?;
    let child_id = child.id();
    let mut child = NativeChild {
        child: Some(child),
        changed,
        reaper,
        reservation: Some(Box::new(reservation)),
        reaped: false,
    };
    // Locals drop in reverse order; signal the group before a fallback reaper
    // may release the leader's PID after a registration panic.
    let mut process_group = ProcessGroupGuard::new(Some(child_id));
    #[cfg(test)]
    if let Some(spawned_pid) = &configuration.hooks.spawned_pid {
        spawned_pid.store(child.id().unwrap_or(0), std::sync::atomic::Ordering::SeqCst);
    }
    let pipes = register_native_pipes(
        &mut child,
        #[cfg(test)]
        &configuration.hooks,
    );
    let (stdout, stderr) = match pipes {
        Ok(pipes) => pipes,
        Err((stream, source)) => {
            let primary = context.interrupted(Error::read_output(
                context.request_id(),
                context.command.clone(),
                stream,
                source.kind(),
            ));
            let group = process_group.signal_checked();
            let kill = child.start_kill();
            let waited = child.wait_blocking();
            if waited.is_ok()
                || waited
                    .as_ref()
                    .is_err_and(|source| source.raw_os_error() == Some(ErrnoValue::child()))
            {
                process_group.disarm();
            }
            return Err(
                match cleanup_result(group, kill, waited.map(|_| ()), context) {
                    Ok(()) => primary,
                    Err(cleanup) => context.cleanup_failed(primary, cleanup),
                },
            );
        }
    };
    let readers = ReaderTasks::spawn(
        stdout,
        stderr,
        stdout_discarded,
        configuration.output_limits,
        context
            .operation
            .as_ref()
            .and_then(OperationContext::stdout_prefix),
        #[cfg(test)]
        &configuration.hooks,
    );
    let ownership = ChildOwnership {
        process_group,
        child: SupervisedChild::Native(child),
        readers,
        #[cfg(feature = "test-support")]
        synchronous_reap_on_drop: configuration.synchronous_reap_on_supervisor_drop,
        _reservation: None,
    };
    #[cfg(test)]
    wait_at_barriers(
        configuration.hooks.after_spawn_reached.as_ref(),
        configuration.hooks.after_spawn_release.as_ref(),
    );
    Ok(ownership)
}

type NativePipes = (Option<ChildStdout>, Option<ChildStderr>);

fn register_native_pipes(
    child: &mut NativeChild,
    #[cfg(test)] hooks: &TestHooks,
) -> Result<NativePipes, (&'static str, io::Error)> {
    let child = child.child_mut().map_err(|source| ("stdout", source))?;
    #[cfg(test)]
    if hooks.pipe_registration_failure == Some(PipeRegistrationFailure::Stdout) {
        return Err(("stdout", io::Error::other("private-registration-failure")));
    }
    let stdout = child
        .stdout
        .take()
        .map(ChildStdout::from_std)
        .transpose()
        .map_err(|source| ("stdout", source))?;
    #[cfg(test)]
    if hooks.pipe_registration_failure == Some(PipeRegistrationFailure::Stderr) {
        return Err(("stderr", io::Error::other("private-registration-failure")));
    }
    let stderr = child
        .stderr
        .take()
        .map(ChildStderr::from_std)
        .transpose()
        .map_err(|source| ("stderr", source))?;
    Ok((stdout, stderr))
}

enum SupervisedChild {
    Tokio(Child),
    Native(NativeChild),
}

impl SupervisedChild {
    async fn wait(&mut self) -> io::Result<ExitStatus> {
        match self {
            Self::Tokio(child) => child.wait().await,
            Self::Native(child) => child.wait().await,
        }
    }

    fn start_kill(&mut self) -> io::Result<()> {
        match self {
            Self::Tokio(child) => child.start_kill(),
            Self::Native(child) => child.start_kill(),
        }
    }

    #[cfg(feature = "test-support")]
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Tokio(child) => child.try_wait(),
            Self::Native(child) => child.try_wait(),
        }
    }
}

pub(crate) struct NativeChild {
    child: Option<StdChild>,
    changed: Signal,
    reaper: mpsc::Sender<NativeReap>,
    reservation: Option<Box<dyn Any + Send>>,
    reaped: bool,
}

impl NativeChild {
    #[cfg(feature = "control-mode")]
    pub(crate) fn spawn(
        command: &mut std::process::Command,
        reservation: Box<dyn Any + Send>,
    ) -> io::Result<Self> {
        let reaper = native_reaper()?;
        let changed = signal(SignalKind::child())?;
        let child = command.spawn()?;
        Ok(Self {
            child: Some(child),
            changed,
            reaper,
            reservation: Some(reservation),
            reaped: false,
        })
    }

    #[cfg(feature = "control-mode")]
    pub(crate) fn retain(&mut self, owner: impl Any + Send) {
        self.reservation = Some(Box::new((self.reservation.take(), owner)));
    }

    #[cfg(any(test, feature = "control-mode"))]
    pub(crate) fn id(&self) -> Option<u32> {
        self.child.as_ref().map(StdChild::id)
    }

    pub(crate) fn child_mut(&mut self) -> io::Result<&mut StdChild> {
        self.child
            .as_mut()
            .ok_or_else(|| io::ErrorKind::BrokenPipe.into())
    }

    pub(crate) fn start_kill(&mut self) -> io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        self.child_mut()?.kill()
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self.child_mut()?.try_wait();
        self.reaped |= result.as_ref().is_ok_and(Option::is_some)
            || result
                .as_ref()
                .is_err_and(|source| source.raw_os_error() == Some(ErrnoValue::child()));
        result
    }

    pub(crate) async fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            self.changed
                .recv()
                .await
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
    }

    pub(crate) fn wait_blocking(&mut self) -> io::Result<ExitStatus> {
        let result = loop {
            match self.child_mut()?.wait() {
                Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
                result => break result,
            }
        };
        self.reaped |= result.is_ok()
            || result
                .as_ref()
                .is_err_and(|source| source.raw_os_error() == Some(ErrnoValue::child()));
        result
    }
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let job = NativeReap {
                child,
                _reservation: self.reservation.take(),
            };
            if let Err(failed) = self.reaper.send(job) {
                reap_native(failed.0);
            }
        }
    }
}

struct NativeReap {
    child: StdChild,
    _reservation: Option<Box<dyn Any + Send>>,
}

fn native_reaper() -> io::Result<mpsc::Sender<NativeReap>> {
    static REAPER: OnceLock<Mutex<Option<mpsc::Sender<NativeReap>>>> = OnceLock::new();
    let mut slot = REAPER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(sender) = slot.as_ref() {
        return Ok(sender.clone());
    }
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("libtmux-tokio-reaper".to_owned())
        .spawn(move || {
            for job in receiver {
                reap_native(job);
            }
        })?;
    *slot = Some(sender.clone());
    Ok(sender)
}

fn reap_native(mut job: NativeReap) {
    loop {
        match job.child.wait() {
            Ok(_) => return,
            Err(source) if source.raw_os_error() == Some(ErrnoValue::child()) => return,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

struct ChildOwnership {
    // This guard must drop before `child`; the unreaped leader anchors the PGID.
    process_group: ProcessGroupGuard,
    child: SupervisedChild,
    readers: ReaderTasks,
    #[cfg(feature = "test-support")]
    synchronous_reap_on_drop: bool,
    // Both admission lanes follow the process and readers into cleanup.
    _reservation: Option<Reservation>,
}

struct Reservation {
    _permit: Option<OwnedSemaphorePermit>,
    _parked_permit: Option<Arc<OwnedSemaphorePermit>>,
    // Registry removal follows child and reader cleanup.
    #[allow(
        dead_code,
        reason = "RAII removal must run after process and reader cleanup"
    )]
    registry: RegistryGuard,
}

impl Drop for ChildOwnership {
    fn drop(&mut self) {
        #[cfg(feature = "test-support")]
        if self.synchronous_reap_on_drop && self.process_group.is_armed() {
            self.readers.abort();
            self.process_group.signal();
            let _ = self.child.start_kill();
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => {
                        self.process_group.disarm();
                        break;
                    }
                    Ok(None) => {
                        std::thread::yield_now();
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                        std::thread::yield_now();
                    }
                    Err(error) if error.raw_os_error() == Some(ErrnoValue::child()) => {
                        self.process_group.disarm();
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

async fn supervise_outer(
    mut ownership: ChildOwnership,
    mut cancellation: watch::Receiver<bool>,
    context: RequestContext,
    result_sender: oneshot::Sender<Result<CommandResult, Error>>,
    #[cfg(test)] hooks: TestHooks,
) {
    let outcome = {
        let inner = supervise_inner(
            &mut ownership.child,
            &mut ownership.readers,
            &mut ownership.process_group,
            &mut cancellation,
            &context,
            #[cfg(test)]
            &hooks,
        );
        CatchUnwindFuture::new(inner).await
    };

    let result = match outcome {
        Ok(InnerOutcome::Complete {
            status,
            stdout,
            stderr,
        }) => Ok(CommandResult::new(
            context.request_id,
            context.command.clone(),
            status,
            stdout,
            stderr,
        )),
        Ok(InnerOutcome::Failed(error)) => {
            let cleanup = cleanup_child(
                &mut ownership.child,
                &mut ownership.readers,
                &mut ownership.process_group,
                &context,
            )
            .await;
            // Reader cancellation settles the collected prefix before its receipt is decoded.
            let error = context.interrupted(error);
            match cleanup {
                Ok(()) => Err(error),
                Err(wait_error) => Err(context.cleanup_failed(error, wait_error)),
            }
        }
        Err(()) => {
            let primary = Error::supervisor_lost(context.request_id(), context.command.clone());
            let cleanup = cleanup_child(
                &mut ownership.child,
                &mut ownership.readers,
                &mut ownership.process_group,
                &context,
            )
            .await;
            let primary = context.interrupted(primary);
            match cleanup {
                Ok(()) => Err(primary),
                Err(wait_error) => Err(context.cleanup_failed(primary, wait_error)),
            }
        }
    };

    drop(ownership);
    trace_finished(&context, &result);
    let _ = result_sender.send(result);
}

async fn supervise_inner(
    child: &mut SupervisedChild,
    readers: &mut ReaderTasks,
    process_group: &mut ProcessGroupGuard,
    cancellation: &mut watch::Receiver<bool>,
    context: &RequestContext,
    #[cfg(test)] hooks: &TestHooks,
) -> InnerOutcome {
    #[cfg(test)]
    inject_supervisor_failure(hooks).await;

    #[cfg(test)]
    if let Some(source) = inject_wait_failure(hooks).await {
        return InnerOutcome::Failed(Error::wait_child(
            context.request_id(),
            context.command.clone(),
            source,
        ));
    }

    let deadline = context.deadline;
    let (stdout, stderr) = match drain_readers(readers, cancellation, deadline, context).await {
        Ok(streams) => streams,
        Err(error) => return InnerOutcome::Failed(error),
    };

    let wait_result = loop {
        let outcome = tokio::select! {
            result = async {
                process_group.finish_group().await?;
                child.wait().await
            } => Some(result),
            () = cancelled(cancellation) => None,
            () = deadline_elapsed(deadline) => {
                return InnerOutcome::Failed(Error::timeout(
                    context.request_id(),
                    context.command.clone(),
                    context.timeout,
                ));
            }
        };
        match outcome {
            Some(Err(source)) if source.kind() == io::ErrorKind::Interrupted => {}
            outcome => break outcome,
        }
    };

    match wait_result {
        Some(Ok(status)) => {
            process_group.disarm();
            InnerOutcome::Complete {
                status: ProcessStatus::from_exit_status(status),
                stdout,
                stderr,
            }
        }
        Some(Err(source)) => {
            if source.raw_os_error() == Some(ErrnoValue::child()) {
                process_group.disarm();
            }
            InnerOutcome::Failed(Error::wait_child(
                context.request_id(),
                context.command.clone(),
                source,
            ))
        }
        None => InnerOutcome::Failed(Error::executor_shutdown(
            context.request_id(),
            context.command.clone(),
        )),
    }
}

enum InnerOutcome {
    Complete {
        status: ProcessStatus,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Failed(Error),
}

async fn drain_readers(
    readers: &mut ReaderTasks,
    cancellation: &mut watch::Receiver<bool>,
    deadline: Option<Instant>,
    context: &RequestContext,
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut stdout = None;
    let mut stderr = None;

    while stdout.is_none() || stderr.is_none() {
        let stdout_reader = &mut readers.stdout;
        let stderr_reader = &mut readers.stderr;
        tokio::select! {
            result = wait_reader(stdout_reader), if stdout.is_none() => {
                *stdout_reader = None;
                stdout = Some(map_reader_result(result, "stdout", context)?);
            }
            result = wait_reader(stderr_reader), if stderr.is_none() => {
                *stderr_reader = None;
                stderr = Some(map_reader_result(result, "stderr", context)?);
            }
            () = cancelled(cancellation) => {
                return Err(Error::executor_shutdown(
                    context.request_id(),
                    context.command.clone(),
                ));
            }
            () = deadline_elapsed(deadline) => {
                return Err(Error::timeout(
                    context.request_id(),
                    context.command.clone(),
                    context.timeout,
                ));
            }
        }
    }

    let stdout = stdout.ok_or_else(|| {
        Error::read_output(
            context.request_id(),
            context.command.clone(),
            "stdout",
            io::ErrorKind::Other,
        )
    })?;
    let stderr = stderr.ok_or_else(|| {
        Error::read_output(
            context.request_id(),
            context.command.clone(),
            "stderr",
            io::ErrorKind::Other,
        )
    })?;
    Ok((stdout, stderr))
}

async fn deadline_elapsed(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn wait_reader(
    reader: &mut Option<JoinHandle<Result<Vec<u8>, io::Error>>>,
) -> Result<Result<Vec<u8>, io::Error>, tokio::task::JoinError> {
    match reader.as_mut() {
        Some(reader) => reader.await,
        None => Ok(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "output reader is unavailable",
        ))),
    }
}

fn map_reader_result(
    result: Result<Result<Vec<u8>, io::Error>, tokio::task::JoinError>,
    stream: &'static str,
    context: &RequestContext,
) -> Result<Vec<u8>, Error> {
    match result {
        Ok(Ok(bytes)) => Ok(bytes),
        // A budget overrun is a decision the caller can act on -- ask tmux for
        // less, or raise the budget -- so it is not folded into the generic
        // "the pipe failed" case.
        Ok(Err(source))
            if source
                .get_ref()
                .is_some_and(|inner| inner.downcast_ref::<OutputTooLarge>().is_some()) =>
        {
            let limit = source
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<OutputTooLarge>())
                .map_or(0, |exceeded| exceeded.limit);
            Err(Error::OutputLimitExceeded {
                request_id: context.request_id(),
                command: context.command.clone(),
                stream,
                limit,
            })
        }
        Ok(Err(source)) => Err(Error::read_output(
            context.request_id(),
            context.command.clone(),
            stream,
            source.kind(),
        )),
        Err(_) => Err(Error::read_output(
            context.request_id(),
            context.command.clone(),
            stream,
            io::ErrorKind::Other,
        )),
    }
}

async fn cleanup_child(
    child: &mut SupervisedChild,
    readers: &mut ReaderTasks,
    process_group: &mut ProcessGroupGuard,
    context: &RequestContext,
) -> Result<(), Error> {
    readers.abort_and_join().await;
    let group = if context.operation.is_some() {
        process_group.signal_checked()
    } else {
        process_group.signal();
        Ok(())
    };
    let kill = child.start_kill();
    let waited = loop {
        match child.wait().await {
            Ok(_) => {
                process_group.disarm();
                break Ok(());
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                if source.raw_os_error() == Some(ErrnoValue::child()) {
                    process_group.disarm();
                }
                break Err(source);
            }
        }
    };
    if context.operation.is_some() {
        cleanup_result(group, kill, waited, context)
    } else {
        waited.map_err(|source| {
            Error::wait_child(context.request_id(), context.command.clone(), source)
        })
    }
}

fn cleanup_result(
    group: io::Result<()>,
    kill: io::Result<()>,
    waited: io::Result<()>,
    context: &RequestContext,
) -> Result<(), Error> {
    let failures: Vec<_> = [
        ("process-group signal", group),
        ("direct-child signal", kill),
        ("child wait", waited),
    ]
    .into_iter()
    .filter_map(|(stage, result)| result.err().map(|source| (stage, source.kind())))
    .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::wait_child(
            context.request_id(),
            context.command.clone(),
            io::Error::other(
                failures
                    .iter()
                    .map(|(stage, kind)| format!("{stage} failed ({kind:?})"))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        ))
    }
}

async fn cancelled(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}

struct ReaderTasks {
    stdout: Option<JoinHandle<Result<Vec<u8>, io::Error>>>,
    stderr: Option<JoinHandle<Result<Vec<u8>, io::Error>>>,
}

impl ReaderTasks {
    fn spawn(
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
        stdout_discarded: bool,
        limits: OutputLimits,
        prefix: Option<CollectedPrefix>,
        #[cfg(test)] hooks: &TestHooks,
    ) -> Self {
        #[cfg(test)]
        let stdout = if stdout_discarded {
            tokio::spawn(std::future::ready(Ok(Vec::new())))
        } else {
            spawn_reader(
                stdout,
                limits.max_stdout_bytes,
                prefix,
                hooks.reader_failure,
                hooks.reader_failure_release.clone(),
            )
        };
        #[cfg(not(test))]
        let stdout = if stdout_discarded {
            tokio::spawn(std::future::ready(Ok(Vec::new())))
        } else {
            spawn_reader(stdout, limits.max_stdout_bytes, prefix)
        };

        #[cfg(test)]
        let stderr = spawn_reader(stderr, limits.max_stderr_bytes, None, None, None);
        #[cfg(not(test))]
        let stderr = spawn_reader(stderr, limits.max_stderr_bytes, None);

        Self {
            stdout: Some(stdout),
            stderr: Some(stderr),
        }
    }

    async fn abort_and_join(&mut self) {
        if let Some(reader) = self.stdout.take() {
            reader.abort();
            let _ = reader.await;
        }
        if let Some(reader) = self.stderr.take() {
            reader.abort();
            let _ = reader.await;
        }
    }

    #[cfg(feature = "test-support")]
    fn abort(&self) {
        if let Some(reader) = &self.stdout {
            reader.abort();
        }
        if let Some(reader) = &self.stderr {
            reader.abort();
        }
    }
}

#[cfg_attr(
    test,
    allow(
        clippy::panic,
        reason = "the panic branch exercises JoinError sanitization"
    )
)]
fn spawn_reader<R>(
    reader: Option<R>,
    limit: usize,
    prefix: Option<CollectedPrefix>,
    #[cfg(test)] failure: Option<ReaderFailure>,
    #[cfg(test)] failure_release: Option<Arc<Notify>>,
) -> JoinHandle<Result<Vec<u8>, io::Error>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        #[cfg(test)]
        if let Some(failure) = failure {
            if let Some(release) = failure_release {
                release.notified().await;
            }
            match failure {
                ReaderFailure::Error => {
                    return Err(io::Error::other("injected reader failure"));
                }
                #[cfg(feature = "tracing")]
                ReaderFailure::Panic => panic!("sentinel-reader-panic"),
            }
        }

        let Some(mut reader) = reader else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "child output pipe is unavailable",
            ));
        };

        // Read one byte more than the budget. `read_to_end` on an unbounded
        // pipe is how a `capture-pane` over a large history, or a `run-shell`
        // that never stops printing, becomes the process's memory ceiling
        // rather than tmux's. Taking `limit + 1` distinguishes "exactly at the
        // budget" from "over it" without a second read.
        let mut bytes = Vec::new();
        let overflow = limit.saturating_add(1);
        if let Some(prefix) = prefix {
            let mut scratch = [0; 8192];
            loop {
                let allowance = overflow.saturating_sub(bytes.len()).min(scratch.len());
                let count = reader.read(&mut scratch[..allowance]).await?;
                if count == 0 {
                    return Ok(bytes);
                }
                bytes.extend_from_slice(&scratch[..count]);
                prefix
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(&scratch[..count]);
                if bytes.len() > limit {
                    return Err(io::Error::new(
                        io::ErrorKind::FileTooLarge,
                        OutputTooLarge { limit },
                    ));
                }
                tokio::task::yield_now().await;
            }
        }
        let mut bounded = reader.take(overflow as u64);
        bounded.read_to_end(&mut bytes).await?;
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                OutputTooLarge { limit },
            ));
        }

        Ok(bytes)
    })
}

/// Marker for a read that ran past its budget.
///
/// Carried as the source of an `io::Error` so the executor can tell this from
/// an ordinary pipe failure and report the budget that was exceeded.
#[derive(Debug)]
pub(crate) struct OutputTooLarge {
    pub(crate) limit: usize,
}

impl fmt::Display for OutputTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "output exceeded the {} byte budget", self.limit)
    }
}

impl std::error::Error for OutputTooLarge {}

struct RegistryGuard {
    shared: Arc<Shared>,
    request_id: RequestId,
    armed: bool,
}

impl RegistryGuard {
    fn new(shared: Arc<Shared>, request_id: RequestId) -> Self {
        Self {
            shared,
            request_id,
            armed: true,
        }
    }

    fn remove(&mut self) {
        if self.armed {
            lock_lifecycle(&self.shared)
                .entries
                .remove(&self.request_id);
            self.armed = false;
            self.shared.empty.notify_waiters();
        }
    }
}

impl Drop for RegistryGuard {
    fn drop(&mut self) {
        self.remove();
    }
}

struct CancellationGuard {
    sender: watch::Sender<bool>,
    armed: bool,
}

impl CancellationGuard {
    fn new(sender: watch::Sender<bool>) -> Self {
        Self {
            sender,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.sender.send(true);
        }
    }
}

struct CatchUnwindFuture<F> {
    future: Pin<Box<F>>,
}

impl<F> CatchUnwindFuture<F> {
    fn new(future: F) -> Self {
        Self {
            future: Box::pin(future),
        }
    }
}

impl<F: Future> Future for CatchUnwindFuture<F> {
    type Output = Result<F::Output, ()>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match catch_unwind(AssertUnwindSafe(|| this.future.as_mut().poll(context))) {
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Ok(Poll::Pending) => Poll::Pending,
            Err(payload) => {
                discard_panic_payload(payload);
                Poll::Ready(Err(()))
            }
        }
    }
}

fn discard_panic_payload(_payload: Box<dyn Any + Send>) {}

fn lock_lifecycle(shared: &Shared) -> MutexGuard<'_, Lifecycle> {
    shared
        .lifecycle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct ErrnoValue;

impl ErrnoValue {
    fn child() -> i32 {
        rustix::io::Errno::CHILD.raw_os_error()
    }
}

#[cfg(test)]
fn wait_at_barriers(
    reached: Option<&Arc<std::sync::Barrier>>,
    release: Option<&Arc<std::sync::Barrier>>,
) {
    if let Some(reached) = reached {
        reached.wait();
    }
    if let Some(release) = release {
        release.wait();
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
async fn inject_supervisor_failure(hooks: &TestHooks) {
    if let Some(release) = &hooks.supervisor_failure_release {
        if let Some(reached) = &hooks.supervisor_failure_reached {
            reached.notify_one();
        }
        release.notified().await;
        panic!("sentinel-supervisor-panic");
    }
}

#[cfg(test)]
async fn inject_wait_failure(hooks: &TestHooks) -> Option<io::Error> {
    let release = hooks.wait_failure_release.as_ref()?;
    release.notified().await;
    Some(io::Error::from(io::ErrorKind::Other))
}

#[cfg(feature = "tracing")]
fn trace_requested(context: &RequestContext) {
    tracing::debug!(
        request_id = context.request_id(),
        command = %context.command,
        "tmux command requested"
    );
}

#[cfg(not(feature = "tracing"))]
fn trace_requested(_context: &RequestContext) {}

#[cfg(feature = "tracing")]
fn trace_failed(context: &RequestContext, error: &Error) {
    tracing::debug!(
        request_id = context.request_id(),
        command = %context.command,
        error = %error,
        "tmux command failed"
    );
}

#[cfg(not(feature = "tracing"))]
fn trace_failed(_context: &RequestContext, _error: &Error) {}

#[cfg(feature = "tracing")]
fn trace_finished(context: &RequestContext, result: &Result<CommandResult, Error>) {
    match result {
        Ok(result) => tracing::debug!(
            request_id = context.request_id(),
            command = %context.command,
            success = result.success(),
            stdout_len = result.stdout().len(),
            stderr_len = result.stderr().len(),
            "tmux command finished"
        ),
        Err(error) => trace_failed(context, error),
    }
}

#[cfg(not(feature = "tracing"))]
fn trace_finished(_context: &RequestContext, _result: &Result<CommandResult, Error>) {}

#[cfg(test)]
mod tests;
