use std::any::Any;
use std::collections::{HashMap, hash_map::Entry};
use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use event_listener::Event;
use futures_channel::oneshot;
use futures_io::AsyncRead;
use futures_util::FutureExt as _;
use futures_util::io::AsyncReadExt as _;

#[cfg(test)]
use tokio::sync::Notify;

use crate::driver::{
    AsyncIoRuntime, Process, Reader, Runtime, RuntimeExt as _, TaskError, TaskHandle,
};
use crate::limits::{DispatchLimits, OutputLimits};

#[cfg(feature = "tracing")]
use tracing::instrument::{Instrument as _, WithSubscriber as _};

use crate::Error;
use crate::command::{CommandRequest, CommandResult, CommandSummary, ProcessStatus, RequestId};
use crate::internal::executor::{DispatchFuture, DispatchSpan, Executor, ShutdownFuture};
use crate::internal::process::{
    CleanupFailure, LaunchContext, OwnedSemaphorePermit, ProcessAdmission, ProcessGroupGuard,
    terminate_child, validate_request,
};
use crate::internal::watch;

#[derive(Clone)]
pub(crate) struct SubprocessExecutor {
    configuration: Arc<Configuration>,
    shared: Arc<Shared>,
}

#[derive(Clone)]
struct Configuration {
    runtime: Arc<dyn Runtime>,
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
    empty: Event,
    /// How many dispatches may hold a tmux client process at once.
    ///
    /// Each dispatch is a process, two pipes, and two reader tasks. Without a
    /// ceiling a caller's own fan-out becomes the machine's problem, and tmux
    /// serializes on the far side regardless, so the extra clients buy
    /// queueing rather than throughput.
    admission: ProcessAdmission,
    wait_admission: ProcessAdmission,
}

struct Lifecycle {
    accepting: bool,
    entries: HashMap<RequestId, watch::Sender<bool>>,
    cleanup_failure: Option<CleanupFailure>,
}

#[derive(Clone)]
struct RequestContext {
    runtime: Arc<dyn Runtime>,
    request_id: RequestId,
    command: CommandSummary,
    timeout: Duration,
    deadline: Option<Instant>,
}

impl RequestContext {
    fn request_id(&self) -> u64 {
        self.request_id.get()
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
#[derive(Clone, Default)]
pub(crate) struct TestHooks {
    deny_termination: bool,
    after_reservation_reached: Option<Arc<std::sync::Barrier>>,
    after_reservation_release: Option<Arc<std::sync::Barrier>>,
    after_spawn_reached: Option<Arc<std::sync::Barrier>>,
    after_spawn_release: Option<Arc<std::sync::Barrier>>,
    spawned_pid: Option<Arc<std::sync::atomic::AtomicU32>>,
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
            .clone()
            .with_runtime(Arc::clone(&self.configuration.runtime))
            .acquire(context.request_id, &context.command, context.deadline)
            .await
    }

    /// Replace how many dispatches may run at once.
    pub(crate) fn with_dispatch_limits(mut self, limits: DispatchLimits) -> Self {
        self.shared = Arc::new(Shared {
            lifecycle: Mutex::new(Lifecycle {
                accepting: true,
                entries: HashMap::new(),
                cleanup_failure: None,
            }),
            empty: Event::new(),
            admission: ProcessAdmission::new(limits.max_in_flight, limits.acquire_timeout),
            wait_admission: ProcessAdmission::new(limits.max_wait_clients, None),
        });
        self
    }

    /// Replace the byte budgets each dispatch's output is read under.
    pub(crate) fn with_output_limits(mut self, limits: OutputLimits) -> Self {
        Arc::make_mut(&mut self.configuration).output_limits = limits;
        self
    }

    /// Select the runtime that owns this executor's processes and supervisors.
    pub(crate) fn with_runtime(mut self, runtime: Arc<dyn Runtime>) -> Self {
        Arc::make_mut(&mut self.configuration).runtime = runtime;
        self
    }

    pub(crate) fn new(executable: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            configuration: Arc::new(Configuration {
                runtime: Arc::new(AsyncIoRuntime),
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
                    cleanup_failure: None,
                }),
                empty: Event::new(),
                admission: ProcessAdmission::new(DispatchLimits::DEFAULT_IN_FLIGHT, None),
                wait_admission: ProcessAdmission::new(DispatchLimits::DEFAULT_WAIT_CLIENTS, None),
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
        let request_id = request.request_id();
        validate_request(&self.configuration.launch, &request)?;
        let context = RequestContext {
            runtime: Arc::clone(&self.configuration.runtime),
            request_id,
            command: request.summary().clone(),
            timeout: self.configuration.timeout,
            deadline: if request.is_unbounded() {
                None
            } else {
                Instant::now().checked_add(self.configuration.timeout)
            },
        };
        trace_requested(&context);

        // Parked clients have a separate budget so ordinary commands can
        // still acquire the capacity needed to release them.
        let permit = if request.is_unbounded() {
            self.shared
                .wait_admission
                .try_acquire(context.request_id, &context.command)
        } else {
            self.acquire_permit(&context).await
        };
        let permit = match permit {
            Ok(permit) => Some(permit),
            Err(error) => {
                trace_failed(&context, &error);
                return Err(error);
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

        let mut process = self.configuration.launch.command(request.argv());
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match self.configuration.runtime.spawn_process(process) {
            Ok(child) => child,
            Err(source) => {
                drop(registry);
                let executable_not_found = (source.kind() == io::ErrorKind::NotFound
                    && self
                        .configuration
                        .launch
                        .current_dir()
                        .is_none_or(std::path::Path::is_dir))
                    || self.configuration.launch.executable_missing_from_path();
                let error = Error::spawn(
                    context.request_id(),
                    context.command.clone(),
                    source,
                    executable_not_found,
                );
                trace_failed(&context, &error);
                return Err(error);
            }
        };

        let process_group = ProcessGroupGuard::new(child.id());
        #[cfg(test)]
        let process_group = {
            let mut process_group = process_group;
            if self.configuration.hooks.deny_termination {
                process_group.deny_termination_for_test();
            }
            process_group
        };

        #[cfg(test)]
        if let (Some(spawned_pid), Some(child_id)) =
            (&self.configuration.hooks.spawned_pid, child.id())
        {
            spawned_pid.store(child_id, std::sync::atomic::Ordering::SeqCst);
        }

        #[cfg(test)]
        wait_at_barriers(
            self.configuration.hooks.after_spawn_reached.as_ref(),
            self.configuration.hooks.after_spawn_release.as_ref(),
        );

        let readers = ReaderTasks::spawn(
            &*self.configuration.runtime,
            child.take_stdout(),
            child.take_stderr(),
            self.configuration.output_limits,
            #[cfg(test)]
            &self.configuration.hooks,
        );
        let caller_cancellation = CancellationGuard::new(cancellation_sender);
        let (result_sender, result_receiver) = oneshot::channel();

        let ownership = ChildOwnership {
            process_group,
            child,
            readers,
            #[cfg(feature = "test-support")]
            synchronous_reap_on_drop: self.configuration.synchronous_reap_on_supervisor_drop,
            _permit: permit,
            registry,
        };
        let supervisor = supervise_outer(
            ownership,
            cancellation_receiver,
            context.clone(),
            result_sender,
            #[cfg(test)]
            self.configuration.hooks.clone(),
        );
        #[cfg(feature = "tracing")]
        drop(self.configuration.runtime.spawn(Box::pin(
            supervisor.in_current_span().with_current_subscriber(),
        )));
        #[cfg(not(feature = "tracing"))]
        drop(self.configuration.runtime.spawn(Box::pin(supervisor)));

        await_result(result_receiver, caller_cancellation, context).await
    }
}

impl Executor for SubprocessExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        DispatchSpan::new(&request, "subprocess").run(self.clone().run(request))
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
            shared.wait_admission.close();

            loop {
                let notified = shared.empty.listen();
                {
                    let lifecycle = lock_lifecycle(&shared);
                    if lifecycle.entries.is_empty() {
                        return lifecycle
                            .cleanup_failure
                            .as_ref()
                            .map_or(Ok(()), |failure| Err(failure.error()));
                    }
                }
                notified.await;
            }
        })
    }
}

async fn await_result(
    receiver: oneshot::Receiver<Result<CommandResult, Error>>,
    mut cancellation: CancellationGuard,
    context: RequestContext,
) -> Result<CommandResult, Error> {
    match receiver.await {
        Ok(result) => {
            cancellation.disarm();
            result
        }
        Err(_) => Err(Error::supervisor_lost(
            context.request_id(),
            context.command,
        )),
    }
}

struct ChildOwnership {
    // This guard must drop before `child`; the unreaped leader anchors the PGID.
    process_group: ProcessGroupGuard,
    child: Box<dyn Process>,
    readers: ReaderTasks,
    #[cfg(feature = "test-support")]
    synchronous_reap_on_drop: bool,
    // Admission follows the process and readers into supervisor cleanup, and
    // a parked client holds none.
    _permit: Option<OwnedSemaphorePermit>,
    // This must remain last so registry removal follows child and reader cleanup.
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
            let _ = self.process_group.signal();
            let killed = self.process_group.kill_child(&mut *self.child).is_ok();
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => {
                        self.process_group.disarm();
                        break;
                    }
                    Ok(None) if killed && Instant::now() < deadline => {
                        std::thread::yield_now();
                    }
                    Err(error)
                        if error.kind() == io::ErrorKind::Interrupted
                            && Instant::now() < deadline =>
                    {
                        std::thread::yield_now();
                    }
                    Err(error) if error.raw_os_error() == Some(ErrnoValue::child()) => {
                        self.process_group.disarm();
                        break;
                    }
                    Ok(None) | Err(_) => break,
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
            &mut *ownership.child,
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
            match cleanup_child(
                &mut *ownership.child,
                &mut ownership.readers,
                &mut ownership.process_group,
                &context,
            )
            .await
            {
                Ok(()) => Err(error),
                Err(wait_error) => {
                    ownership.registry.remember_cleanup(&wait_error);
                    Err(wait_error)
                }
            }
        }
        Err(()) => {
            let cleanup = cleanup_child(
                &mut *ownership.child,
                &mut ownership.readers,
                &mut ownership.process_group,
                &context,
            )
            .await;
            match cleanup {
                Ok(()) => Err(Error::supervisor_lost(
                    context.request_id(),
                    context.command.clone(),
                )),
                Err(wait_error) => {
                    ownership.registry.remember_cleanup(&wait_error);
                    Err(wait_error)
                }
            }
        }
    };

    drop(ownership);
    trace_finished(&context, &result);
    let _ = result_sender.send(result);
}

async fn supervise_inner(
    child: &mut dyn Process,
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
    let (stdout, stderr) = match drain_readers(readers, cancellation, context).await {
        Ok(streams) => streams,
        Err(error) => return InnerOutcome::Failed(error),
    };

    let wait_result = loop {
        let mut waited = child.wait().fuse();
        let mut cancelled = std::pin::pin!(cancelled(cancellation).fuse());
        let mut elapsed = std::pin::pin!(deadline_elapsed(&*context.runtime, deadline).fuse());
        let outcome = futures_util::select! {
            result = waited => Some(result),
            () = cancelled => None,
            () = elapsed => {
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
    context: &RequestContext,
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    let mut stdout = None;
    let mut stderr = None;

    while stdout.is_none() || stderr.is_none() {
        let ready = next_reader(
            readers,
            stdout.is_none(),
            stderr.is_none(),
            cancellation,
            context,
        )
        .await;
        match ready {
            ReaderReady::Stdout(result) => {
                readers.stdout = None;
                stdout = Some(map_reader_result(result, "stdout", context)?);
            }
            ReaderReady::Stderr(result) => {
                readers.stderr = None;
                stderr = Some(map_reader_result(result, "stderr", context)?);
            }
            ReaderReady::Cancelled => {
                return Err(Error::executor_shutdown(
                    context.request_id(),
                    context.command.clone(),
                ));
            }
            ReaderReady::TimedOut => {
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

enum ReaderReady {
    Stdout(Result<Result<Vec<u8>, io::Error>, TaskError>),
    Stderr(Result<Result<Vec<u8>, io::Error>, TaskError>),
    Cancelled,
    TimedOut,
}

async fn next_reader(
    readers: &mut ReaderTasks,
    need_stdout: bool,
    need_stderr: bool,
    cancellation: &mut watch::Receiver<bool>,
    context: &RequestContext,
) -> ReaderReady {
    let mut stdout = std::pin::pin!(wait_reader(&mut readers.stdout, need_stdout).fuse());
    let mut stderr = std::pin::pin!(wait_reader(&mut readers.stderr, need_stderr).fuse());
    let mut cancelled = std::pin::pin!(cancelled(cancellation).fuse());
    let mut elapsed = std::pin::pin!(deadline_elapsed(&*context.runtime, context.deadline).fuse());
    futures_util::select! {
        result = stdout => ReaderReady::Stdout(result),
        result = stderr => ReaderReady::Stderr(result),
        () = cancelled => ReaderReady::Cancelled,
        () = elapsed => ReaderReady::TimedOut,
    }
}

async fn deadline_elapsed(runtime: &dyn Runtime, deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        runtime.sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn wait_reader(
    reader: &mut Option<TaskHandle<Result<Vec<u8>, io::Error>>>,
    enabled: bool,
) -> Result<Result<Vec<u8>, io::Error>, TaskError> {
    if !enabled {
        return std::future::pending().await;
    }
    match reader.as_mut() {
        Some(reader) => reader.await,
        None => Ok(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "output reader is unavailable",
        ))),
    }
}

fn map_reader_result(
    result: Result<Result<Vec<u8>, io::Error>, TaskError>,
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
    child: &mut dyn Process,
    readers: &mut ReaderTasks,
    process_group: &mut ProcessGroupGuard,
    context: &RequestContext,
) -> Result<(), Error> {
    readers.abort_and_join().await;
    terminate_child(child, process_group)
        .await
        .map_err(|source| Error::wait_child(context.request_id(), context.command.clone(), source))
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
    stdout: Option<TaskHandle<Result<Vec<u8>, io::Error>>>,
    stderr: Option<TaskHandle<Result<Vec<u8>, io::Error>>>,
}

impl ReaderTasks {
    fn spawn(
        runtime: &dyn Runtime,
        stdout: Option<Reader>,
        stderr: Option<Reader>,
        limits: OutputLimits,
        #[cfg(test)] hooks: &TestHooks,
    ) -> Self {
        #[cfg(test)]
        let stdout = spawn_reader(
            runtime,
            stdout,
            limits.max_stdout_bytes,
            hooks.reader_failure,
            hooks.reader_failure_release.clone(),
        );
        #[cfg(not(test))]
        let stdout = spawn_reader(runtime, stdout, limits.max_stdout_bytes);

        #[cfg(test)]
        let stderr = spawn_reader(runtime, stderr, limits.max_stderr_bytes, None, None);
        #[cfg(not(test))]
        let stderr = spawn_reader(runtime, stderr, limits.max_stderr_bytes);

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
        reason = "the panic branch exercises task error sanitization"
    )
)]
fn spawn_reader<R>(
    runtime: &dyn Runtime,
    reader: Option<R>,
    limit: usize,
    #[cfg(test)] failure: Option<ReaderFailure>,
    #[cfg(test)] failure_release: Option<Arc<Notify>>,
) -> TaskHandle<Result<Vec<u8>, io::Error>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    runtime.spawn_value(async move {
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

        let Some(reader) = reader else {
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

    fn remember_cleanup(&self, error: &Error) {
        if let Error::WaitChild {
            request_id,
            command,
            source,
        } = error
        {
            lock_lifecycle(&self.shared)
                .cleanup_failure
                .get_or_insert_with(|| CleanupFailure::new(*request_id, command.clone(), source));
        }
    }

    fn remove(&mut self) {
        if self.armed {
            lock_lifecycle(&self.shared)
                .entries
                .remove(&self.request_id);
            self.armed = false;
            self.shared.empty.notify(usize::MAX);
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
