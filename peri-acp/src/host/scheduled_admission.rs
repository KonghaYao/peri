use std::sync::Arc;

use peri_acp_types::interaction::UserInteractionBroker;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::session_resources::work::{
    Delivery, EvidenceQuery, WorkAction, WorkAdmission, WorkCommand, WorkDecision, WorkGuard,
    WorkInspection, WorkPage, WorkQuery, WorkReceipt, WorkResolution, WorkSelector, WorkStage,
    WorkTarget,
};
use peri_acp_types::session_resources::{ControlStatus, SessionResources};
use peri_acp_types::store::{PersistedPayload, deserialize_persisted_payload};
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
) -> anyhow::Result<WorkInspection> {
    let snapshot =
        super::work_query::inspect(resources, &admission.session_id, WorkSelector::Availability)
            .await?;
    let admitted = super::work_query::inspect(
        resources,
        &admission.session_id,
        WorkSelector::Admission {
            admission_id: admission.admission_id.clone(),
        },
    )
    .await?;
    let availability = super::work_query::availability(&snapshot)?;
    if snapshot.control.lifecycle != admission.lifecycle
        || snapshot.control.control_generation != admission.control_generation
        || snapshot.control.status != ControlStatus::Active
        || snapshot.control.attempt.as_ref() != Some(&admission.execution)
        || admitted.control != snapshot.control
        || availability.blocked
        || availability.pending
        || !super::work_query::admission(&admitted, &admission.admission_id)?.is_some_and(
            |record| {
                record.admission == *admission
                    && !record.entering_mutation_id.is_empty()
                    && record.leaving_evidence_id.is_none()
            },
        )
    {
        anyhow::bail!("scheduled admission no longer owns the exact active SDK execution");
    }
    Ok(snapshot)
}

async fn scheduled_triggers(
    resources: &dyn SessionResources,
    snapshot: &WorkInspection,
    admission: &WorkAdmission,
) -> anyhow::Result<Vec<ScheduledTrigger>> {
    let availability = super::work_query::availability(snapshot)?;
    let candidate = availability
        .candidates
        .iter()
        .find(|candidate| {
            candidate.work_id == admission.work_id
                && candidate.work_revision == admission.work_revision
        })
        .ok_or_else(|| anyhow::anyhow!("scheduled admission candidate is no longer exact"))?;
    if candidate.stage != WorkStage::ReasonReady {
        return Ok(Vec::new());
    }
    let processing = super::work_query::inspect(
        resources,
        &admission.session_id,
        WorkSelector::Processing {
            processing_id: admission.work_id.clone(),
        },
    )
    .await?;
    let mut triggers = Vec::new();
    if super::work_query::processing(&processing, &admission.work_id)?.is_some() {
        let mut query = WorkQuery::new(
            &admission.session_id,
            WorkSelector::ProcessingDeliveries {
                processing_id: admission.work_id.clone(),
            },
        );
        loop {
            let page = resources.inspect_work(&query).await?;
            if page.control != snapshot.control {
                anyhow::bail!("scheduled recipient changed")
            }
            let WorkPage::Deliveries(deliveries) = page.page else {
                anyhow::bail!("scheduled delivery inspection returned conflicting page");
            };
            for delivery in deliveries
                .into_iter()
                .filter(|delivery| delivery.participates_in_reason)
            {
                if let Some(trigger) = scheduled_trigger(resources, admission, &delivery).await? {
                    triggers.push(trigger)
                }
            }
            let Some(cursor) = page.next_cursor else {
                break;
            };
            query.cursor = Some(cursor);
        }
    } else {
        for delivery_id in &candidate.delivery_ids {
            let page = super::work_query::inspect(
                resources,
                &admission.session_id,
                WorkSelector::Delivery {
                    delivery_id: delivery_id.clone(),
                },
            )
            .await?;
            let WorkPage::Deliveries(deliveries) = &page.page else {
                anyhow::bail!("scheduled delivery page missing")
            };
            let delivery = deliveries
                .first()
                .ok_or_else(|| anyhow::anyhow!("scheduled candidate delivery missing"))?;
            if let Some(trigger) = scheduled_trigger(resources, admission, delivery).await? {
                triggers.push(trigger)
            }
        }
    }
    Ok(triggers)
}

async fn scheduled_trigger(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
    delivery: &Delivery,
) -> anyhow::Result<Option<ScheduledTrigger>> {
    if delivery.recipient_lifecycle != admission.lifecycle {
        anyhow::bail!("scheduled recipient changed")
    }
    let event = &delivery.publication.event;
    if event.content.role != "system_reminder" {
        return Ok(None);
    }
    let evidence = resources
        .read_evidence(&EvidenceQuery {
            session_id: admission.session_id.clone(),
            reference: event.content.content.clone(),
        })
        .await?;
    evidence.validate()?;
    if evidence.reference != event.content.content {
        anyhow::bail!("scheduled evidence reference conflicts")
    }
    let serialized = String::from_utf8(evidence.bytes)?;
    let PersistedPayload::SystemReminder { reminder, .. } =
        deserialize_persisted_payload(&serialized)?
    else {
        return Ok(None);
    };
    let reminder = reminder.as_reminder();
    if reminder.source.0 != "cron" || reminder.kind != "triggered" {
        return Ok(None);
    }
    if event.producer_namespace != "peri-agent.inbox" {
        anyhow::bail!("scheduled producer identity is not trusted")
    }
    let task_id = reminder
        .metadata
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("scheduled task identity missing"))?;
    let prompt = reminder
        .metadata
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("scheduled original prompt missing"))?;
    Ok(Some(ScheduledTrigger {
        delivery_id: delivery.delivery_id.clone(),
        task_id: task_id.into(),
        prompt: prompt.into(),
    }))
}

async fn approve_work(
    resources: &dyn SessionResources,
    admission: &WorkAdmission,
    permission_mode: &SharedPermissionMode,
    broker: &Arc<dyn UserInteractionBroker>,
) -> anyhow::Result<()> {
    let snapshot = snapshot_for(resources, admission).await?;
    for trigger in scheduled_triggers(resources, &snapshot, admission).await? {
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
        let page = super::work_query::inspect(
            resources,
            &admission.session_id,
            WorkSelector::Delivery {
                delivery_id: delivery_id.clone(),
            },
        )
        .await?;
        let WorkPage::Deliveries(deliveries) = &page.page else {
            anyhow::bail!("rejected scheduled delivery page missing")
        };
        let delivery = deliveries
            .first()
            .ok_or_else(|| anyhow::anyhow!("rejected scheduled delivery missing"))?;
        let processing = super::work_query::inspect(
            resources,
            &admission.session_id,
            WorkSelector::Processing {
                processing_id: admission.work_id.clone(),
            },
        )
        .await?;
        let action = if let Some(processing_id) = &delivery.processing_id {
            let work = super::work_query::processing(&processing, &admission.work_id)?
                .filter(|work| &work.processing_id == processing_id)
                .ok_or_else(|| {
                    anyhow::anyhow!("rejected scheduled work is not the exact SDK work")
                })?;
            if work.delivery_count as usize != denied_delivery_ids.len() {
                commit_original(resources, &WorkCommand {
                    session_id: admission.session_id.clone(), recipient_lifecycle: admission.lifecycle,
                    mutation_id: format!("scheduled-rejection-block:{}:{delivery_id}", admission.admission_id),
                    action: WorkAction::BlockWork {
                        expected_revision: processing.head.change_seq,
                        target: WorkTarget { work_id: work.processing_id.clone(), expected_work_revision: work.revision },
                        reason: "restored mixed scheduled batch approval rejected".into(),
                        recovery_condition: "selective durable resolution must preserve unrelated claimed deliveries".into(),
                    },
                }).await?;
                anyhow::bail!(
                    "restored mixed scheduled batch is durably blocked without abandoning unrelated deliveries"
                );
            }
            WorkAction::AbandonWork {
                expected_revision: processing.head.change_seq,
                expected_control_generation: admission.control_generation,
                target: WorkTarget {
                    work_id: work.processing_id.clone(),
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
                    expected_revision: snapshot.head.change_seq,
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
