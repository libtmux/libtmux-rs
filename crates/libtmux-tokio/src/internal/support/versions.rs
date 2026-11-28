use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{Instant, timeout_at};

use crate::command::{CommandRequest, CommandResult, CommandSummary, ProcessStatus, RequestId};
use crate::internal::core::CoreConfiguration;
use crate::internal::executor::{DispatchFuture, Executor};
use crate::{Command, Error, TmuxVersion};

#[derive(Default)]
pub(crate) struct VersionCache {
    version: Mutex<Option<TmuxVersion>>,
    initializing: tokio::sync::Mutex<()>,
}

impl VersionCache {
    fn get(&self) -> Option<TmuxVersion> {
        self.version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn remember(&self, version: TmuxVersion) {
        *self
            .version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(version);
    }

    fn forget(&self, expected: &TmuxVersion) {
        let mut version = self
            .version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if version.as_ref() == Some(expected) {
            *version = None;
        }
    }
}

#[allow(
    clippy::await_holding_invalid_type,
    reason = "serializes the first probe; executor dispatch never reacquires this guard"
)]
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "version admission and operation dispatch share one absolute deadline"
)]
pub(crate) fn dispatch(
    executor: Arc<dyn Executor>,
    configuration: CoreConfiguration,
    request: CommandRequest,
    endpoint_probe: RequestId,
    binary_probe: RequestId,
    absent_cleanup: bool,
    versions: Arc<VersionCache>,
    operation: Option<crate::internal::operation::OperationContext>,
) -> DispatchFuture {
    DispatchFuture::new(async move {
        let deadline = operation.as_ref().map_or_else(
            || Instant::now().checked_add(configuration.timeout()),
            crate::internal::operation::OperationContext::deadline,
        );
        let request_id = request.request_id();
        let summary = request.summary().clone();
        let unbounded = request.is_unbounded();
        let version_check = async {
            #[cfg(feature = "control-mode")]
            if let Some(version) = executor.checked_control_version() {
                return Ok(Some((version.clone(), false)));
            }
            if let Some(version) = versions.get().filter(|_| !absent_cleanup) {
                return Ok(Some((version, false)));
            }
            let _initialization = versions.initializing.lock().await;
            if let Some(version) = versions.get().filter(|_| !absent_cleanup) {
                return Ok(Some((version, false)));
            }
            let live = live_version(&executor, &configuration, endpoint_probe).await?;
            if absent_cleanup && live.is_none() {
                if let Some(version) = versions.get() {
                    versions.forget(&version);
                }
                return Ok(None);
            }
            let allow_start = live.is_none();
            let version = match live {
                Some(version) => version,
                None => binary_version(&executor, &configuration, binary_probe).await?,
            };
            version.ensure_supported()?;
            if !allow_start {
                versions.remember(version.clone());
            }
            Ok(Some((version, allow_start)))
        };
        let Some((version, allow_start)) = within(
            version_check,
            deadline,
            request_id,
            &summary,
            configuration.timeout(),
            true,
        )
        .await?
        else {
            return Ok(CommandResult::new(
                request_id,
                summary,
                ProcessStatus::from_block_outcome(true),
                Vec::new(),
                Vec::new(),
            ));
        };
        request.require_features(&version)?;
        let request = if allow_start {
            request
        } else {
            request.live_only()
        };
        // Native clients put their version check and effects on the same queue.
        let result = if let Some(operation) = operation {
            match executor
                .execute_operation(request.fence_ordinary_version(&version), operation)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    versions.forget(&version);
                    return Err(error);
                }
            }
        } else {
            // A challenged persistent client cannot reconnect to a replacement.
            #[cfg(feature = "control-mode")]
            let request = if executor.renders_control_line() {
                request
            } else {
                request.fence_ordinary_version(&version)
            };
            #[cfg(not(feature = "control-mode"))]
            let request = request.fence_ordinary_version(&version);
            within(
                executor.execute(request),
                if unbounded { None } else { deadline },
                request_id,
                &summary,
                configuration.timeout(),
                false,
            )
            .await?
        };
        if !result.success() && result.stderr().trim_ascii() == b"syntax error" {
            // The ordinary guard has no private response block. Invalidating
            // this observation does not establish whether the action ran.
            versions.forget(&version);
        }
        if (absent_cleanup && result.success()) || (!result.success() && is_cold(result.stderr())) {
            versions.forget(&version);
        } else if allow_start && result.success() {
            versions.remember(version);
        }
        Ok(result)
    })
}

pub(in crate::internal) async fn within<T>(
    future: impl Future<Output = Result<T, Error>>,
    deadline: Option<Instant>,
    request_id: RequestId,
    summary: &CommandSummary,
    budget: Duration,
    before_dispatch: bool,
) -> Result<T, Error> {
    let expired = || {
        if before_dispatch {
            Error::timeout_before_dispatch(request_id.get(), summary.clone(), budget)
        } else {
            Error::timeout(request_id.get(), summary.clone(), budget)
        }
    };
    let result = match deadline {
        Some(deadline) => timeout_at(deadline, future).await.map_err(|_| expired())?,
        None => future.await,
    };
    result.map_err(|error| match error {
        Error::Timeout { .. } => expired(),
        other => other,
    })
}

#[allow(
    clippy::await_holding_invalid_type,
    reason = "coalesces the format probe; endpoint probing never reacquires this guard"
)]
pub(crate) async fn format_version(
    executor: &Arc<dyn Executor>,
    configuration: &CoreConfiguration,
    endpoint_probe: RequestId,
    binary_probe: RequestId,
    versions: &VersionCache,
) -> Result<TmuxVersion, Error> {
    #[cfg(feature = "control-mode")]
    if let Some(version) = executor.checked_control_version() {
        return Ok(version.clone());
    }
    if let Some(version) = versions.get() {
        return Ok(version);
    }
    let _initialization = versions.initializing.lock().await;
    if let Some(version) = versions.get() {
        return Ok(version);
    }
    endpoint_version(
        executor,
        configuration,
        endpoint_probe,
        binary_probe,
        versions,
    )
    .await
}

pub(crate) async fn endpoint_version(
    executor: &Arc<dyn Executor>,
    configuration: &CoreConfiguration,
    endpoint_probe: RequestId,
    binary_probe: RequestId,
    versions: &VersionCache,
) -> Result<TmuxVersion, Error> {
    if let Some(version) = live_version(executor, configuration, endpoint_probe).await? {
        version.ensure_supported()?;
        versions.remember(version.clone());
        Ok(version)
    } else {
        let version = binary_version(executor, configuration, binary_probe).await?;
        version.ensure_supported()?;
        Ok(version)
    }
}

async fn live_version(
    executor: &Arc<dyn Executor>,
    configuration: &CoreConfiguration,
    request_id: RequestId,
) -> Result<Option<TmuxVersion>, Error> {
    // Over a control connection the probe's refusal is printed outside any
    // block, so it cannot be read back. A routed handle is seeded with the
    // version its connection attached under, and has no other source.
    #[cfg(feature = "control-mode")]
    if executor.renders_control_line() {
        return executor
            .checked_control_version()
            .cloned()
            .map(Some)
            .ok_or_else(|| {
                Error::control_mode(std::io::Error::other(
                    "a control-routed handle holds no checked daemon version",
                ))
            });
    }
    let probe = crate::layout::VersionProbe::new();
    let command = probe.command();
    #[cfg(feature = "control-mode")]
    let line = executor
        .renders_control_line()
        .then(|| command.control_mode_line());
    // `-N` keeps a vanished daemon from being restarted by this client.
    let mut global_argv = configuration.global_argv().to_vec();
    global_argv.push(std::ffi::OsString::from("-N"));
    // The answer is on stderr and in the exit status. A tmux 3.1 or older
    // daemon keeps the client's stdout open after the client exits, so a
    // piped stdout would hold the refusal until the deadline.
    let request =
        CommandRequest::with_global_argv(request_id, &global_argv, command).discarding_stdout();
    #[cfg(feature = "control-mode")]
    let request = request.with_control_line(line.flatten());
    let result = executor.execute(request).await?;
    if result.success() {
        // `run-shell -C` of a non-command always fails; success is another
        // program answering in tmux's place.
        return Err(Error::from_invalid_version_output(result.stderr().len()));
    }
    if let Some(version) = probe.parse(result.stderr())? {
        return Ok(Some(version));
    }
    if is_cold(result.stderr()) {
        return Ok(None);
    }
    // `run-shell` gained `-C` in tmux 3.2 and the client's `-N` earlier still:
    // an older daemon answers the probe with a usage line, not the marker.
    if is_pre_probe_usage(result.stderr()) {
        return Err(Error::unsupported_tmux_version(
            TmuxVersion::older_than_probe(),
            TmuxVersion::MIN_SUPPORTED,
        ));
    }
    Err(Error::version_probe_failed(
        request_id.get(),
        result.command().clone(),
        result.exit_code(),
        result.signal(),
    ))
}

async fn binary_version(
    executor: &Arc<dyn Executor>,
    configuration: &CoreConfiguration,
    request_id: RequestId,
) -> Result<TmuxVersion, Error> {
    let result = executor
        .execute(CommandRequest::new(request_id, Command::new("-V")))
        .await?;
    if !result.success() {
        return Err(Error::version_probe_failed(
            request_id.get(),
            result.command().clone(),
            result.exit_code(),
            result.signal(),
        ));
    }
    TmuxVersion::parse_executable_output(configuration.executable(), result.stdout())
        .map_err(Into::into)
}

/// What tmux 3.1 and older answer the probe with.
///
/// A 3.1 daemon prints `run-shell`'s usage, having no `-C`. A 3.0 daemon
/// refuses a newer client's protocol. A 3.1 or older client rejects `-N`
/// before it connects.
fn is_pre_probe_usage(stderr: &[u8]) -> bool {
    let reason = stderr.trim_ascii();
    reason.starts_with(b"usage: run-shell ")
        || reason.starts_with(b"server version is too old for client")
        || reason.starts_with(b"usage: tmux ")
        || reason.starts_with(b"tmux: unknown option")
}

/// Whether tmux found no daemon at the endpoint.
///
/// A file that is not a socket answers `ECONNREFUSED` on Linux but `ENOTSOCK`
/// on macOS, so both spellings mean the same stale path.
fn is_cold(stderr: &[u8]) -> bool {
    let reason = stderr.trim_ascii();
    reason.starts_with(b"no server running on ")
        || (reason.starts_with(b"error connecting to ")
            && (reason.ends_with(b" (No such file or directory)")
                || reason.ends_with(b" (Connection refused)")
                || reason.ends_with(b" (Socket operation on non-socket)")))
}

#[cfg(test)]
mod tests {
    use super::{is_cold, is_pre_probe_usage};

    #[cfg(feature = "test-support")]
    fn fixture_configuration(
        server: &crate::Server,
        timeout: std::time::Duration,
    ) -> crate::internal::core::CoreConfiguration {
        use crate::internal::core::{BuildContext, CoreConfiguration, SocketSelection};

        CoreConfiguration::resolve(
            &SocketSelection::Path(server.socket_path().to_owned()),
            None,
            None,
            server.tmux_executable().to_os_string(),
            timeout,
            BuildContext::capture(),
        )
        .expect("private endpoint configuration")
    }

    #[cfg(all(feature = "control-mode", feature = "test-support"))]
    #[tokio::test]
    async fn a_control_gate_uses_its_challenged_version_after_native_cache_changes() {
        use std::sync::Arc;

        use super::{VersionCache, dispatch, format_version};
        use crate::command::{CommandRequest, RequestId};
        use crate::control::ControlMode;
        use crate::internal::control_executor::ControlModeExecutor;
        use crate::internal::executor::Executor;
        use crate::test::TestServer;
        use crate::{Command, OutputLimits, ReleaseSuffix, ReleaseVersion, TmuxVersion};

        let fixture = TestServer::new().await.expect("private daemon starts");
        let server = fixture.server();
        if !server
            .daemon_version()
            .await
            .expect("daemon version")
            .has_behavior(&crate::since::CONTROL_ACQUISITION)
        {
            fixture.shutdown().await.expect("ineligible fixture stops");
            return;
        }
        let session = fixture.session("pinned-version").await.expect("session");
        let (sender, events) = ControlMode::attach(server, session.id())
            .await
            .expect("qualified client")
            .split();
        let checked = sender.version().clone();
        let executor: Arc<dyn Executor> =
            Arc::new(ControlModeExecutor::new(sender, OutputLimits::default()));
        let configuration = fixture_configuration(server, server.default_timeout());
        let versions = Arc::new(VersionCache::default());
        let stale =
            TmuxVersion::parse_output(b"tmux 99.0\n").expect("synthetic replacement report");
        versions.remember(stale.clone());
        assert_eq!(
            format_version(
                &executor,
                &configuration,
                RequestId::new(1),
                RequestId::new(2),
                &versions
            )
            .await
            .expect("pinned format version"),
            checked
        );
        let command = Command::new("set-environment")
            .arg("-g")
            .arg("LIBTMUX_BAD_CACHE")
            .arg("changed")
            .requires_tmux(
                "synthetic future capability",
                ReleaseVersion::new(99, 0, ReleaseSuffix::FINAL),
            );
        let line = command.control_mode_line();
        let request = CommandRequest::with_global_argv(
            RequestId::new(3),
            configuration.global_argv(),
            command,
        )
        .with_control_line(line);
        let error = dispatch(
            executor.clone(),
            configuration.clone(),
            request,
            RequestId::new(4),
            RequestId::new(5),
            false,
            Arc::clone(&versions),
            None,
        )
        .await
        .expect_err("the stale endpoint report cannot admit a control command");
        assert_eq!(error.kind(), crate::ErrorKind::UnsupportedVersion);
        versions.forget(&stale);
        assert_eq!(
            format_version(
                &executor,
                &configuration,
                RequestId::new(6),
                RequestId::new(7),
                &versions
            )
            .await
            .expect("clearing endpoint cache does not clear actor qualification"),
            checked
        );
        let unchanged = server
            .cmd(
                Command::new("show-environment")
                    .arg("-g")
                    .arg("LIBTMUX_BAD_CACHE"),
            )
            .await
            .expect("inspect daemon environment");
        assert!(!unchanged.success());
        drop(executor);
        events.shutdown().await.expect("control client stops");
        fixture.shutdown().await.expect("private daemon stops");
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn a_typed_fence_failure_invalidates_its_cached_version_without_replay() {
        use std::sync::Arc;
        use std::time::Duration;

        use libtmux_core::{Effect, NewSession, Reply};

        use super::{VersionCache, dispatch};
        use crate::command::{CommandRequest, RequestId};
        use crate::internal::executor::Executor as _;
        use crate::internal::operation::{OperationContext, nonce};
        use crate::internal::subprocess::SubprocessExecutor;
        use crate::test::{TestServer, scaled, unique_name};
        use crate::{Command, TmuxVersion};

        let fixture = TestServer::new().await.expect("private fixture starts");
        let server = fixture.server();
        let name = unique_name("unreplayed-stale-fence");
        let timeout = scaled(Duration::from_secs(5));
        let configuration = fixture_configuration(server, timeout);
        let executor = Arc::new(SubprocessExecutor::new(server.tmux_executable(), timeout));
        let versions = Arc::new(VersionCache::default());
        versions.remember(
            TmuxVersion::parse_output(b"tmux 99.0\n").expect("deliberately stale version"),
        );
        let nonce = nonce().expect("fresh receipt nonce");
        let operation = NewSession::new(&name);
        let request = CommandRequest::native_argv(
            RequestId::new(1),
            configuration.global_argv(),
            operation.ordinary_arguments(nonce),
            Command::new("new-session").summary(),
        );
        let result = dispatch(
            executor.clone(),
            configuration,
            request,
            RequestId::new(2),
            RequestId::new(3),
            false,
            Arc::clone(&versions),
            Some(OperationContext::creation(
                server.socket_path(),
                nonce,
                tokio::time::Instant::now().checked_add(timeout),
            )),
        )
        .await
        .expect("native guard returns its status");
        assert!(!result.success());
        let status = result.exit_code();
        let (stdout, stderr) = result.into_streams();
        let error = Reply::new(status, stdout, stderr.len(), false)
            .ordinary_creation(operation, server.socket_path(), nonce)
            .expect_err("there is no acknowledged action to replay");
        assert_eq!(error.effect(), Effect::Unknown);
        assert!(
            versions.get().is_none(),
            "the next independent request must probe afresh"
        );
        assert!(
            !server
                .has_session(&name)
                .await
                .expect("exact session lookup")
        );
        executor.shutdown().await.expect("request owner finishes");
        fixture
            .shutdown()
            .await
            .expect("private fixture shuts down");
    }

    #[test]
    fn pre_probe_answers_from_old_daemons_and_clients_are_recognized() {
        for reason in [
            b"usage: run-shell [-b] [-t target-pane] shell-command\n".as_slice(),
            b"server version is too old for client\n",
            b"tmux: unknown option -- N\n",
            b"usage: tmux [-2CluvV] [-c shell-command] [-f file] [-L socket-name]\n",
        ] {
            assert!(is_pre_probe_usage(reason), "{}", reason.escape_ascii());
        }
        for reason in [
            b"unknown command: libtmux-version-0-3.3a\n".as_slice(),
            b"server exited unexpectedly\n",
            b"protocol version mismatch\n",
            b"no server running on /socket\n",
        ] {
            assert!(!is_pre_probe_usage(reason), "{}", reason.escape_ascii());
        }
    }

    #[test]
    fn version_check_cold_diagnostics_exclude_live_or_permission_failures() {
        for reason in [
            b"no server running on /socket\n".as_slice(),
            b"error connecting to /socket (No such file or directory)\n",
            b"error connecting to /socket (Connection refused)\n",
            b"error connecting to /socket (Socket operation on non-socket)\n",
        ] {
            assert!(is_cold(reason));
        }
        for reason in [
            b"server exited unexpectedly".as_slice(),
            b"error connecting to /socket (Permission denied)\n",
            b"protocol version mismatch",
            b"can't find session: absent",
        ] {
            assert!(!is_cold(reason));
        }
    }
}
