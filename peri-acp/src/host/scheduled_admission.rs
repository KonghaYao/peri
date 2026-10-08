use std::sync::Arc;

use peri_acp_types::interaction::UserInteractionBroker;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::session_resources::work::{
    WorkAction, WorkAdmission, WorkCommand, WorkDecision, WorkGuard, WorkQuery, WorkReceipt,
    WorkResolution, WorkSnapshot, WorkStage, WorkTarget,
};
use peri_acp_types::session_resources::{ControlStatus, SessionResources};
use peri_acp_types::store::{deserialize_persisted_payload, PersistedPayload};
use peri_agent::agent::stages::SdkRunStartedFn;

#[derive(Debug, PartialEq, Eq)]
struct ScheduledTrigger {
    delivery_id: String,
    task_id: String,
    prompt: String,
}

pub(crate) fn after_run_started(
    started: SdkRunStartedFn,
    resources: Arc<dyn SessionResources>,
    permission_mode: Arc<SharedPermissionMode>,
    broker: Arc<dyn UserInteractionBroker>,
) -> SdkRunStartedFn {
    Arc::new(move |admission| {
        let started = Arc::clone(&started);
        let resources = Arc::clone(&resources);
        let permission_mode = Arc::clone(&permission_mode);
        let broker = Arc::clone(&broker);
        Box::pin(async move {
            started(admission.clone()).await?;
            approve_work(resources.as_ref(), &admission, &permission_mode, &broker)
                .await
                .map_err(|error| error.to_string())
        })
    })
}

async fn snapshot_for(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
) -> anyhow::Result<WorkSnapshot> {
    let snapshot = resources
        .load_session_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            limit: 1,
        })
        .await?;
    if snapshot.control.lifecycle != admission.lifecycle
        || snapshot.control.control_generation != admission.control_generation
        || snapshot.control.status != ControlStatus::Active
        || snapshot.control.attempt.as_ref() != Some(&admission.execution)
        || snapshot.blocked
        || !snapshot.pending_commands.is_empty()
        || !snapshot
            .state
            .admissions
            .get(&admission.admission_id)
            .is_some_and(|record| {
                record.admission == *admission
                    && record.entering_receipt.is_some()
                    && record.settled_receipt.is_none()
            })
    {
        anyhow::bail!("scheduled admission no longer owns the exact active SDK execution");
    }
    Ok(snapshot)
}

fn scheduled_triggers(
    snapshot: &WorkSnapshot,
    admission: &WorkAdmission,
) -> anyhow::Result<Vec<ScheduledTrigger>> {
    let candidate = snapshot.candidates.iter().find(|candidate| {
        candidate.work_id == admission.work_id && candidate.work_revision == admission.work_revision
    });
    let Some(candidate) = candidate else {
        anyhow::bail!("scheduled admission candidate is no longer exact");
    };
    if candidate.stage != WorkStage::ReasonReady {
        return Ok(Vec::new());
    }
    let delivery_ids = if let Some(batch_id) = &candidate.batch_id {
        let batch = snapshot
            .state
            .batches
            .get(batch_id)
            .filter(|batch| batch.recipient_lifecycle == admission.lifecycle)
            .ok_or_else(|| anyhow::anyhow!("scheduled restored batch recipient is not exact"))?;
        &batch.processing_delivery_ids
    } else {
        &candidate.delivery_ids
    };
    let mut triggers = Vec::new();
    for delivery_id in delivery_ids {
        let delivery = snapshot
            .state
            .deliveries
            .get(delivery_id)
            .ok_or_else(|| anyhow::anyhow!("scheduled candidate delivery is missing"))?;
        if delivery.recipient_lifecycle != admission.lifecycle {
            anyhow::bail!("scheduled candidate recipient changed");
        }
        let payload =
            deserialize_persisted_payload(&delivery.publication.event.content.serialized)?;
        let PersistedPayload::SystemReminder { reminder, .. } = payload else {
            continue;
        };
        let reminder = reminder.as_reminder();
        if reminder.source.0 != "cron" || reminder.kind != "triggered" {
            continue;
        }
        if delivery.publication.event.producer_namespace != "peri-agent.inbox" {
            anyhow::bail!("scheduled trigger producer identity is not trusted");
        }
        let task_id = reminder
            .metadata
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("scheduled task identity missing"))?;
        // 原始指令由正文的唯一编解码权威还原（metadata 只保留身份与长度摘要）。
        let prompt = peri_acp_types::cron::cron_trigger_prompt(reminder).ok_or_else(|| {
            anyhow::anyhow!("scheduled task instruction is not recoverable from its body")
        })?;
        triggers.push(ScheduledTrigger {
            delivery_id: delivery_id.clone(),
            task_id: task_id.into(),
            prompt: prompt.clone(),
        });
    }
    Ok(triggers)
}

async fn approve_work(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
    permission_mode: &SharedPermissionMode,
    broker: &Arc<dyn UserInteractionBroker>,
) -> anyhow::Result<()> {
    let snapshot = snapshot_for(resources, admission).await?;
    for trigger in scheduled_triggers(&snapshot, admission)? {
        if !super::prompt::approve_scheduled_trigger(
            permission_mode,
            Some(broker),
            &trigger.task_id,
            &trigger.prompt,
        )
        .await
        {
            abandon_work(resources, admission, &[trigger.delivery_id]).await?;
            anyhow::bail!(
                "scheduled trigger rejected; exact admitted deliveries were explicitly abandoned"
            );
        }
        snapshot_for(resources, admission).await?;
    }
    Ok(())
}

async fn commit_original(
    resources: &dyn SessionResources,
    command: &WorkCommand,
) -> anyhow::Result<WorkReceipt> {
    let receipt = match resources.apply_work_mutation(command).await {
        Ok(receipt) => receipt,
        Err(_) => match resources.resolve_work_mutation(command).await? {
            WorkResolution::Applied { receipt } => receipt,
            WorkResolution::Unknown | WorkResolution::NotApplied => {
                anyhow::bail!("scheduled decision mutation remains unconfirmed");
            }
        },
    };
    if receipt.session_id != command.session_id
        || receipt.mutation_id != command.mutation_id
        || receipt.decision != WorkDecision::Accepted
    {
        anyhow::bail!("scheduled decision receipt is not an exact accepted mutation");
    }
    Ok(receipt)
}

async fn abandon_work(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
    denied_delivery_ids: &[String],
) -> anyhow::Result<()> {
    for delivery_id in denied_delivery_ids {
        let snapshot = snapshot_for(resources, admission).await?;
        let delivery = snapshot
            .state
            .deliveries
            .get(delivery_id)
            .ok_or_else(|| anyhow::anyhow!("rejected scheduled delivery missing"))?;
        let action = if let Some(batch_id) = &delivery.batch_id {
            let batch = snapshot
                .state
                .batches
                .get(batch_id)
                .ok_or_else(|| anyhow::anyhow!("restored scheduled batch missing"))?;
            let work = snapshot
                .state
                .works
                .get(&admission.work_id)
                .filter(|work| work.batch_id == *batch_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("rejected scheduled work is not the exact SDK work")
                })?;
            if batch.delivery_ids != denied_delivery_ids {
                commit_original(resources, &WorkCommand {
                    session_id: admission.session_id.clone(), recipient_lifecycle: admission.lifecycle,
                    mutation_id: format!("scheduled-rejection-block:{}:{delivery_id}", admission.admission_id),
                    action: WorkAction::BlockWork {
                        expected_revision: snapshot.state.revision,
                        target: WorkTarget { work_id: work.work_id.clone(), expected_work_revision: work.revision },
                        reason: "restored mixed scheduled batch approval rejected".into(),
                        recovery_condition: "selective durable resolution must preserve unrelated claimed deliveries".into(),
                    },
                }).await?;
                anyhow::bail!(
                    "restored mixed scheduled batch is durably blocked without abandoning unrelated deliveries"
                );
            }
            WorkAction::AbandonWork {
                expected_revision: snapshot.state.revision,
                expected_control_generation: admission.control_generation,
                target: WorkTarget {
                    work_id: work.work_id.clone(),
                    expected_work_revision: work.revision,
                },
                reason: "scheduled trigger approval rejected".into(),
                authorization_ref: format!(
                    "scheduled-trigger-decision:{}:{delivery_id}",
                    admission.admission_id
                ),
            }
        } else {
            WorkAction::AbandonDelivery {
                guard: WorkGuard {
                    expected_revision: snapshot.state.revision,
                    expected_control_generation: admission.control_generation,
                    execution: admission.execution.clone(),
                },
                delivery_id: delivery_id.clone(),
                reason: "scheduled trigger approval rejected".into(),
                evidence: format!(
                    "scheduled-trigger-decision:{}:{delivery_id}",
                    admission.admission_id
                ),
            }
        };
        commit_original(
            resources,
            &WorkCommand {
                session_id: admission.session_id.clone(),
                recipient_lifecycle: admission.lifecycle,
                mutation_id: format!(
                    "scheduled-rejection:{}:{delivery_id}",
                    admission.admission_id
                ),
                action,
            },
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "scheduled_admission_test.rs"]
mod tests;
