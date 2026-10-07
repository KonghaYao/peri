use super::reducer::{bump, work_mut};
use super::*;

/// 终态裁剪：`Settled` / `Abandoned` 的 work 不再保留模型请求正文。正文占
/// 账本体积约 95%，而终态对账不再读取它（恢复路径只对 `ReasonInFlight` /
/// `Blocked` / `ActReady` 用正文）。保留 `request_id` 与 `response` 等身份事实；
/// 既有存量记录不被扫描清理（用户裁决保留现场），因此只在进入终态的动作内调用。
fn trim_terminal_request(work: &mut WorkRecord) {
    work.reason_request = None;
}

fn blocked_budget(
    state: &mut WorkState,
    target: &WorkTarget,
    receipt: &mut WorkReceipt,
    reason: &str,
) -> Result<(), WorkRejection> {
    let work = work_mut(state, target)?;
    work.resume_stage = Some(work.stage);
    work.stage = WorkStage::Blocked;
    work.reason = Some(reason.into());
    work.recovery_condition = Some("explicit budget reset authorization".into());
    bump(work, receipt)
}

pub(super) fn begin_reason(
    state: &mut WorkState,
    target: &WorkTarget,
    request_id: &str,
    request: &ReasonRequest,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    let work = work_mut(state, target)?.clone();
    if work.stage != WorkStage::ReasonReady
        || request_id.is_empty()
        || state
            .works
            .values()
            .any(|prior| prior.request_id.as_deref() == Some(request_id))
    {
        return Err(WorkRejection::InvalidTransition);
    }
    if request.model_ref.is_empty()
        || request.authorization_ref.is_empty()
        || serde_json::from_str::<serde_json::Value>(&request.serialized_request).is_err()
        || request.request_digest
            != format!(
                "{:x}",
                Sha256::digest(request.serialized_request.as_bytes())
            )
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let budget = state
        .budgets
        .get_mut(&work.budget_id)
        .ok_or(WorkRejection::Conflict)?;
    if budget.reason_requests >= state.limits.reason_requests {
        return blocked_budget(state, target, receipt, "reason budget exhausted");
    }
    budget.reason_requests = budget
        .reason_requests
        .checked_add(1)
        .ok_or(WorkRejection::VersionExhausted)?;
    let work = work_mut(state, target)?;
    work.stage = WorkStage::ReasonInFlight;
    work.request_id = Some(request_id.into());
    work.reason_request = Some(request.clone());
    bump(work, receipt)
}

fn successor(
    state: &mut WorkState,
    source: &WorkRecord,
    next_work_id: &str,
    stage: WorkStage,
    invocation_ids: Vec<String>,
) -> Result<(), WorkRejection> {
    if next_work_id.is_empty() || state.works.contains_key(next_work_id) {
        return Err(WorkRejection::Conflict);
    }
    state.works.insert(
        next_work_id.into(),
        WorkRecord {
            work_id: next_work_id.into(),
            revision: 0,
            budget_id: source.budget_id.clone(),
            batch_id: source.batch_id.clone(),
            stage,
            resume_stage: None,
            request_id: None,
            reason_request: None,
            response: None,
            invocation_ids,
            reason: None,
            recovery_condition: None,
        },
    );
    if let Some(binding) = state.work_delegations.get(&source.work_id).cloned() {
        state.work_delegations.insert(next_work_id.into(), binding);
    }
    Ok(())
}

pub(super) fn commit_reason(
    command: &WorkCommand,
    state: &mut WorkState,
    receipt: &mut WorkReceipt,
    projections: &mut Vec<WorkPayload>,
) -> Result<(), WorkRejection> {
    let WorkAction::CommitReasonResponseAndDispatchIntent {
        target,
        request_id,
        response,
        dispatch_intents: intents,
        next_work_id,
        ..
    } = &command.action
    else {
        return Err(WorkRejection::InvalidTransition);
    };
    let next_work_id = next_work_id.as_deref();
    let source = work_mut(state, target)?.clone();
    if source.stage != WorkStage::ReasonInFlight
        || source.request_id.as_deref() != Some(request_id.as_str())
        || response.role != "assistant"
        || response.validate().is_err()
        || (!intents.is_empty() && next_work_id.is_none())
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let payload = deserialize_persisted_payload(&response.serialized)
        .map_err(|_| WorkRejection::InvalidTransition)?;
    let message = payload
        .as_message()
        .ok_or(WorkRejection::InvalidTransition)?;
    let calls = message.tool_calls();
    if calls.len() != intents.len() {
        return Err(WorkRejection::Conflict);
    }
    let mut invocation_ids = Vec::new();
    for intent in intents {
        if invocation_ids.contains(&intent.invocation_id) {
            return Err(WorkRejection::Conflict);
        }
        let call = calls
            .iter()
            .find(|call| call.id == intent.tool_call_id)
            .ok_or(WorkRejection::Conflict)?;
        let arguments: serde_json::Value = serde_json::from_str(&intent.arguments_json)
            .map_err(|_| WorkRejection::InvalidTransition)?;
        if call.name != intent.tool_name || call.arguments != arguments {
            return Err(WorkRejection::Conflict);
        }
        super::bindings::prepare(command, state, intent, next_work_id)?;
        invocation_ids.push(intent.invocation_id.clone());
    }
    let batch = state
        .batches
        .get(&source.batch_id)
        .ok_or(WorkRejection::Conflict)?
        .clone();
    for delivery_id in &batch.processing_delivery_ids {
        let obligation = state
            .obligations
            .get_mut(delivery_id)
            .ok_or(WorkRejection::Conflict)?;
        if !matches!(
            obligation.status,
            ObligationStatus::InProgress | ObligationStatus::Satisfied
        ) {
            return Err(WorkRejection::InvalidTransition);
        }
        obligation.status = ObligationStatus::Satisfied;
        obligation.reason = None;
    }
    projections.push(response.clone());
    if let Some(next_work_id) = next_work_id {
        successor(
            state,
            &source,
            next_work_id,
            if intents.is_empty() {
                WorkStage::ReasonReady
            } else {
                WorkStage::ActReady
            },
            invocation_ids,
        )?;
        state
            .works
            .get_mut(next_work_id)
            .ok_or(WorkRejection::Conflict)?
            .response = Some(response.clone());
    }
    let work = work_mut(state, target)?;
    work.response = Some(response.clone());
    work.stage = WorkStage::Settled;
    trim_terminal_request(work);
    bump(work, receipt)?;
    if let Some(next_work_id) = next_work_id {
        let next = state
            .works
            .get(next_work_id)
            .ok_or(WorkRejection::Conflict)?;
        receipt.work_id = Some(next.work_id.clone());
        receipt.work_revision = Some(next.revision);
        receipt.stage = Some(next.stage);
    }
    Ok(())
}

pub(super) fn begin_dispatch(
    state: &mut WorkState,
    target: &WorkTarget,
    invocation_id: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    let source = work_mut(state, target)?.clone();
    if source.stage != WorkStage::ActReady
        || !source
            .invocation_ids
            .iter()
            .any(|identity| identity == invocation_id)
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let invocation = state
        .invocations
        .get(invocation_id)
        .ok_or(WorkRejection::Conflict)?;
    if invocation.status != InvocationStatus::Prepared
        || invocation.work_id.as_deref() != Some(&source.work_id)
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let budget = state
        .budgets
        .get_mut(&source.budget_id)
        .ok_or(WorkRejection::Conflict)?;
    if budget.dispatches >= state.limits.dispatches {
        return blocked_budget(state, target, receipt, "dispatch budget exhausted");
    }
    budget.dispatches = budget
        .dispatches
        .checked_add(1)
        .ok_or(WorkRejection::VersionExhausted)?;
    state
        .invocations
        .get_mut(invocation_id)
        .ok_or(WorkRejection::Conflict)?
        .status = InvocationStatus::DispatchAccepted;
    bump(work_mut(state, target)?, receipt)
}

pub(super) fn commit_act(
    command: &WorkCommand,
    state: &mut WorkState,
    target: &WorkTarget,
    results: &[InvocationResult],
    next_work_id: Option<&str>,
    receipt: &mut WorkReceipt,
    projections: &mut Vec<WorkPayload>,
) -> Result<(), WorkRejection> {
    let source = work_mut(state, target)?.clone();
    if !matches!(
        source.stage,
        WorkStage::ActReady | WorkStage::Blocked | WorkStage::Abandoned
    ) || source.invocation_ids.is_empty()
        || results.is_empty()
        || (source.stage != WorkStage::ActReady && next_work_id.is_some())
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let mut seen = Vec::new();
    for result in results {
        if !source.invocation_ids.contains(&result.invocation_id)
            || seen.contains(&result.invocation_id)
        {
            return Err(WorkRejection::Conflict);
        }
        seen.push(result.invocation_id.clone());
        let invocation = state
            .invocations
            .get_mut(&result.invocation_id)
            .ok_or(WorkRejection::Conflict)?;
        if invocation.work_id.as_deref() != Some(&source.work_id)
            || invocation.recipient_lifecycle != command.recipient_lifecycle
        {
            return Err(WorkRejection::Conflict);
        }
        let rejected_before_dispatch = invocation.status == InvocationStatus::Prepared
            && matches!(&result.outcome, InvocationOutcome::Cancelled { evidence } if !evidence.is_empty());
        if !rejected_before_dispatch
            && !matches!(
                invocation.status,
                InvocationStatus::DispatchAccepted | InvocationStatus::OutcomeUnknown
            )
        {
            return Err(WorkRejection::InvalidTransition);
        }
        match &result.outcome {
            InvocationOutcome::Completed { result } | InvocationOutcome::Failed { result } => {
                if result.role != "tool" || result.validate().is_err() {
                    return Err(WorkRejection::InvalidTransition);
                }
                let payload = deserialize_persisted_payload(&result.serialized)
                    .map_err(|_| WorkRejection::InvalidTransition)?;
                if !matches!(payload.as_message(), Some(crate::messages::BaseMessage::Tool { tool_call_id, .. }) if tool_call_id == &invocation.intent.tool_call_id)
                {
                    return Err(WorkRejection::Conflict);
                }
            }
            InvocationOutcome::Cancelled { evidence } if evidence.is_empty() => {
                return Err(WorkRejection::InvalidTransition);
            }
            InvocationOutcome::Cancelled { .. } => {}
        }
        invocation.status = InvocationStatus::Settled;
        invocation.outcome = Some(result.outcome.clone());
        invocation.unknown_reason = None;
        if let Some(projection) = invocation
            .settled_projection(&command.session_id)
            .map_err(|_| WorkRejection::InvalidTransition)?
        {
            projections.push(projection);
        }
    }
    let complete = source.invocation_ids.iter().all(|invocation_id| {
        state
            .invocations
            .get(invocation_id)
            .is_some_and(|invocation| invocation.status == InvocationStatus::Settled)
    });
    if !complete && next_work_id.is_some() {
        return Err(WorkRejection::InvalidTransition);
    }
    if complete {
        if let Some(next_work_id) = next_work_id {
            successor(
                state,
                &source,
                next_work_id,
                WorkStage::ReasonReady,
                Vec::new(),
            )?;
        }
    }
    let work = work_mut(state, target)?;
    if complete && source.stage == WorkStage::ActReady {
        work.stage = WorkStage::Settled;
        work.reason = None;
        work.recovery_condition = None;
        trim_terminal_request(work);
    }
    bump(work, receipt)?;
    if complete {
        if let Some(next_work_id) = next_work_id {
            receipt.work_id = Some(next_work_id.into());
            receipt.work_revision = Some(0);
            receipt.stage = Some(WorkStage::ReasonReady);
        }
    }
    Ok(())
}

pub(super) fn block(
    state: &mut WorkState,
    target: &WorkTarget,
    reason: &str,
    recovery_condition: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    if reason.is_empty() || recovery_condition.is_empty() {
        return Err(WorkRejection::InvalidTransition);
    }
    let work = work_mut(state, target)?;
    if matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned) {
        return Err(WorkRejection::InvalidTransition);
    }
    if work.stage != WorkStage::Blocked {
        work.resume_stage = Some(work.stage);
    }
    work.stage = WorkStage::Blocked;
    work.reason = Some(reason.into());
    work.recovery_condition = Some(recovery_condition.into());
    let batch_id = work.batch_id.clone();
    bump(work, receipt)?;
    for obligation in state.obligations.values_mut().filter(|obligation| {
        obligation.work_id.as_deref() == Some(&batch_id)
            && obligation.status == ObligationStatus::InProgress
    }) {
        obligation.status = ObligationStatus::Blocked;
        obligation.reason = Some(reason.into());
    }
    Ok(())
}

pub(super) fn unknown(
    state: &mut WorkState,
    target: &WorkTarget,
    invocation_id: &str,
    reason: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    let work = work_mut(state, target)?.clone();
    if reason.is_empty() {
        return Err(WorkRejection::InvalidTransition);
    }
    if !work
        .invocation_ids
        .iter()
        .any(|identity| identity == invocation_id)
    {
        return Err(WorkRejection::Conflict);
    }
    let invocation = state
        .invocations
        .get_mut(invocation_id)
        .ok_or(WorkRejection::Conflict)?;
    if invocation.status != InvocationStatus::DispatchAccepted
        || invocation.work_id.as_deref() != Some(&work.work_id)
    {
        return Err(WorkRejection::InvalidTransition);
    }
    invocation.status = InvocationStatus::OutcomeUnknown;
    invocation.unknown_reason = Some(reason.into());
    if work.stage == WorkStage::Abandoned {
        return bump(work_mut(state, target)?, receipt);
    }
    block(
        state,
        target,
        reason,
        "query original owner invocation or explicit resolution",
        receipt,
    )
}

pub(super) fn resume(
    state: &mut WorkState,
    target: &WorkTarget,
    evidence: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    let source = work_mut(state, target)?.clone();
    if source.stage != WorkStage::Blocked
        || evidence.is_empty()
        || source.invocation_ids.iter().any(|invocation_id| {
            state
                .invocations
                .get(invocation_id)
                .is_some_and(|invocation| {
                    matches!(
                        invocation.status,
                        InvocationStatus::DispatchAccepted | InvocationStatus::OutcomeUnknown
                    )
                })
        })
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let resume_stage = source
        .resume_stage
        .ok_or(WorkRejection::InvalidTransition)?;
    if resume_stage == WorkStage::ReasonInFlight {
        return Err(WorkRejection::InvalidTransition);
    }
    let budget = state
        .budgets
        .get_mut(&source.budget_id)
        .ok_or(WorkRejection::Conflict)?;
    if budget.recoveries >= state.limits.recoveries {
        return blocked_budget(state, target, receipt, "recovery budget exhausted");
    }
    budget.recoveries = budget
        .recoveries
        .checked_add(1)
        .ok_or(WorkRejection::VersionExhausted)?;
    let work = work_mut(state, target)?;
    work.stage = resume_stage;
    work.resume_stage = None;
    work.reason = None;
    work.recovery_condition = None;
    let batch_id = work.batch_id.clone();
    bump(work, receipt)?;
    for obligation in state.obligations.values_mut().filter(|obligation| {
        obligation.work_id.as_deref() == Some(&batch_id)
            && obligation.status == ObligationStatus::Blocked
    }) {
        obligation.status = ObligationStatus::InProgress;
        obligation.reason = None;
    }
    Ok(())
}

pub(super) fn abandon(
    state: &mut WorkState,
    target: &WorkTarget,
    reason: &str,
    authorization_ref: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    if reason.is_empty() || authorization_ref.is_empty() {
        return Err(WorkRejection::InvalidTransition);
    }
    let work = work_mut(state, target)?;
    if matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned) {
        return Err(WorkRejection::InvalidTransition);
    }
    work.stage = WorkStage::Abandoned;
    work.reason = Some(format!("{reason}; authorization={authorization_ref}"));
    trim_terminal_request(work);
    let batch_id = work.batch_id.clone();
    bump(work, receipt)?;
    for obligation in state.obligations.values_mut().filter(|obligation| {
        obligation.work_id.as_deref() == Some(&batch_id)
            && obligation.status != ObligationStatus::Satisfied
    }) {
        obligation.status = ObligationStatus::Abandoned;
        obligation.reason = Some(reason.into());
    }
    Ok(())
}

pub(super) fn settle(
    state: &mut WorkState,
    target: &WorkTarget,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    let source = work_mut(state, target)?.clone();
    if source.stage != WorkStage::ActReady
        || !source.invocation_ids.iter().all(|invocation_id| {
            state
                .invocations
                .get(invocation_id)
                .is_some_and(|invocation| invocation.status == InvocationStatus::Settled)
        })
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let work = work_mut(state, target)?;
    work.stage = WorkStage::Settled;
    trim_terminal_request(work);
    bump(work, receipt)
}

#[cfg(test)]
#[path = "processing_test.rs"]
mod tests;
