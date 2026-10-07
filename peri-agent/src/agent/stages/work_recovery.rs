use peri_acp_types::session_resources::work::*;
use peri_acp_types::store::PersistedPayload;

use super::work_pipeline::WorkSession;

#[derive(Debug)]
pub(crate) enum RecoveredStage {
    ReasonReady,
    ReasonUncertain {
        request_id: String,
        request: PayloadRef,
    },
    ActReady {
        response: PersistedPayload,
        prepared: Vec<InvocationIntent>,
        settled: Vec<InvocationResult>,
    },
    ReconcileInvocations {
        invocation_ids: Vec<String>,
    },
    Blocked {
        reason: Option<String>,
        recovery_condition: Option<String>,
    },
    Finished,
}

#[derive(Debug)]
pub(crate) struct RecoveredWork {
    pub(crate) target: WorkTarget,
    pub(crate) phase_sequence: u64,
    pub(crate) deliveries: Vec<Delivery>,
    pub(crate) stage: RecoveredStage,
}

pub(crate) async fn recover_work(
    session: &WorkSession,
    work_id: &str,
) -> anyhow::Result<RecoveredWork> {
    let head = session.inspect_head().await?;
    if head.head.legacy_unknown != 0 {
        return Err(anyhow::anyhow!(
            "legacy responsibility requires explicit recovery evidence"
        ));
    }
    let processing = session.processing(work_id).await?;
    let deliveries = session.deliveries(work_id).await?;
    for delivery in &deliveries {
        if delivery.processing_id.as_deref() != Some(work_id)
            || delivery.recipient_lifecycle != processing.recipient_lifecycle
            || (delivery.participates_in_reason && delivery.projection.is_none())
        {
            return Err(anyhow::anyhow!(
                "durable processing delivery membership is inconsistent"
            ));
        }
    }
    let stage = match processing.stage {
        WorkStage::ReasonReady => RecoveredStage::ReasonReady,
        WorkStage::ReasonInFlight => RecoveredStage::ReasonUncertain {
            request_id: processing
                .request_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("uncertain request identity missing"))?,
            request: processing
                .request
                .clone()
                .ok_or_else(|| anyhow::anyhow!("uncertain request evidence missing"))?,
        },
        WorkStage::ActReady => {
            let response = session
                .payload(
                    processing
                        .response
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Act response checkpoint missing"))?,
                )
                .await?;
            let mut prepared = Vec::new();
            let mut settled = Vec::new();
            let mut uncertain = Vec::new();
            for effect in session.effects(&processing).await? {
                if effect.processing_id.as_deref() != Some(work_id)
                    || effect.recipient_lifecycle != session.admission.lifecycle
                {
                    return Err(anyhow::anyhow!(
                        "effect responsibility differs from processing"
                    ));
                }
                match effect.status {
                    InvocationStatus::Prepared => prepared.push(effect.intent),
                    InvocationStatus::Settled => settled.push(InvocationResult {
                        invocation_id: effect.invocation_id,
                        expected_effect_revision: effect.revision,
                        outcome: effect
                            .outcome
                            .ok_or_else(|| anyhow::anyhow!("settled outcome missing"))?,
                    }),
                    InvocationStatus::DispatchAccepted | InvocationStatus::OutcomeUnknown => {
                        uncertain.push(effect.invocation_id);
                    }
                }
            }
            if uncertain.is_empty() {
                RecoveredStage::ActReady {
                    response,
                    prepared,
                    settled,
                }
            } else {
                RecoveredStage::ReconcileInvocations {
                    invocation_ids: uncertain,
                }
            }
        }
        WorkStage::Blocked => RecoveredStage::Blocked {
            reason: processing.blocked_evidence.clone(),
            recovery_condition: processing.recovery_condition.clone(),
        },
        WorkStage::Settled | WorkStage::Abandoned => RecoveredStage::Finished,
    };
    Ok(RecoveredWork {
        target: WorkSession::target(&processing),
        phase_sequence: processing.phase_sequence,
        deliveries,
        stage,
    })
}

#[cfg(test)]
#[path = "work_recovery_test.rs"]
mod tests;
