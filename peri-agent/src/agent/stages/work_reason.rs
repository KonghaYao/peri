use std::sync::Arc;

use peri_acp_types::messages::{BaseMessage, ToolCallRequest};
use peri_acp_types::session_resources::work::*;
use peri_acp_types::store::PersistedPayload;
use sha2::{Digest, Sha256};

use super::work_boundary::WorkBoundary;
use super::work_pipeline::{request_checkpoint, WorkSession};
use super::work_recovery::{recover_work, RecoveredStage};
use super::StageContext;
use crate::agent::react::{Reasoning, ToolCall};
use crate::error::AgentError;
use crate::session::tool_catalog::SessionToolCatalogSnapshot;
use crate::tools::{BaseTool, CanonicalToolInvocation};
use peri_acp_types::error::WorkBudgetKind;

pub(crate) async fn prepare(
    ctx: &StageContext,
    messages: &[BaseMessage],
    tools: &[&dyn BaseTool],
) -> anyhow::Result<Option<peri_model::PreparedModelCall>> {
    let Some(session) = ctx.work.ensure(ctx).await? else {
        return Ok(None);
    };
    let prepared = ctx.runtime.llm.prepare_reasoning(messages, tools)?;
    let checkpoint = request_checkpoint(
        &session,
        &serde_json::json!({
            "request": prepared.checkpoint(),
            "authorizationRef": session.admission.admission_id,
            "toolDefinitions": tools.iter().map(|tool| tool.definition()).collect::<Vec<_>>(),
        }),
        ctx.runtime.llm.model_name(),
        session.admission.admission_id.clone(),
    )
    .await?;
    let mut state = ctx.work.state.lock().await;
    let snapshot = session.inspect_head().await?;
    let processing = session
        .processing(
            state
                .work_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("model work missing"))?,
        )
        .await?;
    let target = WorkSession::target(&processing);
    let request_id = uuid::Uuid::now_v7().to_string();
    let command = session.command(WorkAction::BeginReason {
        guard: session.guard(&snapshot)?,
        target: target.clone(),
        request_id: request_id.clone(),
        request: checkpoint,
    });
    let receipt = session.ledger.commit_execution_transition(&command).await?;
    if receipt.stage != Some(WorkStage::ReasonInFlight) {
        if receipt.stage == Some(WorkStage::Blocked) {
            if let Some(error) = budget_exhaustion(
                &processing,
                &snapshot.head.limits,
                WorkBudgetKind::ReasonRequests,
            ) {
                return Err(anyhow::Error::new(error));
            }
        }
        return Err(anyhow::anyhow!("model budget blocked before send"));
    }
    state.request_id = Some(request_id);
    Ok(Some(prepared))
}

pub(super) fn budget_exhaustion(
    processing: &Processing,
    limits: &WorkLimits,
    kind: WorkBudgetKind,
) -> Option<AgentError> {
    let (used, limit) = match kind {
        WorkBudgetKind::ReasonRequests => {
            (processing.budget.reason_requests, limits.reason_requests)
        }
        WorkBudgetKind::Dispatches => (processing.budget.dispatches, limits.dispatches),
        WorkBudgetKind::Recoveries => (processing.budget.recoveries, limits.recoveries),
    };
    (used >= limit).then_some(AgentError::WorkBudgetExhausted {
        budget: kind,
        used,
        limit,
    })
}

pub(super) fn blocked_budget_error(
    processing: &Processing,
    limits: &WorkLimits,
) -> Option<AgentError> {
    if processing.stage != WorkStage::Blocked {
        return None;
    }
    let kind = match processing.blocked_evidence.as_deref()? {
        "reason budget exhausted" => WorkBudgetKind::ReasonRequests,
        "dispatch budget exhausted" => WorkBudgetKind::Dispatches,
        "recovery budget exhausted" => WorkBudgetKind::Recoveries,
        _ => return None,
    };
    budget_exhaustion(processing, limits, kind)
}

async fn bind_intent(
    session: &WorkSession,
    invocation: &CanonicalToolInvocation,
) -> anyhow::Result<InvocationIntent> {
    let call = &invocation.raw_call;
    let metadata = invocation
        .target
        .invocation_target(&session.admission.session_id, session.admission.lifecycle)
        .await
        .map_err(anyhow::Error::msg)?;
    let metadata = match metadata {
        Some(metadata) => metadata,
        None if invocation.target.mcp_server_name().is_some() => {
            return Err(anyhow::anyhow!("MCP target has no trusted owner metadata"))
        }
        None => peri_acp_types::tools::InvocationTargetMetadata {
            owner_identity: format!(
                "peri-local:{}:{}",
                session.admission.instance_id, session.admission.generation_id
            ),
            scope_id: session.admission.session_id.clone(),
            scope_epoch: None,
            authorization_ref: session.admission.admission_id.clone(),
            recovery_locator: format!("best-effort:{}", session.admission.session_id),
        },
    };
    if metadata.owner_identity.is_empty()
        || metadata.scope_id.is_empty()
        || metadata.authorization_ref.is_empty()
        || metadata.recovery_locator.is_empty()
    {
        return Err(anyhow::anyhow!("invocation owner metadata is incomplete"));
    }
    let arguments_json = serde_json::to_string(&call.input)?;
    let effective_arguments_json = serde_json::to_string(&invocation.policy_call.input)?;
    Ok(InvocationIntent {
        invocation_id: uuid::Uuid::now_v7().to_string(),
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        arguments_digest: format!("{:x}", Sha256::digest(arguments_json.as_bytes())),
        arguments: super::work_reads::prepare_evidence(
            session.ledger.resources().as_ref(),
            &session.admission.session_id,
            arguments_json.into_bytes(),
        )
        .await?,
        effective_tool_name: invocation.policy_call.name.clone(),
        effective_arguments_digest: format!(
            "{:x}",
            Sha256::digest(effective_arguments_json.as_bytes())
        ),
        effective_arguments: super::work_reads::prepare_evidence(
            session.ledger.resources().as_ref(),
            &session.admission.session_id,
            effective_arguments_json.into_bytes(),
        )
        .await?,
        owner_identity: metadata.owner_identity,
        scope_id: metadata.scope_id,
        scope_epoch: metadata.scope_epoch,
        authorization_ref: metadata.authorization_ref,
        recovery_locator: metadata.recovery_locator,
    })
}

pub(crate) async fn commit_response(
    ctx: &StageContext,
    reasoning: &mut Reasoning,
    catalog: &Arc<SessionToolCatalogSnapshot>,
) -> anyhow::Result<()> {
    let Some(session) = ctx.work.ensure(ctx).await? else {
        return Ok(());
    };
    let calls = reasoning
        .tool_calls
        .iter()
        .map(|call| ToolCallRequest::new(call.id.clone(), call.name.clone(), call.input.clone()))
        .collect();
    let source = reasoning.source_message.clone().unwrap_or_else(|| {
        if reasoning.tool_calls.is_empty() {
            BaseMessage::ai(
                reasoning
                    .final_answer
                    .clone()
                    .unwrap_or_else(|| reasoning.thought.clone()),
            )
        } else {
            BaseMessage::ai_with_tool_calls(reasoning.thought.clone(), calls)
        }
    });
    let mut state = ctx.work.state.lock().await;
    let all_tools = catalog.tool_map();
    let mut intents = Vec::new();
    let mut invocations = std::collections::HashMap::new();
    for call in &reasoning.tool_calls {
        if call.id.is_empty() || invocations.contains_key(&call.id) {
            return Err(anyhow::anyhow!("model invocation IDs are invalid"));
        }
        let invocation = ctx
            .runtime
            .tool_invocation_resolver
            .resolve(call, &all_tools)
            .map_err(|error| {
                tracing::warn!(
                    tool = %peri_acp_types::session::bounded_error_message(&call.name, 120),
                    "completed model response rejected before tool dispatch; original Reason checkpoint retained"
                );
                error
            })?;
        intents.push(bind_intent(&session, &invocation).await?);
        invocations.insert(call.id.clone(), invocation);
    }
    let snapshot = session.inspect_head().await?;
    let processing = session
        .processing(
            state
                .work_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("response work missing"))?,
        )
        .await?;
    let target = WorkSession::target(&processing);
    let request_id = state
        .request_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("response request identity missing"))?;
    let next_work_id = if !intents.is_empty()
        || reasoning.stop_reason == peri_model::StopReason::MaxTokens
        || reasoning.stream_interruption.is_some()
    {
        Some(target.work_id.clone())
    } else {
        None
    };
    let command = session.command(WorkAction::CommitReasonResponseAndDispatchIntent {
        guard: session.guard(&snapshot)?,
        target,
        request_id,
        response: session
            .prepare_payload(&PersistedPayload::Message(source.clone()))
            .await?,
        dispatch_intents: intents,
        next_work_id: next_work_id.clone(),
    });
    session.ledger.commit_execution_transition(&command).await?;
    if let Some(next_work_id) = next_work_id {
        state.work_id = Some(next_work_id);
    }
    state.invocations = invocations;
    state.request_id = None;
    reasoning.source_message = Some(source);
    Ok(())
}

pub(crate) async fn recover_reasoning(
    ctx: &StageContext,
    catalog: &Arc<SessionToolCatalogSnapshot>,
) -> anyhow::Result<Option<Reasoning>> {
    let Some(session) = ctx.work.ensure(ctx).await? else {
        return Ok(None);
    };
    let mut state = ctx.work.state.lock().await;
    let recovered = recover_work(
        &session,
        state
            .work_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("recovery work missing"))?,
    )
    .await?;
    let RecoveredStage::ActReady {
        response,
        prepared,
        settled,
    } = recovered.stage
    else {
        return Ok(None);
    };
    if !settled.is_empty() {
        return Err(anyhow::anyhow!(
            "partially settled Act requires explicit recovery"
        ));
    }
    let PersistedPayload::Message(source) = response else {
        return Err(anyhow::anyhow!(
            "Act checkpoint is not an assistant message"
        ));
    };
    let tool_calls = source
        .tool_calls()
        .iter()
        .map(|call| ToolCall::new(call.id.clone(), call.name.clone(), call.arguments.clone()))
        .collect::<Vec<_>>();
    let all_tools = catalog.tool_map();
    for call in &tool_calls {
        let invocation = ctx
            .runtime
            .tool_invocation_resolver
            .resolve(call, &all_tools)?;
        let candidate = bind_intent(&session, &invocation).await?;
        let prior = prepared
            .iter()
            .find(|intent| intent.tool_call_id == call.id)
            .ok_or_else(|| anyhow::anyhow!("recovered intent missing"))?;
        if prior.effective_tool_name != candidate.effective_tool_name
            || prior.effective_arguments_digest != candidate.effective_arguments_digest
            || prior.owner_identity != candidate.owner_identity
            || prior.scope_id != candidate.scope_id
            || prior.scope_epoch != candidate.scope_epoch
            || prior.authorization_ref != candidate.authorization_ref
            || prior.recovery_locator != candidate.recovery_locator
        {
            return Err(anyhow::anyhow!(
                "recovered immutable invocation target changed"
            ));
        }
        state.invocations.insert(call.id.clone(), invocation);
    }
    let mut reasoning = Reasoning::with_tools(source.content(), tool_calls);
    reasoning.source_message = Some(source);
    Ok(Some(reasoning))
}

impl WorkBoundary {
    pub(crate) async fn bound_invocation(&self, call_id: &str) -> Option<CanonicalToolInvocation> {
        self.state.lock().await.invocations.get(call_id).cloned()
    }
}

pub(crate) async fn mirror_response(
    ctx: &StageContext,
    message: BaseMessage,
) -> anyhow::Result<bool> {
    let Some(session) = ctx.work.ensure(ctx).await? else {
        return Ok(false);
    };
    let state = ctx.work.state.lock().await;
    let processing = session
        .processing(
            state
                .work_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("response processing missing"))?,
        )
        .await?;
    let response = processing
        .response
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("response checkpoint missing"))?;
    let persisted = session.payload(response).await?;
    let actual = peri_acp_types::store::serialize_persisted_payload(&persisted)?;
    let expected =
        peri_acp_types::store::serialize_persisted_payload(&PersistedPayload::Message(message))?;
    if actual != expected {
        return Err(anyhow::anyhow!("Act projected an uncommitted response"));
    }
    ctx.session
        .transcript
        .write()
        .mirror_committed_payload(persisted);
    Ok(true)
}

pub(crate) async fn block_uncertain_model(ctx: &StageContext) -> anyhow::Result<()> {
    let mut state = ctx.work.state.lock().await;
    let Some(session) = state.session.clone() else {
        return Ok(());
    };
    let Some(request_id) = state.request_id.clone() else {
        return Ok(());
    };
    state.frozen = true;
    let snapshot = session.inspect_head().await?;
    let processing = session
        .processing(
            state
                .work_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("uncertain model work missing"))?,
        )
        .await?;
    let target = WorkSession::target(&processing);
    if processing.stage != WorkStage::ReasonInFlight {
        return Ok(());
    }
    let command = session.command(WorkAction::BlockWork {
        expected_revision: snapshot.head.change_seq,
        target,
        reason: "model request has no durably committed response".into(),
        recovery_condition: format!(
            "reconcile original model request {request_id}; do not regenerate"
        ),
    });
    session.ledger.commit(&command).await?;
    Ok(())
}

#[cfg(test)]
#[path = "work_reason_test.rs"]
mod tests;

#[cfg(test)]
#[path = "work_reason_resolver_test.rs"]
mod resolver_tests;
