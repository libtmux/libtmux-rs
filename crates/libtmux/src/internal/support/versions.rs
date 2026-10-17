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
pub(crate) fn dispatch(
    executor: Arc<dyn Executor>,
    configuration: CoreConfiguration,
    request: CommandRequest,
    endpoint_probe: RequestId,
    binary_probe: RequestId,
    absent_cleanup: bool,
    versions: Arc<VersionCache>,
) -> DispatchFuture {
    DispatchFuture::new(async move {
        let deadline = Instant::now().checked_add(configuration.timeout());
        let request_id = request.request_id();
        let summary = request.summary().clone();
        let unbounded = request.is_unbounded();
        let version_check = async {
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
                None => binary_version(&executor, binary_probe).await?,
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
        let request = if allow_start {
            request
        } else {
            request.live_only()
        };
        // The guard and effects share tmux's queue, so replacing its daemon
        // after the probe cannot run effects on an unverified version.
        let dispatch = executor.execute(request.fence_version(&version));
        let result = within(
            dispatch,
            if unbounded { None } else { deadline },
            request_id,
            &summary,
            configuration.timeout(),
        )
        .await?;
        if !result.success()
            && result.stderr().trim_ascii() == b"unknown command: libtmux-core-version-changed"
        {
            versions.forget(&version);
            return Err(Error::version_probe_failed(
                request_id.get(),
                crate::layout::version_command().summary(),
                result.exit_code(),
                result.signal(),
            ));
        }
        if (absent_cleanup && result.success()) || (!result.success() && is_cold(result.stderr())) {
            versions.forget(&version);
        } else if allow_start && result.success() {
            versions.remember(version);
        }
        Ok(result)
    })
}

async fn within<T>(
    future: impl Future<Output = Result<T, Error>>,
    deadline: Option<Instant>,
    request_id: RequestId,
    summary: &CommandSummary,
    budget: Duration,
) -> Result<T, Error> {
    let result = match deadline {
        Some(deadline) => timeout_at(deadline, future)
            .await
            .map_err(|_| Error::timeout(request_id.get(), summary.clone(), budget))?,
        None => future.await,
    };
    result.map_err(|error| match error {
        Error::Timeout { .. } => Error::timeout(request_id.get(), summary.clone(), budget),
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
        let version = binary_version(executor, binary_probe).await?;
        version.ensure_supported()?;
        Ok(version)
    }
}

async fn live_version(
    executor: &Arc<dyn Executor>,
    configuration: &CoreConfiguration,
    request_id: RequestId,
) -> Result<Option<TmuxVersion>, Error> {
    let command = crate::layout::version_command();
    #[cfg(feature = "control-mode")]
    let line = executor
        .renders_control_line()
        .then(|| command.control_mode_line());
    let request =
        CommandRequest::with_global_argv(request_id, configuration.global_argv(), command);
    #[cfg(feature = "control-mode")]
    let request = request.with_control_line(line.flatten());
    let result = executor.execute(request).await?;
    if result.success() {
        return TmuxVersion::parse_output(result.stdout()).map(Some);
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
    TmuxVersion::parse_output(result.stdout())
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

fn is_cold(stderr: &[u8]) -> bool {
    let reason = stderr.trim_ascii();
    reason.starts_with(b"no server running on ")
        || (reason.starts_with(b"error connecting to ")
            && reason.ends_with(b" (No such file or directory)"))
}

#[cfg(test)]
mod tests {
    use super::{is_cold, is_pre_probe_usage};

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
