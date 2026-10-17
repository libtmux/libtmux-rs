use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::{Instant, timeout_at};

use super::core::CoreConfiguration;
use super::executor::{DispatchFuture, Executor};
use crate::command::{CommandRequest, CommandResult, CommandSummary, ProcessStatus, RequestId};
use crate::{Command, Error, TmuxVersion};

#[derive(Default)]
pub(crate) struct VersionState {
    version: Mutex<Option<TmuxVersion>>,
    initializing: tokio::sync::Mutex<()>,
    startup_allowed: tokio::sync::OnceCell<bool>,
}

impl VersionState {
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
    versions: Arc<VersionState>,
) -> DispatchFuture {
    DispatchFuture::new(async move {
        let deadline = Instant::now().checked_add(configuration.timeout());
        let request_id = request.request_id();
        let summary = request.summary().clone();
        let unbounded = request.is_unbounded();
        let admission = async {
            if let Some(version) = versions.get().filter(|_| !absent_cleanup) {
                return Ok(Some(version));
            }
            let _initialization = versions.initializing.lock().await;
            if let Some(version) = versions.get().filter(|_| !absent_cleanup) {
                return Ok(Some(version));
            }
            let live = live_version(&executor, &configuration, endpoint_probe).await?;
            if absent_cleanup && live.is_none() {
                return Ok(None);
            }
            let version = if let Some(version) = live {
                version
            } else {
                let version = binary_version(&executor, binary_probe).await?;
                version.ensure_supported()?;
                let _ = versions.startup_allowed.set(true);
                version
            };
            version.ensure_supported()?;
            versions.remember(version.clone());
            Ok(Some(version))
        };
        let Some(version) = within(
            admission,
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
        let startup_allowed = within(
            async {
                Ok(*versions
                    .startup_allowed
                    .get_or_init(|| async {
                        binary_version(&executor, binary_probe)
                            .await
                            .is_ok_and(|version| version.ensure_supported().is_ok())
                    })
                    .await)
            },
            deadline,
            request_id,
            &summary,
            configuration.timeout(),
        )
        .await?;
        let request = if startup_allowed {
            request
        } else {
            request.live_only()
        };
        // The guard and effects share tmux's queue, so replacing its daemon
        // after the probe cannot admit effects on an unverified version.
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
    versions: &VersionState,
) -> Result<TmuxVersion, Error> {
    if let Some(version) = versions.get() {
        return Ok(version);
    }
    let _initialization = versions.initializing.lock().await;
    if let Some(version) = versions.get() {
        return Ok(version);
    }
    let version = endpoint_version(executor, configuration, endpoint_probe, binary_probe).await?;
    version.ensure_supported()?;
    versions.remember(version.clone());
    Ok(version)
}

pub(crate) async fn endpoint_version(
    executor: &Arc<dyn Executor>,
    configuration: &CoreConfiguration,
    endpoint_probe: RequestId,
    binary_probe: RequestId,
) -> Result<TmuxVersion, Error> {
    match live_version(executor, configuration, endpoint_probe).await? {
        Some(version) => Ok(version),
        None => binary_version(executor, binary_probe).await,
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

fn is_cold(stderr: &[u8]) -> bool {
    let reason = stderr.trim_ascii();
    reason.starts_with(b"no server running on ")
        || (reason.starts_with(b"error connecting to ")
            && reason.ends_with(b" (No such file or directory)"))
}

#[cfg(test)]
mod tests {
    use super::is_cold;

    #[test]
    fn admission_cold_diagnostics_exclude_live_or_permission_failures() {
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
