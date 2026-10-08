use super::*;
use crate::session::{ExecutionBinding, MessageActivation, MessageDisposition, MessageRequirement};
use crate::session_resources::ControlStatus;

/// 调度要求与激活语义是否自洽。
///
/// 队列调度的 `Required` 与 reminder delivery 的 `Required` 分别建模：
/// `model_visible` 只是"这条内容是否进入模型上下文"的受众事实，**不**参与本判定——
/// 合法 Tui-only 的 Required 提醒（也必须经持久化投递）不得被当成非法转换拒绝，
/// 也不得靠扩大受众绕过校验。真正矛盾的是"必须处理"却声明 `Passive`：
/// 这样的投递等不到任何处理机会，工具结果/提醒会永久挂起。
pub(super) fn disposition_is_consistent(policy: &MessagePolicy) -> bool {
    !(policy.requirement == MessageRequirement::Required
        && policy.activation == MessageActivation::Passive)
}

pub(super) fn publish(
    command: &WorkCommand,
    control: &ControlState,
    state: &mut WorkState,
    delivery: &PublishDelivery,
    receipt: &mut WorkReceipt,
    events: &mut Vec<WorkEvent>,
) -> Result<(), WorkRejection> {
    if delivery.delivery_id.is_empty()
        || delivery.event.producer_namespace.is_empty()
        || delivery.event.event_id.is_empty()
        || delivery.event.event_kind.is_empty()
        || delivery.event.content.validate().is_err()
    {
        return Err(WorkRejection::InvalidTransition);
    }
    if !disposition_is_consistent(&delivery.policy) {
        return Err(WorkRejection::InvalidTransition);
    }
    for (delivery_id, prior) in &state.deliveries {
        let same_event = prior.publication.event.producer_namespace
            == delivery.event.producer_namespace
            && prior.publication.event.event_id == delivery.event.event_id;
        if same_event && prior.publication.event != delivery.event {
            return Err(WorkRejection::Conflict);
        }
        let same_key = same_event
            && prior.recipient_lifecycle == command.recipient_lifecycle
            && prior.publication.purpose == delivery.purpose;
        if delivery_id == &delivery.delivery_id || same_key {
            if !same_key || prior.publication != *delivery {
                return Err(WorkRejection::Conflict);
            }
            receipt.delivery_id = Some(delivery_id.clone());
            receipt.admission_sequence = Some(prior.admission_sequence);
            return Ok(());
        }
    }
    let closing = matches!(
        control.status,
        ControlStatus::Closing | ControlStatus::Closed
    );
    let old_lifecycle = command.recipient_lifecycle != control.lifecycle;
    if (old_lifecycle || closing)
        && !matches!(command.action, WorkAction::PublishTaskSettlement { .. })
    {
        return Err(WorkRejection::StaleLifecycle);
    }
    if (old_lifecycle || closing)
        && !matches!(
            delivery.purpose,
            DeliveryPurpose::TaskTerminal | DeliveryPurpose::Settlement
        )
    {
        return Err(WorkRejection::StaleLifecycle);
    }
    let required = delivery.policy.requirement == MessageRequirement::Required;
    let pending: Vec<_> = state
        .deliveries
        .iter()
        .filter(|(delivery_id, record)| {
            let same_lane =
                (record.publication.policy.requirement == MessageRequirement::Required) == required;
            let live = state
                .obligations
                .get(*delivery_id)
                .map(|obligation| {
                    matches!(
                        obligation.status,
                        ObligationStatus::Pending
                            | ObligationStatus::InProgress
                            | ObligationStatus::Blocked
                    )
                })
                .unwrap_or(!record.projected && record.disposition.is_none());
            same_lane && live
        })
        .collect();
    let (count_limit, bytes_limit) = if required {
        (
            state.limits.required_deliveries,
            state.limits.required_bytes,
        )
    } else {
        (
            state.limits.optional_deliveries,
            state.limits.optional_bytes,
        )
    };
    let bytes = pending
        .iter()
        .try_fold(
            delivery.event.content.serialized.len() as u64,
            |total, (_, record)| {
                total.checked_add(record.publication.event.content.serialized.len() as u64)
            },
        )
        .ok_or(WorkRejection::VersionExhausted)?;
    if pending.len() as u64 >= count_limit || bytes > bytes_limit {
        return Err(WorkRejection::Capacity);
    }
    let sequence = state.next_admission_sequence;
    state.next_admission_sequence = sequence
        .checked_add(1)
        .ok_or(WorkRejection::VersionExhausted)?;
    let ended_execution = match &delivery.policy.activation {
        MessageActivation::ContinueCurrentRun(bound) => {
            control.attempt.as_ref().is_none_or(|current| {
                current.turn_id != bound.turn_id || current.attempt_id != bound.attempt_id
            })
        }
        _ => false,
    };
    let disposition = if old_lifecycle || closing {
        Some("closed lifecycle settlement".to_owned())
    } else if ended_execution {
        Some("original execution ended".to_owned())
    } else {
        None
    };
    let projection_key = serde_json::to_vec(&(
        command.session_id.as_str(),
        command.recipient_lifecycle,
        delivery.event.producer_namespace.as_str(),
        delivery.event.event_id.as_str(),
        delivery.purpose,
    ))
    .map_err(|_| WorkRejection::InvalidTransition)?;
    let hash = Sha256::digest(projection_key);
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let projection = if delivery.purpose == DeliveryPurpose::UserInput {
        delivery.event.content.clone()
    } else {
        delivery
            .event
            .content
            .with_message_id(MessageId::from(uuid::Uuid::from_bytes(bytes)))
            .map_err(|_| WorkRejection::InvalidTransition)?
    };
    state.deliveries.insert(
        delivery.delivery_id.clone(),
        DeliveryRecord {
            recipient_lifecycle: command.recipient_lifecycle,
            publication: delivery.clone(),
            projection,
            admission_sequence: sequence,
            projected: false,
            projection_version: 0,
            batch_id: None,
            disposition,
        },
    );
    if required {
        let (status, reason) = if old_lifecycle || closing || ended_execution {
            (
                ObligationStatus::Suppressed,
                Some("settlement only; no executable obligation".into()),
            )
        } else if control.status != ControlStatus::Active {
            (
                ObligationStatus::Blocked,
                Some("session control blocks processing".into()),
            )
        } else {
            (ObligationStatus::Pending, None)
        };
        state.obligations.insert(
            delivery.delivery_id.clone(),
            RequiredObligation {
                delivery_id: delivery.delivery_id.clone(),
                status,
                work_id: None,
                reason,
            },
        );
    }
    events.push(delivery.event.clone());
    receipt.delivery_id = Some(delivery.delivery_id.clone());
    receipt.admission_sequence = Some(sequence);
    Ok(())
}

pub(super) fn claim(
    command: &WorkCommand,
    state: &mut WorkState,
    guard: &WorkGuard,
    batch_id: &str,
    delivery_ids: &[String],
    receipt: &mut WorkReceipt,
    projections: &mut Vec<WorkPayload>,
) -> Result<(), WorkRejection> {
    if batch_id.is_empty()
        || delivery_ids.is_empty()
        || delivery_ids.len() as u64 > state.limits.max_batch_size
        || state.batches.contains_key(batch_id)
        || state.works.contains_key(batch_id)
    {
        return Err(WorkRejection::InvalidTransition);
    }
    if state.works.values().any(|work| {
        !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
            && state.work_lifecycle(&work.work_id) == Some(command.recipient_lifecycle)
    }) {
        return Err(WorkRejection::InvalidTransition);
    }
    if state.has_unknown_live_work_lifecycle() {
        return Err(WorkRejection::LegacyUnknown);
    }
    let expected: Vec<_> = state
        .claimable_deliveries(command.recipient_lifecycle)
        .into_iter()
        .take(delivery_ids.len())
        .collect();
    if expected != delivery_ids {
        return Err(WorkRejection::Conflict);
    }
    let execution = ExecutionBinding {
        turn_id: guard.execution.turn_id,
        attempt_id: guard.execution.attempt_id.clone(),
    };
    let mut processing_delivery_ids = Vec::new();
    let mut projection_versions = BTreeMap::new();
    for delivery_id in delivery_ids {
        let delivery = state
            .deliveries
            .get_mut(delivery_id)
            .ok_or(WorkRejection::Conflict)?;
        let disposition = delivery.publication.policy.disposition(&execution);
        if disposition == MessageDisposition::Suppressed {
            delivery.disposition = Some("original execution is no longer current".into());
            if let Some(obligation) = state.obligations.get_mut(delivery_id) {
                obligation.status = ObligationStatus::Suppressed;
                obligation.reason = delivery.disposition.clone();
            }
        } else {
            if !delivery.projected && delivery.publication.policy.model_visible {
                projections.push(delivery.projection.clone());
            }
            delivery.projected = true;
            delivery.projection_version = 1;
            projection_versions.insert(delivery_id.clone(), delivery.projection_version);
            if disposition == MessageDisposition::Process {
                processing_delivery_ids.push(delivery_id.clone());
                let obligation = state
                    .obligations
                    .get_mut(delivery_id)
                    .ok_or(WorkRejection::Conflict)?;
                obligation.status = ObligationStatus::InProgress;
                obligation.work_id = Some(batch_id.into());
                obligation.reason = None;
            }
        }
        delivery.batch_id = Some(batch_id.into());
    }
    state.batches.insert(
        batch_id.into(),
        ProcessingBatch {
            batch_id: batch_id.into(),
            delivery_ids: delivery_ids.to_vec(),
            processing_delivery_ids: processing_delivery_ids.clone(),
            projection_versions,
            execution: guard.execution.clone(),
            recipient_lifecycle: command.recipient_lifecycle,
        },
    );
    receipt.batch_id = Some(batch_id.into());
    if !processing_delivery_ids.is_empty() {
        state.budgets.insert(batch_id.into(), WorkBudget::default());
        state.works.insert(
            batch_id.into(),
            WorkRecord {
                work_id: batch_id.into(),
                revision: 0,
                budget_id: batch_id.into(),
                batch_id: batch_id.into(),
                stage: WorkStage::ReasonReady,
                resume_stage: None,
                request_id: None,
                reason_request: None,
                response: None,
                invocation_ids: Vec::new(),
                reason: None,
                recovery_condition: None,
            },
        );
        receipt.work_id = Some(batch_id.into());
        receipt.work_revision = Some(0);
        receipt.stage = Some(WorkStage::ReasonReady);
    }
    Ok(())
}

pub(super) fn abandon_delivery(
    command: &WorkCommand,
    state: &mut WorkState,
    delivery_id: &str,
    reason: &str,
    evidence: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    if delivery_id.is_empty() || reason.is_empty() || evidence.is_empty() {
        return Err(WorkRejection::InvalidTransition);
    }
    let delivery = state
        .deliveries
        .get_mut(delivery_id)
        .ok_or(WorkRejection::Conflict)?;
    if delivery.recipient_lifecycle != command.recipient_lifecycle
        || delivery.batch_id.is_some()
        || delivery.projected
        || delivery.disposition.is_some()
    {
        return Err(WorkRejection::InvalidTransition);
    }
    let obligation = state
        .obligations
        .get_mut(delivery_id)
        .ok_or(WorkRejection::Conflict)?;
    if obligation.status != ObligationStatus::Pending {
        return Err(WorkRejection::InvalidTransition);
    }
    obligation.status = ObligationStatus::Abandoned;
    obligation.reason = Some(format!("{reason}; evidence={evidence}"));
    delivery.disposition = obligation.reason.clone();
    receipt.delivery_id = Some(delivery_id.into());
    receipt.admission_sequence = Some(delivery.admission_sequence);
    Ok(())
}

pub(super) fn withdraw(
    state: &mut WorkState,
    delivery_id: &str,
    authorization_ref: &str,
    receipt: &mut WorkReceipt,
) -> Result<(), WorkRejection> {
    if authorization_ref.is_empty() {
        return Err(WorkRejection::InvalidTransition);
    }
    let delivery = state
        .deliveries
        .get_mut(delivery_id)
        .ok_or(WorkRejection::InvalidTransition)?;
    if delivery.batch_id.is_some()
        || delivery.publication.purpose != DeliveryPurpose::UserInput
        || delivery.disposition.is_some()
    {
        return Err(WorkRejection::InvalidTransition);
    }
    delivery.disposition = Some(format!("withdrawn: {authorization_ref}"));
    if let Some(obligation) = state.obligations.get_mut(delivery_id) {
        obligation.status = ObligationStatus::Abandoned;
        obligation.reason = delivery.disposition.clone();
    }
    receipt.delivery_id = Some(delivery_id.into());
    Ok(())
}
