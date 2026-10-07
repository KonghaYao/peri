use std::sync::Arc;

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;

use super::work_ledger::WorkCommitError;
use super::work_pipeline::WorkSession;
use super::StageContext;
use crate::agent::react::{ToolCall, ToolResult};

pub(crate) struct DispatchBinding {
    pub(crate) lifecycle: u64,
    pub(crate) intent: Arc<InvocationIntent>,
    pub(crate) target: WorkTarget,
    pub(crate) resources: Arc<dyn SessionResources>,
}

pub(crate) async fn begin(
    ctx: &StageContext,
    call: &ToolCall,
) -> anyhow::Result<Option<DispatchBinding>> {
    let Some(session) = ctx.work.ensure(ctx).await? else {
        return Ok(None);
    };
    let mut state = ctx.work.state.lock().await;
    let snapshot = session.snapshot().await?;
    let target = WorkSession::target(
        &snapshot,
        state
            .work_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("dispatch work missing"))?,
    )?;
    let record = snapshot
        .state
        .invocations
        .values()
        .find(|record| {
            record.work_id.as_deref() == Some(&target.work_id)
                && record.intent.tool_call_id == call.id
        })
        .ok_or_else(|| anyhow::anyhow!("dispatch has no committed invocation intent"))?;
    if record.status != InvocationStatus::Prepared
        || record.intent.effective_tool_name != call.name
        || serde_json::from_str::<serde_json::Value>(&record.intent.effective_arguments_json)?
            != call.input
    {
        return Err(anyhow::anyhow!(
            "dispatch differs from its immutable committed intent"
        ));
    }
    let intent = Arc::new(record.intent.clone());
    let command = session.command(WorkAction::BeginDispatch {
        guard: session.guard(&snapshot)?,
        target: target.clone(),
        invocation_id: intent.invocation_id.clone(),
    })?;
    let budget_error = super::work_reason::budget_exhaustion(
        &snapshot.state,
        &target.work_id,
        peri_acp_types::error::WorkBudgetKind::Dispatches,
    );
    drop(snapshot);
    let receipt = match session.ledger.commit_execution_transition(&command).await {
        Ok(receipt) => receipt,
        Err(error) => {
            state.frozen = true;
            return Err(error.into());
        }
    };
    if receipt.stage != Some(WorkStage::ActReady) {
        state.frozen = true;
        if receipt.stage == Some(WorkStage::Blocked) {
            if let Some(error) = budget_error {
                return Err(anyhow::Error::new(error));
            }
        }
        return Err(anyhow::anyhow!("dispatch budget blocked before effect"));
    }
    let target = WorkTarget {
        work_id: target.work_id,
        expected_work_revision: receipt
            .work_revision
            .ok_or_else(|| anyhow::anyhow!("dispatch receipt work revision missing"))?,
    };
    Ok(Some(DispatchBinding {
        lifecycle: session.admission.lifecycle,
        intent,
        target,
        resources: session.ledger.resources(),
    }))
}

pub(crate) async fn unknown(ctx: &StageContext, call_id: &str, reason: &str) -> anyhow::Result<()> {
    let session = {
        let state = ctx.work.state.lock().await;
        state.session.clone()
    };
    let Some(session) = session else {
        return Ok(());
    };
    let mut state = ctx.work.state.lock().await;
    let snapshot = session.snapshot().await?;
    let target = WorkSession::target(
        &snapshot,
        state
            .work_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("uncertain work missing"))?,
    )?;
    let invocation = snapshot.state.invocations.values().find(|record| {
        record.work_id.as_deref() == Some(&target.work_id) && record.intent.tool_call_id == call_id
    });
    let Some(invocation) =
        invocation.filter(|record| record.status == InvocationStatus::DispatchAccepted)
    else {
        return Ok(());
    };
    let command = session.command(WorkAction::OutcomeUnknown {
        expected_revision: snapshot.state.revision,
        target,
        invocation_id: invocation.intent.invocation_id.clone(),
        reason: reason.into(),
    })?;
    state.frozen = true;
    drop(snapshot);
    session.ledger.commit(&command).await?;
    Ok(())
}

pub(crate) async fn commit_results(
    ctx: &StageContext,
    results: &[(ToolCall, ToolResult)],
) -> anyhow::Result<Option<Vec<BaseMessage>>> {
    if results.is_empty() {
        ctx.work.ensure(ctx).await?;
    }
    let Some(session) = ctx.work.settlement_session(ctx).await? else {
        return Ok(None);
    };
    let mut state = ctx.work.state.lock().await;
    let snapshot = session.snapshot().await?;
    let target = WorkSession::target(
        &snapshot,
        state
            .work_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Act work missing"))?,
    )?;
    let source = snapshot
        .state
        .works
        .get(&target.work_id)
        .ok_or_else(|| anyhow::anyhow!("Act work missing"))?;
    let may_advance = !state.frozen
        && source.stage == WorkStage::ActReady
        && snapshot.control.status == peri_acp_types::session_resources::ControlStatus::Active
        && snapshot.control.control_generation == session.admission.control_generation
        && snapshot.control.attempt.as_ref() == Some(&session.admission.execution);
    let mut outcomes = Vec::new();
    let mut invocation_ids = Vec::new();
    for (_, result) in results {
        let invocation = snapshot
            .state
            .invocations
            .values()
            .find(|record| {
                record.work_id.as_deref() == Some(&target.work_id)
                    && record.intent.tool_call_id == result.tool_call_id
            })
            .ok_or_else(|| anyhow::anyhow!("tool result has no durable invocation association"))?;
        let stopped_before_effect = source.stage == WorkStage::Abandoned
            || (source.stage == WorkStage::Blocked
                && source.reason.as_deref() == Some("dispatch budget exhausted"));
        if invocation.status == InvocationStatus::Prepared
            && result.effective_error_code
                != Some(crate::tools::EffectiveToolErrorCode::UserRejected)
            && !may_advance
            && !stopped_before_effect
        {
            continue;
        }
        let message = BaseMessage::tool_result_with_execution_and_failure(
            &result.tool_call_id,
            result.output.as_str(),
            result.is_error,
            result.execution.clone(),
            result.subagent_failure.clone(),
        );
        let payload = WorkPayload::from_payload(&PersistedPayload::Message(message.clone()))?;
        let outcome = if invocation.status == InvocationStatus::Prepared {
            let cancellation = if result.effective_error_code
                == Some(crate::tools::EffectiveToolErrorCode::UserRejected)
            {
                "user-rejected"
            } else if stopped_before_effect {
                "processing-stopped-before-dispatch"
            } else {
                return Err(anyhow::anyhow!(
                    "undispatched invocation has no trusted rejection evidence"
                ));
            };
            InvocationOutcome::Cancelled {
                evidence: format!(
                    "before-effect:{cancellation}:{}",
                    invocation.intent.invocation_id
                ),
            }
        } else if result.is_error {
            InvocationOutcome::Failed { result: payload }
        } else {
            InvocationOutcome::Completed { result: payload }
        };
        invocation_ids.push(invocation.intent.invocation_id.clone());
        outcomes.push(InvocationResult {
            invocation_id: invocation.intent.invocation_id.clone(),
            outcome,
        });
    }
    let complete = source.invocation_ids.iter().all(|identity| {
        invocation_ids.contains(identity)
            || snapshot
                .state
                .invocations
                .get(identity)
                .is_some_and(|invocation| invocation.status == InvocationStatus::Settled)
    });
    let mut next_work_id = (may_advance && complete).then(|| uuid::Uuid::now_v7().to_string());
    if outcomes.is_empty() {
        state.frozen = true;
        return Ok(Some(Vec::new()));
    }
    let mut command = session.command(WorkAction::CommitAct {
        guard: session.guard(&snapshot)?,
        target: target.clone(),
        results: outcomes.clone(),
        next_work_id: next_work_id.clone(),
    })?;
    let mut expected_stage = if complete && source.stage == WorkStage::ActReady {
        WorkStage::Settled
    } else {
        source.stage
    };
    drop(snapshot);
    let mut committed_receipt = None;
    for attempt in 0..8 {
        match session.ledger.commit(&command).await {
            Ok(receipt) => {
                committed_receipt = Some(receipt);
                break;
            }
            Err(WorkCommitError::Rejected { receipt })
                if attempt < 7
                    && matches!(
                        receipt.decision,
                        WorkDecision::Rejected {
                            reason: WorkRejection::StaleRevision | WorkRejection::StaleWorkRevision
                        }
                    ) =>
            {
                let refreshed = session.snapshot().await?;
                let refreshed_target = WorkSession::target(&refreshed, &target.work_id)?;
                let refreshed_work = &refreshed.state.works[&target.work_id];
                let WorkAction::CommitAct {
                    guard,
                    target: prior_target,
                    ..
                } = &command.action
                else {
                    unreachable!();
                };
                let unchanged_execution_work = next_work_id.is_some()
                    && refreshed.control.lifecycle == session.admission.lifecycle
                    && refreshed.control.control_generation == guard.expected_control_generation
                    && refreshed.control.attempt.as_ref() == Some(&guard.execution)
                    && refreshed.control.status
                        == peri_acp_types::session_resources::ControlStatus::Active
                    && refreshed_target.expected_work_revision
                        == prior_target.expected_work_revision
                    && refreshed_work.stage == WorkStage::ActReady;
                if !unchanged_execution_work {
                    state.frozen = true;
                    next_work_id = None;
                }
                let complete = refreshed_work.invocation_ids.iter().all(|identity| {
                    invocation_ids.contains(identity)
                        || refreshed
                            .state
                            .invocations
                            .get(identity)
                            .is_some_and(|invocation| {
                                invocation.status == InvocationStatus::Settled
                            })
                });
                expected_stage = if complete && refreshed_work.stage == WorkStage::ActReady {
                    WorkStage::Settled
                } else {
                    refreshed_work.stage
                };
                command = session.command(WorkAction::CommitAct {
                    guard: session.guard(&refreshed)?,
                    target: refreshed_target,
                    results: outcomes.clone(),
                    next_work_id: next_work_id.clone(),
                })?;
            }
            Err(error) => {
                state.frozen = true;
                return Err(error.into());
            }
        }
    }
    match committed_receipt {
        Some(receipt)
            if match &next_work_id {
                Some(next) => {
                    receipt.stage == Some(WorkStage::ReasonReady)
                        && receipt.work_id.as_deref() == Some(next)
                }
                None => {
                    receipt.work_id.as_deref() == Some(&target.work_id)
                        && receipt.stage == Some(expected_stage)
                }
            } => {}
        _ => {
            state.frozen = true;
            return Err(anyhow::anyhow!(
                "Act receipt does not confirm the exact successor"
            ));
        }
    }
    if let Some(next_work_id) = next_work_id {
        state.work_id = Some(next_work_id);
        state.invocations.clear();
    } else {
        state.frozen = true;
    }
    let committed = session.snapshot().await?;
    let mut projections = Vec::new();
    for invocation_id in invocation_ids {
        let invocation = committed
            .state
            .invocations
            .get(&invocation_id)
            .ok_or_else(|| anyhow::anyhow!("committed invocation result missing"))?;
        let payload = invocation
            .settled_projection(&session.admission.session_id)?
            .ok_or_else(|| {
                anyhow::anyhow!("settled invocation has no canonical result projection")
            })?;
        let persisted = super::work_boundary::persisted_projection(&payload)?;
        let PersistedPayload::Message(message) = persisted else {
            return Err(anyhow::anyhow!("settled tool result is not a message"));
        };
        projections.push(message);
    }
    Ok(Some(projections))
}

#[cfg(test)]
#[path = "work_dispatch_test.rs"]
mod tests;
