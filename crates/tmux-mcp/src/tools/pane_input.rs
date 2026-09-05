use std::collections::BTreeMap;

use rmcp::model::ErrorData;

use crate::TmuxTools;

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
}

pub(crate) struct PaneInputPlan {
    pub(crate) target: libtmux::Pane,
    pub(crate) configured: Vec<String>,
}

impl TmuxTools {
    pub(crate) async fn preflight_pane_input(
        &self,
        pane: &str,
        reach: PaneInputReach,
        missing: MissingSource,
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
            })?;

        let mut selected = BTreeMap::new();
        selected.insert(source.id().to_string(), source.clone());
        if matches!(reach, PaneInputReach::Synchronized) && source.is_synchronized() {
            for candidate in panes {
                if candidate.window_id() == source.window_id() && candidate.is_synchronized() {
                    selected
                        .entry(candidate.id().to_string())
                        .or_insert(candidate);
                }
            }
        }

        for (id, candidate) in &selected {
            if candidate.is_dead() {
                return Err(bad_input(format!(
                    "pane {id} is dead; pane input requires every configured recipient to be alive"
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
        })
    }
}
