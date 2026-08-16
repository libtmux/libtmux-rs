use std::any::Any;
use std::collections::{HashMap, hash_map::Entry};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command as TokioCommand};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::limits::{DispatchLimits, OutputLimits};

#[cfg(feature = "tracing")]
use tracing::instrument::WithSubscriber as _;

use crate::Error;
use crate::command::{CommandRequest, CommandResult, CommandSummary, ProcessStatus, RequestId};
use crate::internal::executor::{DispatchFuture, Executor, ShutdownFuture};

#[derive(Clone)]
pub(crate) struct SubprocessExecutor {
    configuration: Arc<Configuration>,
    shared: Arc<Shared>,
}

#[derive(Clone)]
struct Configuration {
    executable: OsString,
    timeout: Duration,
    output_limits: OutputLimits,
    dispatch_limits: DispatchLimits,
    current_dir: Option<PathBuf>,
    environment: Vec<(OsString, Option<OsString>)>,
    #[cfg(feature = "test-support")]
    synchronous_reap_on_supervisor_drop: bool,
    #[cfg(test)]
    hooks: TestHooks,
}

impl Configuration {
    /// Report whether a bare executable name resolves through `PATH`.
    ///
    /// Spawn failures cannot be classified by [`io::ErrorKind`] alone. On WSL
    /// with Windows directories on `PATH`, a bare name that resolves nowhere
    /// fails with `EIO` rather than `ENOENT`, which would otherwise degrade a
    /// missing tmux into an untyped spawn failure on a supported platform.
    ///
    /// Only bare names are answered here. A name containing a separator is a
    /// path that the operating system resolves against the child's working
    /// directory, so `ENOENT` already identifies it.
    fn executable_missing_from_path(&self) -> bool {
        let executable = std::path::Path::new(&self.executable);
        if executable.components().count() != 1 {
            return false;
        }

        let path = self
            .environment
            .iter()
            .rev()
            .find(|(key, _)| key == "PATH")
            .map_or_else(|| std::env::var_os("PATH"), |(_, value)| value.clone());
        let Some(path) = path else {
            return true;
        };

        !std::env::split_paths(&path).any(|directory| directory.join(executable).is_file())
    }
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
    permits: Arc<Semaphore>,
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
    after_reservation_reached: Option<Arc<std::sync::Barrier>>,
    after_reservation_release: Option<Arc<std::sync::Barrier>>,
    after_spawn_reached: Option<Arc<std::sync::Barrier>>,
    after_spawn_release: Option<Arc<std::sync::Barrier>>,
    spawned_pid: Option<Arc<std::sync::atomic::AtomicU32>>,
    reader_failure: Option<ReaderFailure>,
    reader_failure_release: Option<Arc<Notify>>,
    wait_failure_release: Option<Arc<Notify>>,
    supervisor_failure_release: Option<Arc<Notify>>,
}

impl SubprocessExecutor {
    /// Wait for room to run, or say the server is full.
    async fn acquire_permit(
        &self,
        context: &RequestContext,
    ) -> Result<OwnedSemaphorePermit, Error> {
        let permits = Arc::clone(&self.shared.permits);
        let limits = self.configuration.dispatch_limits;

        let acquired = match limits.acquire_timeout {
            Some(timeout) => match tokio::time::timeout(timeout, permits.acquire_owned()).await {
                Ok(acquired) => acquired,
                // Waited the whole budget without room, which is overload
                // rather than slowness: nothing was sent to tmux.
                Err(_) => {
                    return Err(Error::Overloaded {
                        request_id: context.request_id(),
                        command: context.command.clone(),
                        in_flight: limits.max_in_flight,
                    });
                }
            },
            None => permits.acquire_owned().await,
        };

        acquired
            .map_err(|_| Error::executor_shutdown(context.request_id(), context.command.clone()))
    }

    /// Replace how many dispatches may run at once.
    pub(crate) fn with_dispatch_limits(mut self, limits: DispatchLimits) -> Self {
        Arc::make_mut(&mut self.configuration).dispatch_limits = limits;
        self.shared = Arc::new(Shared {
            lifecycle: Mutex::new(Lifecycle {
                accepting: true,
                entries: HashMap::new(),
            }),
            empty: Notify::new(),
            permits: Arc::new(Semaphore::new(limits.max_in_flight)),
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
                executable: executable.into(),
                timeout,
                output_limits: OutputLimits::default(),
                dispatch_limits: DispatchLimits::default(),
                current_dir: None,
                environment: Vec::new(),
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
                permits: Arc::new(Semaphore::new(DispatchLimits::DEFAULT_IN_FLIGHT)),
            }),
        }
    }

    pub(crate) fn with_environment(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        let mut configuration = (*self.configuration).clone();
        configuration
            .environment
            .push((key.into(), Some(value.into())));
        self.configuration = Arc::new(configuration);
        self
    }

    pub(crate) fn with_environment_removed(mut self, key: impl Into<OsString>) -> Self {
        let mut configuration = (*self.configuration).clone();
        configuration.environment.push((key.into(), None));
        self.configuration = Arc::new(configuration);
        self
    }

    pub(crate) fn with_current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        let mut configuration = (*self.configuration).clone();
        configuration.current_dir = Some(current_dir.into());
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
        validate_request(&self.configuration.executable, &request, request_id)?;
        let context = RequestContext {
            request_id,
            command: request.summary().clone(),
            timeout: self.configuration.timeout,
        };
        trace_requested(&context);

        // Held for the whole dispatch and released on drop, so a cancelled
        // caller returns its permit even though the child it started is still
        // being cleaned up.
        // Bound to `_permit` rather than dropped: the guard is the admission,
        // and releasing it here would let the next dispatch in immediately.
        let _permit = match self.acquire_permit(&context).await {
            Ok(permit) => permit,
            Err(error) => return Err(error),
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

        let mut process = TokioCommand::new(&self.configuration.executable);
        process
            .args(request.argv())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        if let Some(current_dir) = &self.configuration.current_dir {
            process.current_dir(current_dir);
        }
        for (key, value) in &self.configuration.environment {
            if let Some(value) = value {
                process.env(key, value);
            } else {
                process.env_remove(key);
            }
        }

        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(source) => {
                drop(registry);
                let executable_not_found = (source.kind() == io::ErrorKind::NotFound
                    && self
                        .configuration
                        .current_dir
                        .as_deref()
                        .is_none_or(std::path::Path::is_dir))
                    || self.configuration.executable_missing_from_path();
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

        let process_group = ProcessGroupGuard::new(child.id());
        let readers = ReaderTasks::spawn(
            child.stdout.take(),
            child.stderr.take(),
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
        tokio::spawn(supervisor.with_current_subscriber());
        #[cfg(not(feature = "tracing"))]
        tokio::spawn(supervisor);

        await_result(result_receiver, caller_cancellation, context).await
    }
}

impl Executor for SubprocessExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        DispatchFuture::new(self.clone().run(request))
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

fn validate_request(
    executable: &OsStr,
    request: &CommandRequest,
    request_id: RequestId,
) -> Result<(), Error> {
    use std::os::unix::ffi::OsStrExt as _;

    if executable.as_bytes().contains(&0) {
        return Err(Error::invalid_command_input(
            request_id.get(),
            "tmux executable",
        ));
    }
    for (index, argument) in request.argv().iter().enumerate() {
        if argument.as_os_str().as_bytes().contains(&0) {
            let input = match index.cmp(&request.logical_subcommand_index()) {
                std::cmp::Ordering::Less => "tmux global argument",
                std::cmp::Ordering::Equal => "tmux subcommand",
                std::cmp::Ordering::Greater => "tmux argument",
            };
            return Err(Error::invalid_command_input(request_id.get(), input));
        }
    }
    Ok(())
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
    child: Child,
    readers: ReaderTasks,
    #[cfg(feature = "test-support")]
    synchronous_reap_on_drop: bool,
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
        if self.synchronous_reap_on_drop && self.process_group.armed {
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
            match cleanup_child(
                &mut ownership.child,
                &mut ownership.readers,
                &mut ownership.process_group,
                &context,
            )
            .await
            {
                Ok(()) => Err(error),
                Err(wait_error) => Err(wait_error),
            }
        }
        Err(()) => {
            let cleanup = cleanup_child(
                &mut ownership.child,
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
                Err(wait_error) => Err(wait_error),
            }
        }
    };

    drop(ownership);
    trace_finished(&context, &result);
    let _ = result_sender.send(result);
}

async fn supervise_inner(
    child: &mut Child,
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

    let deadline = Instant::now().checked_add(context.timeout);
    let (stdout, stderr) = match drain_readers(readers, cancellation, deadline, context).await {
        Ok(streams) => streams,
        Err(error) => return InnerOutcome::Failed(error),
    };

    let wait_result = loop {
        let outcome = tokio::select! {
            result = child.wait() => Some(result),
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
    child: &mut Child,
    readers: &mut ReaderTasks,
    process_group: &mut ProcessGroupGuard,
    context: &RequestContext,
) -> Result<(), Error> {
    readers.abort_and_join().await;
    process_group.signal();
    let _ = child.start_kill();
    loop {
        match child.wait().await {
            Ok(_) => {
                process_group.disarm();
                return Ok(());
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => {
                if source.raw_os_error() == Some(ErrnoValue::child()) {
                    process_group.disarm();
                }
                return Err(Error::wait_child(
                    context.request_id(),
                    context.command.clone(),
                    source,
                ));
            }
        }
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
        limits: OutputLimits,
        #[cfg(test)] hooks: &TestHooks,
    ) -> Self {
        #[cfg(test)]
        let stdout = spawn_reader(
            stdout,
            limits.max_stdout_bytes,
            hooks.reader_failure,
            hooks.reader_failure_release.clone(),
        );
        #[cfg(not(test))]
        let stdout = spawn_reader(stdout, limits.max_stdout_bytes);

        #[cfg(test)]
        let stderr = spawn_reader(stderr, limits.max_stderr_bytes, None, None);
        #[cfg(not(test))]
        let stderr = spawn_reader(stderr, limits.max_stderr_bytes);

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

struct ProcessGroupGuard {
    process_group: Option<Pid>,
    armed: bool,
}

impl ProcessGroupGuard {
    fn new(child_id: Option<u32>) -> Self {
        let process_group = child_id
            .and_then(|value| i32::try_from(value).ok())
            .and_then(Pid::from_raw);
        Self {
            process_group,
            armed: true,
        }
    }

    fn signal(&self) {
        if self.armed {
            if let Some(process_group) = self.process_group {
                let _ = kill_process_group(process_group, Signal::KILL);
            }
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.signal();
    }
}

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
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::too_many_lines,
        clippy::unwrap_used
    )]

    use std::error::Error as StdError;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::{Read, Write};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;
    use std::process;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    use rustix::io::Errno;
    use rustix::process::{Pid, WaitOptions, test_kill_process, waitpid};

    use super::{ReaderFailure, SubprocessExecutor, TestHooks, validate_request};
    use crate::command::{CommandRequest, RequestId};
    use crate::internal::executor::Executor;
    use crate::{Command, Error};

    const CHILD_ENV: &str = "LIBTMUX_RS_TEST_CHILD";
    const CHILD_TEST: &str = "internal::subprocess::tests::child_helper";
    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    /// How long a poll loop waits before looking again.
    ///
    /// Sleeping rather than yielding matters: these loops wait on a separate
    /// process, and a spinning task holds a worker thread against the thing
    /// it is waiting for. With two worker threads and a loaded machine, that
    /// is enough to miss the deadline it is measuring.
    const POLL_INTERVAL: Duration = Duration::from_millis(1);

    #[cfg(feature = "tracing")]
    const TRACE_CHILD_ENV: &str = "LIBTMUX_RS_TRACING_TEST_CHILD";
    #[cfg(feature = "tracing")]
    const TRACE_EARLY_TEST: &str =
        "internal::subprocess::tests::tracing_early_failures_emit_one_sanitized_terminal_event";
    #[cfg(feature = "tracing")]
    const TRACE_SUPERVISOR_TEST: &str = "internal::subprocess::tests::tracing_errors_and_sources_omit_sensitive_argv_and_raw_output";

    #[cfg(feature = "tracing")]
    async fn tracing_test_is_isolated_child(test_name: &str) -> bool {
        if std::env::var_os(TRACE_CHILD_ENV).as_deref() == Some(OsStr::new(test_name)) {
            return true;
        }

        let output = tokio::process::Command::new(
            std::env::current_exe().expect("test executable is available"),
        )
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .arg("--")
        .env(TRACE_CHILD_ENV, test_name)
        .output()
        .await
        .expect("isolated tracing test starts");
        assert!(
            output.status.success(),
            "isolated tracing test failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        false
    }

    #[test]
    fn nul_validation_distinguishes_global_subcommand_and_argument_positions() {
        let cases = [
            (
                vec![OsString::from_vec(b"-S\0tail".to_vec())],
                Command::new("display-message"),
                "tmux global argument",
            ),
            (
                vec![OsString::from("-S"), OsString::from("/tmp/socket")],
                Command::new(OsString::from_vec(b"command\0tail".to_vec())),
                "tmux subcommand",
            ),
            (
                vec![OsString::from("-S"), OsString::from("/tmp/socket")],
                Command::new("display-message").arg(OsString::from_vec(b"argument\0tail".to_vec())),
                "tmux argument",
            ),
        ];

        for (global_argv, command, expected) in cases {
            let request =
                CommandRequest::with_global_argv(RequestId::new(31), &global_argv, command);
            let error = validate_request(OsStr::new("tmux"), &request, RequestId::new(31))
                .expect_err("fixture contains NUL");
            assert!(matches!(
                error,
                Error::InvalidCommandInput { input, .. } if input == expected
            ));
        }
    }

    #[test]
    #[allow(
        clippy::zombie_processes,
        reason = "the process-group test deliberately kills parent and descendant together"
    )]
    fn child_helper() {
        let Some(mode) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        let arguments = helper_arguments();

        match mode.as_bytes() {
            b"streams" => {
                std::io::stdout()
                    .write_all(&vec![0xff; 128 * 1024])
                    .expect("stdout is writable");
                std::io::stderr()
                    .write_all(&vec![0xfe; 128 * 1024])
                    .expect("stderr is writable");
            }
            b"nonzero" => {
                std::io::stdout()
                    .write_all(b"nonzero-stdout\n\n")
                    .expect("stdout is writable");
                process::exit(7);
            }
            b"stdin-eof" => {
                let mut input = Vec::new();
                std::io::stdin()
                    .read_to_end(&mut input)
                    .expect("stdin is readable");
                writeln!(std::io::stdout(), "stdin={}", input.len()).expect("stdout is writable");
            }
            b"echo-last" => {
                std::io::stdout()
                    .write_all(arguments.last().expect("payload argument").as_bytes())
                    .expect("stdout is writable");
            }
            b"block" => {
                write_pid_file(arguments.first().expect("PID path"), None);
                loop {
                    std::thread::park();
                }
            }
            b"secret-block" => {
                write_pid_file(arguments.first().expect("PID path"), None);
                std::io::stdout()
                    .write_all(b"sentinel-output-secret")
                    .expect("stdout is writable");
                loop {
                    std::thread::park();
                }
            }
            b"secret-success" => {
                std::io::stdout()
                    .write_all(b"sentinel-success-output")
                    .expect("stdout is writable");
            }
            b"descendant" => {
                let pid_path = arguments.first().expect("PID path");
                let child =
                    process::Command::new(std::env::current_exe().expect("test executable"))
                        .arg("--exact")
                        .arg(CHILD_TEST)
                        .arg("--nocapture")
                        .arg("--")
                        .env(CHILD_ENV, "grandchild")
                        .spawn()
                        .expect("grandchild starts");
                write_pid_file(pid_path, Some(child.id()));
                loop {
                    std::thread::park();
                }
            }
            b"descendant-parent-exits" => {
                let pid_path = arguments.first().expect("PID path");
                let child =
                    process::Command::new(std::env::current_exe().expect("test executable"))
                        .arg("--exact")
                        .arg(CHILD_TEST)
                        .arg("--nocapture")
                        .arg("--")
                        .env(CHILD_ENV, "grandchild")
                        .spawn()
                        .expect("grandchild starts");
                write_pid_file(pid_path, Some(child.id()));
            }
            b"grandchild" => loop {
                std::thread::park();
            },
            other => panic!("unknown child helper mode: {other:?}"),
        }
    }

    fn helper_arguments() -> Vec<OsString> {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        let marker = arguments
            .iter()
            .rposition(|argument| argument == OsStr::new("--"))
            .expect("helper command includes an argument separator");
        arguments.into_iter().skip(marker + 1).collect()
    }

    fn write_pid_file(path: &OsStr, descendant: Option<u32>) {
        let mut value = process::id().to_string();
        if let Some(pid) = descendant {
            value.push('\n');
            value.push_str(&pid.to_string());
        }
        let path = Path::new(path);
        let mut staging_name = path
            .file_name()
            .expect("PID path has a file name")
            .to_os_string();
        staging_name.push(format!(".{}.tmp", process::id()));
        let staging = path.with_file_name(staging_name);
        let mut file = fs::File::create(&staging).expect("staged PID file is creatable");
        file.write_all(value.as_bytes())
            .expect("staged PID file is writable");
        file.sync_all().expect("staged PID contents are durable");
        drop(file);
        fs::rename(staging, path).expect("PID file is published atomically");
    }

    fn helper_command(arguments: impl IntoIterator<Item = OsString>) -> Command {
        arguments.into_iter().fold(
            Command::new("--exact")
                .arg(CHILD_TEST)
                .arg("--nocapture")
                .arg("--"),
            Command::arg,
        )
    }

    fn request(id: u64, arguments: impl IntoIterator<Item = OsString>) -> CommandRequest {
        CommandRequest::new(RequestId::new(id), helper_command(arguments))
    }

    fn request_with_command(id: u64, command: Command) -> CommandRequest {
        CommandRequest::new(RequestId::new(id), command)
    }

    fn executor(mode: &str, timeout: Duration) -> SubprocessExecutor {
        SubprocessExecutor::new(std::env::current_exe().expect("test executable"), timeout)
            .with_environment(CHILD_ENV, mode)
    }

    async fn read_pids(path: &Path, count: usize) -> Vec<u32> {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                if let Ok(contents) = fs::read_to_string(path) {
                    let pids = contents
                        .lines()
                        .filter_map(|line| line.parse::<u32>().ok())
                        .collect::<Vec<_>>();
                    if pids.len() == count {
                        return pids;
                    }
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .expect("child publishes PIDs before the test deadline")
    }

    fn pid(value: u32) -> Pid {
        Pid::from_raw(i32::try_from(value).expect("test PID fits i32"))
            .expect("test PID is nonzero")
    }

    async fn assert_process_gone(value: u32) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                if matches!(test_kill_process(pid(value)), Err(Errno::SRCH)) {
                    return;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .expect("process disappears before the test deadline");
    }

    fn assert_process_reaped(value: u32) {
        assert!(
            matches!(test_kill_process(pid(value)), Err(Errno::SRCH)),
            "process {value} still exists after terminal cleanup"
        );
    }

    fn assert_error_redacted(error: &Error, secrets: &[&str]) {
        let mut diagnostics = vec![error.to_string(), format!("{error:?}")];
        let mut source = StdError::source(error);
        while let Some(current) = source {
            diagnostics.push(current.to_string());
            diagnostics.push(format!("{current:?}"));
            source = current.source();
        }
        for diagnostic in diagnostics {
            for secret in secrets {
                assert!(
                    !diagnostic.contains(secret),
                    "leaked secret in {diagnostic:?}"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drains_stdout_and_stderr_concurrently_as_exact_bytes() {
        let executor = executor("streams", TEST_TIMEOUT);
        let result = executor
            .execute(request(1, []))
            .await
            .expect("helper exits successfully");

        assert!(
            result
                .stdout()
                .split(|byte| *byte != 0xff)
                .any(|run| run.len() == 128 * 1024)
        );
        assert!(
            result
                .stderr()
                .split(|byte| *byte != 0xfe)
                .any(|run| run.len() == 128 * 1024)
        );
        assert!(result.success());
        executor.shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test]
    async fn nonzero_exit_and_stdout_are_returned_as_data() {
        let executor = executor("nonzero", TEST_TIMEOUT);
        let result = executor
            .execute(request(2, []))
            .await
            .expect("nonzero status remains result data");

        assert_eq!(result.exit_code(), Some(7));
        assert!(result.stdout().ends_with(b"nonzero-stdout\n\n"));
        executor.shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test]
    async fn child_stdin_is_null_and_reaches_eof() {
        let executor = executor("stdin-eof", TEST_TIMEOUT);
        let result = executor
            .execute(request(3, []))
            .await
            .expect("helper reads EOF and exits");

        assert!(
            result
                .stdout()
                .windows(b"stdin=0\n".len())
                .any(|bytes| bytes == b"stdin=0\n")
        );
        executor.shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deadline_kills_awaits_and_unregisters_the_child() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("deadline.pid");
        // The deadline has to lose to the child's startup, not race it. The
        // child publishes its PID and the test reads it back, so a deadline
        // that expires first kills the child before it ever writes, and the
        // read then waits out its own five seconds for a file nobody will
        // write. At 100ms that happened on CI, where re-executing this binary
        // takes longer than it does here. The length is not what is under
        // test; that the deadline kills, awaits, and unregisters is.
        let executor = executor("block", Duration::from_secs(2));
        let dispatch =
            tokio::spawn(executor.execute(request(4, [pid_path.as_os_str().to_os_string()])));
        let child_pid = read_pids(&pid_path, 1).await[0];
        let error = dispatch
            .await
            .expect("dispatch task remains healthy")
            .expect_err("blocking helper reaches deadline");

        assert!(matches!(error, Error::Timeout { .. }));
        assert_eq!(executor.active_request_count(), 0);
        assert_process_reaped(child_pid);
        executor.shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_dispatch_future_cancels_and_reaps() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("drop.pid");
        let executor = executor("block", TEST_TIMEOUT);
        let mut dispatch =
            Box::pin(executor.execute(request(5, [pid_path.as_os_str().to_os_string()])));

        tokio::select! {
            _ = read_pids(&pid_path, 1) => {}
            result = &mut dispatch => panic!("helper terminated before cancellation: {result:?}"),
        }
        let child_pid = read_pids(&pid_path, 1).await[0];
        drop(dispatch);
        executor
            .shutdown()
            .await
            .expect("shutdown waits for cleanup");

        assert_eq!(executor.active_request_count(), 0);
        assert_process_reaped(child_pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aborting_the_awaiting_task_cancels_and_reaps() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("abort.pid");
        let executor = executor("block", TEST_TIMEOUT);
        let dispatch =
            tokio::spawn(executor.execute(request(6, [pid_path.as_os_str().to_os_string()])));
        let child_pid = read_pids(&pid_path, 1).await[0];

        dispatch.abort();
        assert!(dispatch.await.expect_err("task was aborted").is_cancelled());
        executor
            .shutdown()
            .await
            .expect("shutdown waits for cleanup");

        assert_eq!(executor.active_request_count(), 0);
        assert_process_reaped(child_pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_kills_same_group_descendants_holding_pipes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("descendant.pid");
        let executor = executor("descendant", TEST_TIMEOUT);
        let dispatch =
            tokio::spawn(executor.execute(request(7, [pid_path.as_os_str().to_os_string()])));
        let pids = read_pids(&pid_path, 2).await;

        dispatch.abort();
        let _ = dispatch.await;
        executor
            .shutdown()
            .await
            .expect("shutdown waits for cleanup");

        assert_eq!(executor.active_request_count(), 0);
        assert_process_reaped(pids[0]);
        assert_process_gone(pids[1]).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exited_leader_anchors_group_while_descendant_holds_pipes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("exited-leader.pid");
        // Same race as `deadline_kills_awaits_and_unregisters_the_child`, and
        // worse: two PIDs have to be published before the deadline expires.
        let executor = executor("descendant-parent-exits", Duration::from_secs(2));
        let dispatch =
            tokio::spawn(executor.execute(request(25, [pid_path.as_os_str().to_os_string()])));
        let pids = read_pids(&pid_path, 2).await;
        let error = dispatch
            .await
            .expect("dispatch task remains healthy")
            .expect_err("inherited pipes remain open until the overall deadline");

        assert!(matches!(error, Error::Timeout { .. }));
        assert_eq!(executor.active_request_count(), 0);
        assert_process_reaped(pids[0]);
        assert_process_gone(pids[1]).await;
        executor.shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_cancels_all_children_rejects_new_work_and_is_idempotent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first_path = directory.path().join("first.pid");
        let second_path = directory.path().join("second.pid");
        let rejected_path = directory.path().join("rejected.pid");
        let executor = executor("block", TEST_TIMEOUT);
        let first =
            tokio::spawn(executor.execute(request(8, [first_path.as_os_str().to_os_string()])));
        let second =
            tokio::spawn(executor.execute(request(9, [second_path.as_os_str().to_os_string()])));
        let first_pid = read_pids(&first_path, 1).await[0];
        let second_pid = read_pids(&second_path, 1).await[0];

        let first_shutdown_executor = executor.clone();
        let second_shutdown_executor = executor.clone();
        let first_shutdown = tokio::spawn(async move { first_shutdown_executor.shutdown().await });
        let second_shutdown =
            tokio::spawn(async move { second_shutdown_executor.shutdown().await });
        for shutdown in [first_shutdown, second_shutdown] {
            tokio::time::timeout(TEST_TIMEOUT, shutdown)
                .await
                .expect("concurrent shutdown does not miss an empty notification")
                .expect("shutdown task remains healthy")
                .expect("shutdown succeeds");
        }
        executor.shutdown().await.expect("later shutdown succeeds");
        assert_eq!(executor.active_request_count(), 0);
        assert!(matches!(
            executor
                .execute(request(10, [rejected_path.as_os_str().to_os_string()]))
                .await,
            Err(Error::ExecutorShutdown { .. })
        ));
        assert!(!rejected_path.exists());
        let _ = first.await;
        let _ = second.await;
        assert_process_reaped(first_pid);
        assert_process_reaped(second_pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn duplicate_active_request_id_is_rejected_before_spawn() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first_path = directory.path().join("first.pid");
        let duplicate_path = directory.path().join("duplicate.pid");
        let executor = executor("block", TEST_TIMEOUT);
        let first =
            tokio::spawn(executor.execute(request(11, [first_path.as_os_str().to_os_string()])));
        let child_pid = read_pids(&first_path, 1).await[0];

        assert!(matches!(
            executor
                .execute(request(11, [duplicate_path.as_os_str().to_os_string()]))
                .await,
            Err(Error::DuplicateRequest { .. })
        ));
        assert!(!duplicate_path.exists());
        first.abort();
        let _ = first.await;
        executor
            .shutdown()
            .await
            .expect("shutdown waits for cleanup");
        assert_process_reaped(child_pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn abort_between_spawn_and_supervisor_handoff_cannot_orphan() {
        let reached = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let spawned_pid = Arc::new(AtomicU32::new(0));
        let hooks = TestHooks {
            after_spawn_reached: Some(Arc::clone(&reached)),
            after_spawn_release: Some(Arc::clone(&release)),
            spawned_pid: Some(Arc::clone(&spawned_pid)),
            ..TestHooks::default()
        };
        let executor = executor("block", TEST_TIMEOUT).with_test_hooks(hooks);
        let dispatch = tokio::spawn(executor.execute(request(12, [])));

        tokio::task::spawn_blocking(move || reached.wait())
            .await
            .expect("barrier task succeeds");
        dispatch.abort();
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("barrier task succeeds");
        let _ = dispatch.await;
        let child_pid = spawned_pid.load(Ordering::SeqCst);
        assert_ne!(child_pid, 0, "spawn hook records the direct child PID");
        executor
            .shutdown()
            .await
            .expect("shutdown waits for cleanup");

        assert_eq!(executor.active_request_count(), 0);
        assert_process_reaped(child_pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn accepted_start_racing_shutdown_remains_registered_until_cleanup() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("race.pid");
        let reached = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let hooks = TestHooks {
            after_reservation_reached: Some(Arc::clone(&reached)),
            after_reservation_release: Some(Arc::clone(&release)),
            ..TestHooks::default()
        };
        let executor = executor("block", TEST_TIMEOUT).with_test_hooks(hooks);
        let dispatch =
            tokio::spawn(executor.execute(request(13, [pid_path.as_os_str().to_os_string()])));

        tokio::task::spawn_blocking(move || reached.wait())
            .await
            .expect("barrier task succeeds");
        let shutdown_executor = executor.clone();
        let shutdown = tokio::spawn(async move { shutdown_executor.shutdown().await });
        tokio::time::timeout(TEST_TIMEOUT, async {
            while executor.is_accepting() {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .expect("shutdown closes admission before the reservation is released");
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("barrier task succeeds");

        shutdown
            .await
            .expect("shutdown task remains healthy")
            .expect("shutdown succeeds");
        let _ = dispatch.await;
        assert_eq!(executor.active_request_count(), 0);
        if pid_path.exists() {
            assert_process_reaped(read_pids(&pid_path, 1).await[0]);
        }
    }

    #[tokio::test]
    async fn shutdown_winning_admission_prevents_spawn() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("never.pid");
        let executor = executor("block", TEST_TIMEOUT);
        executor.shutdown().await.expect("shutdown succeeds");

        assert!(matches!(
            executor
                .execute(request(14, [pid_path.as_os_str().to_os_string()]))
                .await,
            Err(Error::ExecutorShutdown { .. })
        ));
        assert!(!pid_path.exists());
    }

    #[tokio::test]
    async fn unresolvable_executable_is_typed_regardless_of_spawn_error_kind() {
        // A bare name that resolves nowhere is classified by resolution rather
        // than by `io::ErrorKind`. WSL with Windows directories on `PATH`
        // reports `EIO` here, so classifying by kind alone would return an
        // untyped spawn failure for a missing tmux on a supported platform.
        let directory = tempfile::tempdir().expect("temporary directory");
        let empty_path = directory.path().as_os_str().to_os_string();
        let missing = SubprocessExecutor::new("libtmux-missing-executable", TEST_TIMEOUT)
            .with_environment("PATH", empty_path);

        let error = missing
            .execute(request_with_command(40, Command::new("display-message")))
            .await
            .expect_err("missing executable fails");

        assert!(
            matches!(error, Error::ExecutableNotFound { .. }),
            "unresolvable bare name is typed, got {error:?}",
        );
    }

    #[tokio::test]
    async fn present_but_unexecutable_file_is_not_reported_as_missing() {
        // Resolution mirrors `execvp`: it matches regular files, so a present
        // file that cannot be executed is a permission failure rather than an
        // absent tmux. Conflating the two would send callers looking for an
        // installation that is already there.
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("libtmux-unexecutable");
        fs::write(&executable, b"#!/bin/sh\n").expect("file is created");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o644))
            .expect("permissions are cleared");

        let error = SubprocessExecutor::new("libtmux-unexecutable", TEST_TIMEOUT)
            .with_environment("PATH", directory.path().as_os_str().to_os_string())
            .execute(request_with_command(41, Command::new("display-message")))
            .await
            .expect_err("an unexecutable file fails to spawn");

        assert!(
            !matches!(error, Error::ExecutableNotFound { .. }),
            "a present file is not reported missing, got {error:?}",
        );
    }

    #[tokio::test]
    async fn invalid_executable_and_nul_inputs_are_sanitized_typed_errors() {
        let missing = SubprocessExecutor::new("libtmux-missing-executable", TEST_TIMEOUT);
        let error = missing
            .execute(request_with_command(15, Command::new("display-message")))
            .await
            .expect_err("missing executable fails");
        assert!(matches!(error, Error::ExecutableNotFound { .. }));

        let nul_executable =
            SubprocessExecutor::new(OsString::from_vec(b"tmux\0invalid".to_vec()), TEST_TIMEOUT);
        let nul_commands = [
            Command::new(OsString::from_vec(b"display\0message".to_vec())),
            Command::new("display-message").arg(OsString::from_vec(b"public\0arg".to_vec())),
            Command::new("display-message")
                .sensitive_arg(OsString::from_vec(b"sensitive\0arg".to_vec())),
        ];
        let mut errors = vec![
            nul_executable
                .execute(request_with_command(16, Command::new("display-message")))
                .await
                .expect_err("NUL executable fails"),
        ];
        for (offset, command) in nul_commands.into_iter().enumerate() {
            errors.push(
                executor("echo-last", TEST_TIMEOUT)
                    .execute(request_with_command(17 + offset as u64, command))
                    .await
                    .expect_err("NUL command token fails"),
            );
        }

        for error in &errors {
            assert!(matches!(error, Error::InvalidCommandInput { .. }));
            assert!(StdError::source(error).is_none());
            assert_error_redacted(error, &["tmux\0invalid", "sensitive", "public"]);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reader_failure_and_supervisor_loss_are_sanitized_and_cleanup() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let reader_pid_path = directory.path().join("reader.pid");
        let reader_release = Arc::new(tokio::sync::Notify::new());
        let reader = executor("block", TEST_TIMEOUT).with_test_hooks(TestHooks {
            reader_failure: Some(ReaderFailure::Error),
            reader_failure_release: Some(Arc::clone(&reader_release)),
            ..TestHooks::default()
        });
        let reader_dispatch =
            tokio::spawn(reader.execute(request(20, [reader_pid_path.as_os_str().to_os_string()])));
        let reader_pid = read_pids(&reader_pid_path, 1).await[0];
        reader_release.notify_one();
        let reader_error = reader_dispatch
            .await
            .expect("reader dispatch task remains healthy")
            .expect_err("injected reader failure is surfaced");
        assert!(matches!(reader_error, Error::ReadOutput { .. }));
        assert!(StdError::source(&reader_error).is_none());
        assert_eq!(reader.active_request_count(), 0);
        assert_process_reaped(reader_pid);
        reader.shutdown().await.expect("shutdown succeeds");

        let lost_pid_path = directory.path().join("lost.pid");
        let supervisor_release = Arc::new(tokio::sync::Notify::new());
        let lost = executor("block", TEST_TIMEOUT).with_test_hooks(TestHooks {
            supervisor_failure_release: Some(Arc::clone(&supervisor_release)),
            ..TestHooks::default()
        });
        let lost_dispatch =
            tokio::spawn(lost.execute(request(21, [lost_pid_path.as_os_str().to_os_string()])));
        let lost_pid = read_pids(&lost_pid_path, 1).await[0];
        supervisor_release.notify_one();
        let lost_error = lost_dispatch
            .await
            .expect("lost-supervisor dispatch task remains healthy")
            .expect_err("lost supervisor is surfaced");
        assert!(matches!(lost_error, Error::SupervisorLost { .. }));
        assert!(StdError::source(&lost_error).is_none());
        assert_eq!(lost.active_request_count(), 0);
        assert_process_reaped(lost_pid);
        lost.shutdown().await.expect("shutdown succeeds");

        for error in [&reader_error, &lost_error] {
            assert_error_redacted(
                error,
                &["sentinel-output-secret", "sentinel-supervisor-panic"],
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_failure_is_typed_and_cleanup_reaps_the_child() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("wait.pid");
        let wait_failure_release = Arc::new(tokio::sync::Notify::new());
        let executor = executor("block", TEST_TIMEOUT).with_test_hooks(TestHooks {
            wait_failure_release: Some(Arc::clone(&wait_failure_release)),
            ..TestHooks::default()
        });
        let dispatch =
            tokio::spawn(executor.execute(request(22, [pid_path.as_os_str().to_os_string()])));
        let child_pid = read_pids(&pid_path, 1).await[0];

        wait_failure_release.notify_one();
        let error = dispatch
            .await
            .expect("wait-failure dispatch task remains healthy")
            .expect_err("injected wait failure is surfaced");

        assert!(matches!(error, Error::WaitChild { .. }));
        assert!(StdError::source(&error).is_some());
        assert_eq!(executor.active_request_count(), 0);
        assert_process_reaped(child_pid);
        executor.shutdown().await.expect("shutdown succeeds");
    }

    #[tokio::test]
    async fn shell_metacharacters_reach_the_child_as_one_exact_argument() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let side_effect = directory.path().join("must-not-exist");
        let payload = format!("$(touch {}) ; * & |", side_effect.display());
        let executor = executor("echo-last", TEST_TIMEOUT);
        let command = Command::new("--exact")
            .arg(CHILD_TEST)
            .arg("--nocapture")
            .arg("--")
            .sensitive_arg(payload.clone());
        let result = executor
            .execute(request_with_command(22, command))
            .await
            .expect("helper echoes one literal argument");

        assert!(
            result
                .stdout()
                .windows(payload.len())
                .any(|bytes| bytes == payload.as_bytes())
        );
        assert!(!side_effect.exists());
        assert!(!result.command().to_string().contains(&payload));
        executor.shutdown().await.expect("shutdown succeeds");
    }

    #[cfg(feature = "tracing")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tracing_early_failures_emit_one_sanitized_terminal_event() {
        use std::sync::Mutex;

        use tracing::instrument::WithSubscriber as _;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);

        struct Writer(Arc<Mutex<Vec<u8>>>);

        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Writer;

            fn make_writer(&'a self) -> Self::Writer {
                Writer(Arc::clone(&self.0))
            }
        }

        fn subscriber(buffer: Buffer) -> impl tracing::Subscriber + Send + Sync {
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_max_level(tracing::Level::DEBUG)
                .with_writer(buffer)
                .finish()
        }

        fn assert_single_failure(trace: &str, request_id: u64, secrets: &[&str]) {
            assert_eq!(
                trace.matches("tmux command requested").count(),
                1,
                "trace must contain one requested event: {trace:?}"
            );
            assert_eq!(
                trace.matches("tmux command failed").count(),
                1,
                "trace must contain one terminal failure event: {trace:?}"
            );
            assert!(
                trace.contains(&format!("request_id={request_id}")),
                "trace must retain the safe request ID: {trace:?}"
            );
            for secret in secrets {
                assert!(!trace.contains(secret), "trace leaked {secret}: {trace:?}");
            }
        }

        if !tracing_test_is_isolated_child(TRACE_EARLY_TEST).await {
            return;
        }

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable_secret = "sentinel-missing-executable-path";
        let argument_secret = "sentinel-missing-argument";
        let missing_buffer = Buffer::default();
        let missing =
            SubprocessExecutor::new(directory.path().join(executable_secret), TEST_TIMEOUT);
        let missing_error = async {
            let error = missing
                .execute(request_with_command(
                    27,
                    Command::new("display-message").sensitive_arg(argument_secret),
                ))
                .await
                .expect_err("missing executable fails before supervisor handoff");
            missing.shutdown().await.expect("shutdown succeeds");
            error
        }
        .with_subscriber(subscriber(missing_buffer.clone()))
        .await;
        assert!(matches!(missing_error, Error::ExecutableNotFound { .. }));
        let missing_trace = String::from_utf8_lossy(&missing_buffer.0.lock().unwrap()).into_owned();
        assert_single_failure(&missing_trace, 27, &[executable_secret, argument_secret]);

        let shutdown_secret = "sentinel-shutdown-argument";
        let shutdown_buffer = Buffer::default();
        let closed = executor("echo-last", TEST_TIMEOUT);
        let shutdown_error = async {
            closed.shutdown().await.expect("shutdown succeeds");
            closed
                .execute(request_with_command(
                    28,
                    Command::new("display-message").sensitive_arg(shutdown_secret),
                ))
                .await
                .expect_err("closed executor rejects the request")
        }
        .with_subscriber(subscriber(shutdown_buffer.clone()))
        .await;
        assert!(matches!(shutdown_error, Error::ExecutorShutdown { .. }));
        let shutdown_trace =
            String::from_utf8_lossy(&shutdown_buffer.0.lock().unwrap()).into_owned();
        assert_single_failure(&shutdown_trace, 28, &[shutdown_secret]);
    }

    #[cfg(feature = "tracing")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tracing_errors_and_sources_omit_sensitive_argv_and_raw_output() {
        use std::sync::Mutex;

        use tracing::instrument::WithSubscriber as _;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);

        struct Writer(Arc<Mutex<Vec<u8>>>);

        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Writer;

            fn make_writer(&'a self) -> Self::Writer {
                Writer(Arc::clone(&self.0))
            }
        }

        if !tracing_test_is_isolated_child(TRACE_SUPERVISOR_TEST).await {
            return;
        }

        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("tracing.pid");
        let buffer = Buffer::default();
        let scoped = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(buffer.clone())
            .finish();
        let (error, successful_result) = async {
            let reader_release = Arc::new(tokio::sync::Notify::new());
            let error_executor =
                executor("secret-block", TEST_TIMEOUT).with_test_hooks(TestHooks {
                    reader_failure: Some(ReaderFailure::Panic),
                    reader_failure_release: Some(Arc::clone(&reader_release)),
                    ..TestHooks::default()
                });
            let command = helper_command([pid_path.as_os_str().to_os_string()])
                .sensitive_arg("sentinel-argument-secret");
            let observed_path = pid_path.clone();
            let release_after_output = tokio::spawn(async move {
                let child_pid = read_pids(&observed_path, 1).await[0];
                reader_release.notify_one();
                child_pid
            });
            let error = error_executor
                .execute(request_with_command(23, command))
                .await
                .expect_err("reader panic becomes a typed error");
            let error_pid = release_after_output
                .await
                .expect("reader release task remains healthy");
            assert!(matches!(error, Error::ReadOutput { .. }));
            assert!(StdError::source(&error).is_none());
            assert_process_reaped(error_pid);
            error_executor.shutdown().await.expect("shutdown succeeds");

            let success_executor = executor("secret-success", TEST_TIMEOUT);
            let successful_result = success_executor
                .execute(request_with_command(
                    26,
                    helper_command([]).sensitive_arg("sentinel-success-argument"),
                ))
                .await
                .expect("successful command returns output data");
            success_executor
                .shutdown()
                .await
                .expect("shutdown succeeds");
            (error, successful_result)
        }
        .with_subscriber(scoped)
        .await;

        assert_error_redacted(
            &error,
            &[
                "sentinel-argument-secret",
                "sentinel-output-secret",
                "sentinel-reader-panic",
            ],
        );
        assert!(
            successful_result
                .stdout()
                .windows(b"sentinel-success-output".len())
                .any(|bytes| bytes == b"sentinel-success-output")
        );
        let trace = String::from_utf8_lossy(&buffer.0.lock().unwrap()).into_owned();
        assert!(
            trace.contains("request_id=23"),
            "captured trace did not include the safe request ID: {trace:?}"
        );
        assert!(trace.contains("tmux command requested"));
        assert!(trace.contains("tmux command failed"));
        assert!(trace.contains("tmux command finished"));
        assert!(trace.contains("stdout_len="));
        for secret in [
            "sentinel-argument-secret",
            "sentinel-output-secret",
            "sentinel-reader-panic",
            "sentinel-success-argument",
            "sentinel-success-output",
        ] {
            assert!(!trace.contains(secret), "trace leaked {secret}: {trace:?}");
        }
    }

    #[test]
    fn runtime_teardown_signals_the_group_without_claiming_library_reaping() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let pid_path = directory.path().join("runtime-drop.pid");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime builds");
        let pids = runtime.block_on(async {
            let executor = executor("descendant", TEST_TIMEOUT);
            let dispatch = executor.execute(request(24, [pid_path.as_os_str().to_os_string()]));
            tokio::spawn(dispatch);
            read_pids(&pid_path, 2).await
        });
        drop(runtime);

        let direct = pid(pids[0]);
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            match waitpid(Some(direct), WaitOptions::NOHANG) {
                Ok(Some((_pid, status))) => {
                    assert!(status.terminating_signal().is_some());
                    break;
                }
                Err(Errno::CHILD) => break,
                Ok(None) | Err(Errno::INTR) => {}
                outcome => panic!("unexpected direct-child wait outcome: {outcome:?}"),
            }
            assert!(Instant::now() < deadline, "direct child was not killed");
            std::thread::yield_now();
        }

        while !matches!(test_kill_process(direct), Err(Errno::SRCH)) {
            assert!(Instant::now() < deadline, "direct child did not disappear");
            std::thread::yield_now();
        }

        let descendant = pid(pids[1]);
        while !matches!(test_kill_process(descendant), Err(Errno::SRCH)) {
            assert!(Instant::now() < deadline, "descendant was not killed");
            std::thread::yield_now();
        }
    }
}
