use libtmux::plan::{
    Attribution as PlanAttribution, Op as PlanOp, OperationValue as CoreOperationValue,
    Outcome as PlanOutcome, PaneTarget, Plan, Planner, Safety as PlanSafety, WindowTarget,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ErrorData;
use rmcp::{tool, tool_router};

use crate::{
    Asking, PlanFailure, PlanFailureKind, PlanOperationReport, PlanRun, RunPlanArgs, TmuxTools,
    plan_evidence, plan_evidence_limit, project_plan_value,
};

use super::error::tmux_error;

#[tool_router(router = plan_router, vis = "pub(super)")]
impl TmuxTools {
    /// Refuse a plan holding an operation this server's tier does not offer.
    ///
    /// Named in the error, because "refused" without saying which step is a
    /// message an agent cannot act on.
    fn admit_plan(&self, plan: &Plan) -> Result<(), ErrorData> {
        for (index, op) in plan.steps().iter().enumerate() {
            if !self.safety.admits_operation(op.safety()) {
                return Err(ErrorData::invalid_params(
                    format!(
                        "step {index} is {}, which this server does not offer at the {} \
                         safety tier: {}",
                        match op.safety() {
                            PlanSafety::ReadOnly => "read-only",
                            PlanSafety::Mutating => "mutating",
                            PlanSafety::Destructive => "destructive",
                        },
                        self.safety.name(),
                        op.name(),
                    ),
                    None,
                ));
            }
        }
        Ok(())
    }

    /// Refuse a plan that would destroy the pane carrying this conversation.
    async fn protect_plan_caller(&self, plan: &Plan) -> Result<(), ErrorData> {
        let Some(own) = self.own_pane().await.map(ToOwned::to_owned) else {
            return Ok(());
        };

        for op in plan.steps() {
            match op {
                PlanOp::KillPane(op) => {
                    if let PaneTarget::Id(id) = op.target()
                        && id.as_ref() == own
                    {
                        return Err(Self::self_harm("pane", &own));
                    }
                }
                PlanOp::KillWindow(op) => {
                    let WindowTarget::Id(id) = op.target() else {
                        continue;
                    };
                    let window = self.find_window(id.as_ref()).await?;
                    let panes = window.panes().await.map_err(|error| tmux_error(&error))?;
                    if panes.iter().any(|pane| pane.id().to_string() == own) {
                        return Err(Self::self_harm("window", &own));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Run several tmux operations described as one plan.
    ///
    /// A plan is data, so an agent describes the whole build in one call
    /// rather than one call per step, and every object a later step addresses
    /// is a reference to the step that makes it -- no ids to look up in
    /// between.
    #[tool(
        description = "Run several tmux operations described as one plan, instead of one \
                       call per step. Objects a later step uses are references to the step \
                       that creates them, so no ids are looked up in between. Every \
                       operation is checked against this server's safety tier before \
                       anything runs.",
        title = "Run Plan",
        annotations(
            read_only_hint = false,
            // Builder::build rewrites these hints to the configured tier.
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn run_plan(
        &self,
        Parameters(RunPlanArgs { plan, grouping }): Parameters<RunPlanArgs>,
        asking: Asking,
    ) -> Result<Json<PlanRun>, ErrorData> {
        let planner = match grouping.as_deref() {
            None | Some("sequential") => Planner::Sequential,
            Some("folding") => Planner::Folding,
            Some("marked") => Planner::Marked,
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("unknown grouping {other:?}; use sequential, folding, or marked"),
                    None,
                ));
            }
        };

        // A tool annotation describes the tool, and a plan is a bag of
        // operations, so the tier is enforced per operation instead. Checked
        // before anything runs: refusing halfway would leave the tmux server
        // in a state the caller did not ask for and cannot see.
        self.admit_plan(&plan)?;
        self.protect_plan_caller(&plan).await?;
        let destructive = plan
            .steps()
            .iter()
            .filter(|op| op.safety() == PlanSafety::Destructive)
            .count();
        if destructive > 0 {
            let noun = if destructive == 1 {
                "operation"
            } else {
                "operations"
            };
            self.permitted(
                &asking,
                &format!("a plan containing {destructive} destructive {noun}"),
            )
            .await?;
        }

        let result = plan
            .run(&self.server, planner)
            .await
            .map_err(|e| tmux_error(&e))?;

        let capture_streams = result
            .operations()
            .iter()
            .filter(|report| matches!(report.value(), Some(CoreOperationValue::CapturedPane(_))))
            .count();
        let failure_streams = result
            .steps()
            .iter()
            .filter(|step| {
                step.outcomes().iter().any(|outcome| !outcome.is_complete())
                    && !step.has_sensitive_input()
                    && !step.stderr().is_empty()
            })
            .count();
        let evidence_limit = plan_evidence_limit(capture_streams + failure_streams);

        let failures = result
            .steps()
            .iter()
            .filter(|step| step.outcomes().iter().any(|outcome| !outcome.is_complete()))
            .map(|step| {
                let withheld = step.has_sensitive_input() && !step.stderr().is_empty();
                let kind = step
                    .refusal()
                    .map_or(PlanFailureKind::Refused, |error| error.kind().into());
                PlanFailure {
                    operations: step.step().indices().to_vec(),
                    attribution: match step.attribution() {
                        PlanAttribution::PerCommand => "per_command",
                        PlanAttribution::Merged => "merged",
                    }
                    .to_owned(),
                    kind,
                    stderr: (!withheld && !step.stderr().is_empty())
                        .then(|| plan_evidence(step.stderr(), evidence_limit)),
                    stderr_bytes: step.stderr().len(),
                    stderr_withheld: withheld,
                }
            })
            .collect();

        Ok(Json(PlanRun {
            operations: result
                .operations()
                .iter()
                .map(|report| PlanOperationReport {
                    index: report.index(),
                    kind: report.kind().name().to_owned(),
                    outcome: match report.outcome() {
                        PlanOutcome::Complete => "complete",
                        PlanOutcome::Failed => "failed",
                        PlanOutcome::Skipped => "skipped",
                        PlanOutcome::Unknown => "unknown",
                    }
                    .to_owned(),
                    attribution: report
                        .attribution()
                        .map(|attribution| match attribution {
                            PlanAttribution::PerCommand => "per_command",
                            PlanAttribution::Merged => "merged",
                        })
                        .map(str::to_owned),
                    value: report
                        .value()
                        .and_then(|value| project_plan_value(value, evidence_limit)),
                })
                .collect(),
            failures,
            dispatches: result.dispatches(),
            complete: result.is_complete(),
        }))
    }
}
