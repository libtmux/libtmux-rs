use std::collections::{HashMap, hash_map::Entry};
use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::process::Stdio;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::{CancellationToken, Error};

mod parked;
use crate::command::{CommandRequest, CommandResult, CommandSummary, ProcessStatus, RequestId};
use crate::internal::executor::{DispatchFuture, DispatchSpan, Executor, ShutdownFuture};
use crate::internal::process::{
    LaunchContext, ProcessAdmission, ProcessChild, lock, output_pair, validate_request,
};
use crate::limits::{DispatchLimits, OutputLimits};

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
}

struct Shared {
    lifecycle: Mutex<Lifecycle>,
    empty: Condvar,
    admission: ProcessAdmission,
    wait_admission: ProcessAdmission,
}

struct Lifecycle {
    accepting: bool,
    entries: HashMap<RequestId, CancellationToken>,
    cleanup_failure: Option<(u64, CommandSummary, io::ErrorKind, String)>,
}

struct RequestContext {
    request_id: RequestId,
    command: CommandSummary,
    timeout: Duration,
    deadline: Option<Instant>,
}
impl RequestContext {
    fn request_id(&self) -> u64 {
        self.request_id.get()
    }

    fn check(&self, token: &CancellationToken, shutdown: &CancellationToken) -> Result<(), Error> {
        if shutdown.is_cancelled() {
            Err(Error::executor_shutdown(
                self.request_id(),
                self.command.clone(),
            ))
        } else if token.is_cancelled() {
            Err(Error::cancelled(self.request_id(), self.command.clone()))
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Err(Error::timeout(
                self.request_id(),
                self.command.clone(),
                self.timeout,
            ))
        } else {
            Ok(())
        }
    }
}

impl SubprocessExecutor {
    pub(crate) fn new(executable: impl Into<OsString>, timeout: Duration) -> Self {
        Self {
            configuration: Arc::new(Configuration {
                launch: LaunchContext::new(executable),
                timeout,
                output_limits: OutputLimits::default(),
            }),
            shared: Self::shared(DispatchLimits::default()),
        }
    }

    fn shared(limits: DispatchLimits) -> Arc<Shared> {
        Arc::new(Shared {
            lifecycle: Mutex::new(Lifecycle {
                accepting: true,
                entries: HashMap::new(),
                cleanup_failure: None,
            }),
            empty: Condvar::new(),
            admission: ProcessAdmission::new(limits.max_in_flight, limits.acquire_timeout),
            wait_admission: ProcessAdmission::new(limits.max_wait_clients, None),
        })
    }

    pub(crate) fn with_dispatch_limits(mut self, limits: DispatchLimits) -> Self {
        self.shared = Self::shared(limits);
        self
    }
    pub(crate) fn with_output_limits(mut self, limits: OutputLimits) -> Self {
        Arc::make_mut(&mut self.configuration).output_limits = limits;
        self
    }
    pub(crate) fn with_launch_context(mut self, launch: LaunchContext) -> Self {
        Arc::make_mut(&mut self.configuration).launch = launch;
        self
    }
    #[cfg(feature = "test-support")]
    pub(crate) fn with_synchronous_reap_on_supervisor_drop(self, _enabled: bool) -> Self {
        // Blocking dispatch always reaps its process before returning.
        self
    }
    #[cfg(test)]
    pub(crate) fn with_environment(
        mut self,
        key: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        let configuration = Arc::make_mut(&mut self.configuration);
        configuration.launch = configuration.launch.clone().with_environment(key, value);
        self
    }
    #[cfg(test)]
    pub(crate) fn active_request_count(&self) -> usize {
        lock(&self.shared.lifecycle).entries.len()
    }

    fn run(&self, request: &CommandRequest) -> Result<CommandResult, Error> {
        validate_request(&self.configuration.launch, request)?;
        let context = RequestContext {
            request_id: request.request_id(),
            command: request.summary().clone(),
            timeout: self.configuration.timeout,
            deadline: if request.is_unbounded() {
                None
            } else {
                Instant::now().checked_add(self.configuration.timeout)
            },
        };
        trace_requested(&context);
        let result = self.run_inner(request, &context);
        trace_finished(&context, &result);
        result
    }

    fn run_inner(
        &self,
        request: &CommandRequest,
        context: &RequestContext,
    ) -> Result<CommandResult, Error> {
        let token = request.cancellation_token();
        if token.is_cancelled() {
            return Err(Error::cancelled(
                context.request_id(),
                context.command.clone(),
            ));
        }
        // A parked wait-for client must leave room for the command that releases it.
        let _permit = if request.is_unbounded() {
            self.shared.wait_admission.acquire(
                context.request_id,
                &context.command,
                Some(Instant::now()),
                token,
            )?
        } else {
            self.shared.admission.acquire(
                context.request_id,
                &context.command,
                context.deadline,
                token,
            )?
        };
        let (_registry, cancelled) = self.register(context)?;
        let check = || context.check(token, &cancelled);
        check()?;
        let io_error =
            |source| Error::wait_child(context.request_id(), context.command.clone(), source);
        let (stdout, child_stdout) = output_pair().map_err(io_error)?;
        let (stderr, child_stderr) = output_pair().map_err(io_error)?;
        let mut process = self.configuration.launch.command(request.argv());
        process
            .stdin(Stdio::null())
            .stdout(child_stdout)
            .stderr(child_stderr);
        let mut child =
            ProcessChild::spawn(&self.configuration.launch, &mut process).map_err(|source| {
                let missing = (source.kind() == io::ErrorKind::NotFound
                    && self
                        .configuration
                        .launch
                        .current_dir()
                        .is_none_or(std::path::Path::is_dir))
                    || self.configuration.launch.executable_missing_from_path();
                Error::spawn(
                    context.request_id(),
                    context.command.clone(),
                    source,
                    missing,
                )
            })?;
        drop(process);
        let stdout = OutputReader::new(
            stdout,
            "stdout",
            self.configuration.output_limits.stdout_bytes(),
        );
        let stderr = OutputReader::new(
            stderr,
            "stderr",
            self.configuration.output_limits.stderr_bytes(),
        );
        let (output, status) = if request.is_unbounded() {
            parked::collect(
                &mut child,
                [stdout, stderr],
                context,
                token,
                &cancelled,
                check,
            )
        } else {
            let output = collect_bounded(&mut child, [stdout, stderr], context, check);
            // Kill descendants while the unreaped leader still pins the process-group ID.
            (output, child.finish())
        };
        let status = status.map_err(|source| {
            let mut lifecycle = lock(&self.shared.lifecycle);
            lifecycle.cleanup_failure.get_or_insert_with(|| {
                (
                    context.request_id(),
                    context.command.clone(),
                    source.kind(),
                    source.to_string(),
                )
            });
            io_error(source)
        })?;
        let (stdout, stderr) = output?;
        Ok(CommandResult::new(
            context.request_id,
            context.command.clone(),
            ProcessStatus::from_exit_status(status),
            stdout,
            stderr,
        ))
    }

    fn register(
        &self,
        context: &RequestContext,
    ) -> Result<(RegistryGuard, CancellationToken), Error> {
        let cancelled = CancellationToken::new();
        let mut lifecycle = lock(&self.shared.lifecycle);
        if !lifecycle.accepting {
            return Err(Error::executor_shutdown(
                context.request_id(),
                context.command.clone(),
            ));
        }
        match lifecycle.entries.entry(context.request_id) {
            Entry::Vacant(entry) => {
                entry.insert(cancelled.clone());
            }
            Entry::Occupied(_) => {
                return Err(Error::duplicate_request(
                    context.request_id(),
                    context.command.clone(),
                ));
            }
        }
        Ok((
            RegistryGuard {
                shared: Arc::clone(&self.shared),
                request_id: context.request_id,
            },
            cancelled,
        ))
    }
}

fn collect_bounded(
    child: &mut ProcessChild,
    [mut stdout, mut stderr]: [OutputReader; 2],
    context: &RequestContext,
    check: impl Fn() -> Result<(), Error>,
) -> Result<(Vec<u8>, Vec<u8>), Error> {
    loop {
        check()?;
        // One bounded read per stream prevents a noisy stream starving the other.
        let progress = match (stdout.read(context), stderr.read(context)) {
            (Ok(left), Ok(right)) => left || right,
            (Err(error), _) | (_, Err(error)) => return Err(error),
        };
        let exited = child.exited().map_err(|source| {
            Error::wait_child(context.request_id(), context.command.clone(), source)
        })?;
        if exited && stdout.eof && stderr.eof {
            return Ok((stdout.bytes, stderr.bytes));
        }
        if !progress {
            let pause = context
                .deadline
                .map_or(Duration::from_millis(1), |deadline| {
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(1))
                });
            std::thread::sleep(pause);
        }
    }
}

impl Executor for SubprocessExecutor {
    fn execute(&self, request: CommandRequest) -> DispatchFuture {
        let executor = self.clone();
        DispatchSpan::new(&request, "subprocess").run(move || executor.run(&request))
    }
    fn shutdown(&self) -> ShutdownFuture {
        let shared = Arc::clone(&self.shared);
        ShutdownFuture::new(move || {
            shared.admission.close();
            shared.wait_admission.close();
            let tokens = {
                let mut lifecycle = lock(&shared.lifecycle);
                lifecycle.accepting = false;
                lifecycle.entries.values().cloned().collect::<Vec<_>>()
            };
            for token in tokens {
                token.cancel();
            }
            let mut lifecycle = lock(&shared.lifecycle);
            while !lifecycle.entries.is_empty() {
                lifecycle = shared
                    .empty
                    .wait(lifecycle)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            match &lifecycle.cleanup_failure {
                Some((request_id, command, kind, message)) => Err(Error::wait_child(
                    *request_id,
                    command.clone(),
                    io::Error::new(*kind, message.clone()),
                )),
                None => Ok(()),
            }
        })
    }
}

struct RegistryGuard {
    shared: Arc<Shared>,
    request_id: RequestId,
}
impl Drop for RegistryGuard {
    fn drop(&mut self) {
        lock(&self.shared.lifecycle)
            .entries
            .remove(&self.request_id);
        self.shared.empty.notify_all();
    }
}

struct OutputReader {
    stream: UnixStream,
    name: &'static str,
    limit: usize,
    bytes: Vec<u8>,
    eof: bool,
}
impl OutputReader {
    fn new(stream: UnixStream, name: &'static str, limit: usize) -> Self {
        Self {
            stream,
            name,
            limit,
            bytes: Vec::new(),
            eof: false,
        }
    }
    fn read(&mut self, context: &RequestContext) -> Result<bool, Error> {
        if self.eof {
            return Ok(false);
        }
        let mut buffer = [0; 8192];
        let remaining = self
            .limit
            .saturating_sub(self.bytes.len())
            .saturating_add(1)
            .min(buffer.len());
        match self.stream.read(&mut buffer[..remaining]) {
            Ok(0) => {
                self.eof = true;
                Ok(true)
            }
            Ok(count) if self.bytes.len().saturating_add(count) > self.limit => {
                Err(Error::OutputLimitExceeded {
                    request_id: context.request_id(),
                    command: context.command.clone(),
                    stream: self.name,
                    limit: self.limit,
                })
            }
            Ok(count) => {
                self.bytes.extend_from_slice(&buffer[..count]);
                Ok(true)
            }
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(source) => Err(Error::read_output(
                context.request_id(),
                context.command.clone(),
                self.name,
                source.kind(),
            )),
        }
    }
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
#[path = "subprocess/sync_tests.rs"]
mod tests;
