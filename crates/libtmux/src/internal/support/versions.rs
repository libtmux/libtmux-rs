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
            let live = live_version(&executor, &configuration, endpoint_probe, &versions).await?;
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
            false,
        )
        .await?;
        if !result.success()
            && result.stderr().trim_ascii() == b"unknown command: libtmux-core-version-changed"
        {
            // Ask the replacement what it is, so a caller that stops after one
            // failure still learns the floor. The probe shares the caller's
            // remaining deadline; nothing was sent, so it times out as such.
            let replacement = within(
                live_version(&executor, &configuration, endpoint_probe, &versions),
                deadline,
                request_id,
                &summary,
                configuration.timeout(),
                true,
            )
            .await;
            versions.forget(&version);
            return Err(match replacement {
                Ok(Some(replacement)) => match replacement.ensure_supported() {
                    Ok(()) => Error::server_replaced(request_id.get(), summary.clone()),
                    Err(unsupported) => unsupported,
                },
                Err(
                    error @ (Error::Timeout { .. }
                    | Error::UnsupportedTmuxVersion { .. }
                    | Error::UnrecognizedTmuxVersion { .. }),
                ) => error,
                Ok(None) | Err(_) => Error::server_replaced(request_id.get(), summary.clone()),
            });
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
    if let Some(version) = live_version(executor, configuration, endpoint_probe, versions).await? {
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
    #[cfg_attr(not(feature = "control-mode"), allow(unused_variables))] versions: &VersionCache,
) -> Result<Option<TmuxVersion>, Error> {
    // Over a control connection the probe's refusal is printed outside any
    // block, so it cannot be read back. A routed handle is seeded with the
    // version its connection attached under, and has no other source.
    #[cfg(feature = "control-mode")]
    if executor.renders_control_line() {
        return versions.get().map(Some).ok_or_else(|| {
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
    let output = result.stdout();
    // tmux 3.3a prints argv[0]'s basename; newer releases print "tmux".
    // Only the configured name can qualify the older native framing.
    let configured = std::path::Path::new(configuration.executable())
        .file_name()
        .and_then(|program| output.strip_prefix(program.as_encoded_bytes()))
        .and_then(|rest| rest.strip_prefix(b" "));
    let version = configured
        .or_else(|| output.strip_prefix(b"tmux "))
        .ok_or_else(|| Error::from_invalid_version_output(output.len()))?;
    let mut canonical = b"tmux ".to_vec();
    canonical.extend_from_slice(version);
    TmuxVersion::parse_report(&canonical, output.len())
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
            && (reason.ends_with(b" (No such file or directory)")
                || reason.ends_with(b" (Connection refused)")))
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
            b"error connecting to /socket (Connection refused)\n",
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
