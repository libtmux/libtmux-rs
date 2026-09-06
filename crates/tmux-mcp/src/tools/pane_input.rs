use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

use libtmux::Command;
use rmcp::model::ErrorData;

use crate::TmuxTools;
use crate::run_request::{self, RunLease};

use super::error::{bad_input, object_gone, tmux_error, vanished};

#[derive(Clone, Copy)]
pub(crate) enum PaneInputReach {
    TargetOnly,
    Synchronized,
}

#[derive(Clone, Copy)]
pub(crate) enum MissingSource {
    CallerInput,
    ObservedTransition,
    PasteTransition,
}

pub(crate) struct PaneInputPlan {
    pub(crate) target: libtmux::Pane,
    pub(crate) configured: Vec<String>,
    pub(crate) endpoint: PathBuf,
}

const CLIENT_ATTENTION_FORMAT: &str = "#{client_control_mode}|#{pane_id}|#{window_zoomed_flag}";

fn client_attention_error(detail: &str) -> ErrorData {
    ErrorData::internal_error(
        format!("tmux returned malformed client attention state: {detail}"),
        Some(serde_json::json!({
            "kind": "decode",
            "retryable": false,
            "stale": false,
        })),
    )
}

fn endpoint_error(message: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(
        message.into(),
        Some(serde_json::json!({
            "kind": "decode",
            "retryable": false,
            "stale": false,
        })),
    )
}

pub(crate) fn active_run_error(pane: &str) -> ErrorData {
    ErrorData::internal_error(
        format!(
            "pane {pane} has an active run_shell_command; wait for its completion or pane closure before sending more input"
        ),
        Some(serde_json::json!({
            "kind": "active_run",
            "retryable": true,
            "stale": false,
        })),
    )
}

fn parse_flag(value: &[u8]) -> Result<bool, &'static str> {
    match value {
        b"0" => Ok(false),
        b"1" => Ok(true),
        _ => Err("a client flag was not exactly 0 or 1"),
    }
}

fn parse_pane_id(value: &[u8]) -> Result<String, &'static str> {
    let text = std::str::from_utf8(value).map_err(|_| "a client pane ID was not UTF-8")?;
    let parsed = text
        .parse::<libtmux::PaneId>()
        .map_err(|_| "a client pane ID was invalid")?;
    if parsed.to_string() != text {
        return Err("a client pane ID was not canonical");
    }
    Ok(text.to_owned())
}

fn parse_attended_panes(
    stdout: &[u8],
    window_panes: &BTreeSet<String>,
) -> Result<BTreeSet<String>, &'static str> {
    if stdout.is_empty() {
        return Ok(BTreeSet::new());
    }
    let rows = stdout
        .strip_suffix(b"\n")
        .ok_or("the client listing ended without a row terminator")?;
    let mut attended = BTreeSet::new();
    for row in rows.split(|byte| *byte == b'\n') {
        if row.is_empty() {
            return Err("the client listing contained an empty row");
        }
        let mut fields = row.split(|byte| *byte == b'|');
        let control = parse_flag(fields.next().ok_or("a client row had too few fields")?)?;
        let active = parse_pane_id(fields.next().ok_or("a client row had too few fields")?)?;
        let zoomed = parse_flag(fields.next().ok_or("a client row had too few fields")?)?;
        if fields.next().is_some() {
            return Err("a client row had too many fields");
        }
        if control || !window_panes.contains(&active) {
            continue;
        }
        if zoomed {
            attended.insert(active);
        } else {
            attended.extend(window_panes.iter().cloned());
        }
    }
    Ok(attended)
}

impl TmuxTools {
    async fn pane_input_endpoint(&self) -> Result<PathBuf, ErrorData> {
        let result = self
            .server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("#{socket_path}"),
            )
            .await
            .map_err(|error| tmux_error(&error))?;
        if let Some(error) = result.refusal_for("display-message") {
            return Err(tmux_error(&error));
        }
        let endpoint = result
            .stdout()
            .strip_suffix(b"\n")
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                endpoint_error("tmux returned no resolved socket path for pane input")
            })?;
        Ok(PathBuf::from(OsString::from_vec(endpoint.to_vec())))
    }

    pub(crate) async fn preflight_pane_input(
        &self,
        pane: &str,
        reach: PaneInputReach,
        missing: MissingSource,
    ) -> Result<PaneInputPlan, ErrorData> {
        self.preflight_pane_input_with_run(pane, reach, missing, None)
            .await
    }

    pub(crate) async fn preflight_pane_input_for_run(
        &self,
        pane: &str,
        reach: PaneInputReach,
        missing: MissingSource,
        lease: &RunLease,
    ) -> Result<PaneInputPlan, ErrorData> {
        self.preflight_pane_input_with_run(pane, reach, missing, Some(lease))
            .await
    }

    async fn preflight_pane_input_with_run(
        &self,
        pane: &str,
        reach: PaneInputReach,
        missing: MissingSource,
        lease: Option<&RunLease>,
    ) -> Result<PaneInputPlan, ErrorData> {
        let panes = self
            .server
            .panes()
            .await
            .map_err(|error| tmux_error(&error))?;
        let source = panes
            .iter()
            .find(|candidate| candidate.id().to_string() == pane)
            .cloned()
            .ok_or_else(|| match missing {
                MissingSource::CallerInput => object_gone("pane", pane),
                MissingSource::ObservedTransition => {
                    vanished(&format!("pane {pane} disappeared between run checkpoints"))
                }
                MissingSource::PasteTransition => {
                    vanished(&format!("pane {pane} disappeared before paste dispatch"))
                }
            })?;

        let mut selected = BTreeMap::new();
        selected.insert(source.id().to_string(), source.clone());
        if matches!(reach, PaneInputReach::Synchronized) && source.is_synchronized() {
            for candidate in &panes {
                if candidate.window_id() == source.window_id() && candidate.is_synchronized() {
                    selected
                        .entry(candidate.id().to_string())
                        .or_insert_with(|| candidate.clone());
                }
            }
        }

        let window_panes = panes
            .iter()
            .filter(|candidate| candidate.window_id() == source.window_id())
            .map(|candidate| candidate.id().to_string())
            .collect();

        if let Some(own) = self.protected_pane().await
            && selected.contains_key(own)
        {
            return Err(Self::self_protection(format!(
                "refusing to send input to pane {own}: it matches this MCP server's inherited \
                 caller context, so input there may disrupt or end this conversation. Run the \
                 command in a terminal if that is what you meant."
            )));
        }

        let client_result = self
            .server
            .cmd(
                Command::new("list-clients")
                    .arg("-F")
                    .arg(CLIENT_ATTENTION_FORMAT),
            )
            .await
            .map_err(|error| tmux_error(&error))?;
        if let Some(error) = client_result.refusal_for("list-clients") {
            return Err(tmux_error(&error));
        }
        let attended = parse_attended_panes(client_result.stdout(), &window_panes)
            .map_err(client_attention_error)?;
        let endpoint = self.pane_input_endpoint().await?;

        for (id, candidate) in &selected {
            if run_request::is_reserved(&endpoint, id, lease) {
                return Err(active_run_error(id));
            }
            if attended.contains(id) {
                return Err(bad_input(format!(
                    "pane {id} is visible to an attached terminal client; pane input is reserved for unattended panes"
                )));
            }
            if candidate.is_dead() {
                return Err(bad_input(format!(
                    "pane {id} is dead; pane input requires every configured recipient to be alive"
                )));
            }
            if candidate.is_input_disabled() {
                return Err(bad_input(format!(
                    "pane {id} has input disabled; pane input requires every configured recipient to accept input"
                )));
            }
            if candidate.is_in_mode() {
                return Err(bad_input(format!(
                    "pane {id} is in a tmux mode; wait for the attached client to leave the mode before sending input"
                )));
            }
        }

        Ok(PaneInputPlan {
            target: source,
            configured: selected.into_keys().collect(),
            endpoint,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::parse_attended_panes;

    fn window_panes() -> BTreeSet<String> {
        ["%0", "%1"].into_iter().map(str::to_owned).collect()
    }

    #[test]
    fn terminal_attention_follows_visibility() {
        let panes = window_panes();
        assert_eq!(
            parse_attended_panes(b"0|%1|1\n", &panes).expect("zoomed row"),
            ["%1"].into_iter().map(str::to_owned).collect()
        );
        assert_eq!(
            parse_attended_panes(b"0|%1|0\n", &panes).expect("visible window row"),
            panes
        );
    }

    #[test]
    fn control_and_other_window_clients_are_not_attended() {
        let panes = window_panes();
        assert!(
            parse_attended_panes(b"1|%0|0\n0|%9|0\n", &panes)
                .expect("valid unrelated rows")
                .is_empty()
        );
    }

    #[test]
    fn malformed_client_attention_fails_closed() {
        let panes = window_panes();
        for stdout in [
            b"0|%0|0".as_slice(),
            b"|%0|0\n",
            b"on|%0|0\n",
            b"0||0\n",
            b"0|%01|0\n",
            b"0|%0|\n",
            b"0|%0|on\n",
            b"0|%0\n",
            b"0|%0|0|tail\n",
            b"0|%0|0\n\n",
            b"0|\xff|0\n",
        ] {
            assert!(
                parse_attended_panes(stdout, &panes).is_err(),
                "malformed row was accepted: {stdout:?}"
            );
        }
    }
}
