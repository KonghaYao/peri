use super::work_pipeline::WorkSession;
use super::StageContext;
use crate::agent::react::{ToolCall, ToolResult};
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;
use std::sync::Arc;

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
    let processing = session
        .processing(
            state
                .work_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("dispatch processing missing"))?,
        )
        .await?;
    let snapshot = session.inspect_head().await?;
    let effect = session
        .effects(&processing)
        .await?
        .into_iter()
        .find(|effect| effect.intent.tool_call_id == call.id)
        .ok_or_else(|| anyhow::anyhow!("dispatch has no committed effect"))?;
    let effective_arguments = session.evidence(&effect.intent.effective_arguments).await?;
    if effect.status != InvocationStatus::Prepared
        || effect.intent.effective_tool_name != call.name
        || serde_json::from_slice::<serde_json::Value>(&effective_arguments)? != call.input
    {
        return Err(anyhow::anyhow!(
            "dispatch differs from immutable committed intent"
        ));
    }
    let target = WorkSession::target(&processing);
    let intent = Arc::new(effect.intent);
    let command = session.command(WorkAction::BeginDispatch {
        guard: session.guard(&snapshot)?,
        target: target.clone(),
        invocation_id: effect.invocation_id,
        expected_effect_revision: effect.revision,
    });
    let receipt = match session.ledger.commit_execution_transition(&command).await {
        Ok(receipt) => receipt,
        Err(error) => {
            state.frozen = true;
            return Err(error.into());
        }
    };
    if receipt.stage != Some(WorkStage::ActReady) {
        state.frozen = true;
        if let Some(error) = super::work_reason::budget_exhaustion(
            &processing,
            &snapshot.head.limits,
            peri_acp_types::error::WorkBudgetKind::Dispatches,
        ) {
            return Err(error.into());
        }
        return Err(anyhow::anyhow!("dispatch budget blocked before effect"));
    }
    Ok(Some(DispatchBinding {
        lifecycle: session.admission.lifecycle,
        intent,
        target,
        resources: session.ledger.resources(),
    }))
}

pub(crate) async fn unknown(ctx: &StageContext, call_id: &str, reason: &str) -> anyhow::Result<()> {
    let mut state = ctx.work.state.lock().await;
    let Some(session) = state.session.clone() else {
        return Ok(());
    };
    let processing = session
        .processing(
            state
                .work_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("uncertain processing missing"))?,
        )
        .await?;
    let effect = session
        .effects(&processing)
        .await?
        .into_iter()
        .find(|effect| {
            effect.intent.tool_call_id == call_id
                && effect.status == InvocationStatus::DispatchAccepted
        });
    let Some(effect) = effect else { return Ok(()) };
    let head = session.inspect_head().await?;
    let command = session.command(WorkAction::OutcomeUnknown {
        expected_revision: head.head.change_seq,
        target: WorkSession::target(&processing),
        invocation_id: effect.invocation_id,
        expected_effect_revision: effect.revision,
        reason: reason.into(),
    });
    state.frozen = true;
    session.ledger.commit(&command).await?;
    Ok(())
}

pub(crate) async fn commit_results(
    ctx: &StageContext,
    results: &[(ToolCall, ToolResult)],
) -> anyhow::Result<Option<Vec<BaseMessage>>> {
    if results.is_empty() { ctx.work.ensure(ctx).await?; }
    let Some(session) = ctx.work.settlement_session(ctx).await? else {
        return Ok(None);
    };
    let mut state = ctx.work.state.lock().await;
    let processing_id = state
        .work_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Act processing missing"))?;
    let mut projections = Vec::new();
    for (_, result) in results {
        let processing = session.processing(&processing_id).await?;
        let head = session.inspect_head().await?;
        let effects = session.effects(&processing).await?;
        let effect = effects
            .iter()
            .find(|effect| effect.intent.tool_call_id == result.tool_call_id)
            .ok_or_else(|| anyhow::anyhow!("tool result has no durable effect association"))?;
        if effect.status == InvocationStatus::Settled {
            if let Some(
                InvocationOutcome::Completed { result }
                | InvocationOutcome::Failed { result }
                | InvocationOutcome::Cancelled { result, .. },
            ) = &effect.outcome
            {
                if let PersistedPayload::Message(message) = session.payload(result).await? {
                    projections.push(message);
                }
            }
            continue;
        }
        let stopped_before_effect = processing.stage == WorkStage::Abandoned
            || (processing.stage == WorkStage::Blocked
                && processing.blocked_evidence.as_deref() == Some("dispatch budget exhausted"));
        let may_advance = !state.frozen
            && processing.stage == WorkStage::ActReady
            && head.control.status == peri_acp_types::session_resources::ControlStatus::Active
            && head.control.control_generation == session.admission.control_generation
            && head.control.attempt.as_ref() == Some(&session.admission.execution);
        let message = BaseMessage::tool_result_with_execution_and_failure(
            &result.tool_call_id,
            result.output.as_str(),
            result.is_error,
            result.execution.clone(),
            result.subagent_failure.clone(),
        );
        let outcome = if effect.status == InvocationStatus::Prepared {
            let cancellation = if result.effective_error_code
                == Some(crate::tools::EffectiveToolErrorCode::UserRejected)
            {
                "user-rejected"
            } else if stopped_before_effect {
                "processing-stopped-before-dispatch"
            } else {
                return Err(anyhow::anyhow!(
                    "undispatched effect has no trusted rejection evidence"
                ));
            };
            InvocationOutcome::Cancelled {
                evidence: format!("before-effect:{cancellation}:{}", effect.invocation_id),
                result: session
                    .prepare_payload(&PersistedPayload::Message(message))
                    .await?,
            }
        } else {
            let payload = session
                .prepare_payload(&PersistedPayload::Message(message))
                .await?;
            if result.is_error {
                InvocationOutcome::Failed { result: payload }
            } else {
                InvocationOutcome::Completed { result: payload }
            }
        };
        let final_effect = processing.remaining_effects == 1;
        let command = session.command(WorkAction::CommitAct {
            guard: session.guard(&head)?,
            target: WorkSession::target(&processing),
            results: vec![InvocationResult {
                invocation_id: effect.invocation_id.clone(),
                expected_effect_revision: effect.revision,
                outcome,
            }],
            next_work_id: (may_advance && final_effect).then(|| processing_id.clone()),
        });
        if let Err(error) = session.ledger.commit(&command).await {
            state.frozen = true;
            return Err(error.into());
        }
        let inspection = session
            .ledger
            .inspect(&WorkQuery::new(
                &session.admission.session_id,
                WorkSelector::Effect {
                    invocation_id: effect.invocation_id.clone(),
                },
            ))
            .await?;
        let WorkPage::Effects(records) = inspection.page else {
            return Err(anyhow::anyhow!(
                "settled effect query returned a different page"
            ));
        };
        let committed = records
            .first()
            .filter(|record| record.status == InvocationStatus::Settled)
            .ok_or_else(|| anyhow::anyhow!("effect terminal transition remains unconfirmed"))?;
        if let Some(
            InvocationOutcome::Completed { result }
            | InvocationOutcome::Failed { result }
            | InvocationOutcome::Cancelled { result, .. },
        ) = &committed.outcome
        {
            if let PersistedPayload::Message(message) = session.payload(result).await? {
                projections.push(message);
            }
        }
    }
    let processing = session.processing(&processing_id).await?;
    if processing.stage == WorkStage::ReasonReady {
        state.invocations.clear();
    } else if matches!(
        processing.stage,
        WorkStage::Blocked | WorkStage::Abandoned | WorkStage::Settled
    ) {
        state.frozen = true;
    }
    Ok(Some(projections))
}

#[cfg(test)]
#[path = "work_dispatch_test.rs"]
mod tests;
