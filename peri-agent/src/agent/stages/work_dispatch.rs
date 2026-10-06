use std::sync::Arc;

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::work::*;
use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::store::PersistedPayload;

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
    });
    let receipt = match session.ledger.commit(&command).await {
        Ok(receipt) => receipt,
        Err(error) => {
            state.frozen = true;
            return Err(error.into());
        }
    };
    if receipt.stage != Some(WorkStage::ActReady) {
        state.frozen = true;
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
    });
    state.frozen = true;
    session.ledger.commit(&command).await?;
    Ok(())
}

pub(crate) async fn commit_results(
    ctx: &StageContext,
    results: &[(ToolCall, ToolResult)],
) -> anyhow::Result<Option<Vec<BaseMessage>>> {
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
            .ok_or_else(|| anyhow::anyhow!("Act work missing"))?,
    )?;
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
        let message = BaseMessage::tool_result_with_execution_and_failure(
            &result.tool_call_id,
            result.output.as_str(),
            result.is_error,
            result.execution.clone(),
            result.subagent_failure.clone(),
        );
        let payload = WorkPayload::from_payload(&PersistedPayload::Message(message.clone()))?;
        let outcome = if invocation.status == InvocationStatus::Prepared {
            if result.effective_error_code
                != Some(crate::tools::EffectiveToolErrorCode::UserRejected)
            {
                return Err(anyhow::anyhow!(
                    "undispatched invocation has no trusted rejection evidence"
                ));
            }
            InvocationOutcome::Cancelled {
                evidence: format!(
                    "before-effect:user-rejected:{}",
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
    let next_work_id = uuid::Uuid::now_v7().to_string();
    let command = session.command(WorkAction::CommitAct {
        guard: session.guard(&snapshot)?,
        target,
        results: outcomes,
        next_work_id: Some(next_work_id.clone()),
    });
    match session.ledger.commit(&command).await {
        Ok(receipt)
            if receipt.stage == Some(WorkStage::ReasonReady)
                && receipt.work_id.as_deref() == Some(&next_work_id) => {}
        Ok(_) => {
            state.frozen = true;
            return Err(anyhow::anyhow!(
                "Act receipt does not confirm the exact successor"
            ));
        }
        Err(error) => {
            state.frozen = true;
            return Err(error.into());
        }
    }
    state.work_id = Some(next_work_id);
    state.invocations.clear();
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
