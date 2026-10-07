use peri_acp_types::execution_admission::*;
use peri_acp_types::session_resources::{work::*, ControlAttempt, SessionResources};
use std::sync::Arc;

pub(crate) struct FixtureAdmission(pub(crate) Arc<dyn SessionResources>);

#[async_trait::async_trait]
impl ExecutionAdmissionPort for FixtureAdmission {
    async fn admit(
        &self,
        request: AdmissionRequest,
    ) -> Result<AdmissionOutcome, ExecutionAdmissionError> {
        let snapshot = request.snapshot;
        if snapshot.control.attempt.is_some() {
            return Ok(AdmissionOutcome::Busy);
        }
        let Some(candidate) = snapshot.candidates.first() else {
            return Ok(AdmissionOutcome::Blocked {
                reason: "fixture has no actionable durable work".into(),
            });
        };
        Ok(AdmissionOutcome::Admitted {
            admission: WorkAdmission {
                session_id: snapshot.session_id,
                admission_id: request.request_id,
                instance_id: "fixture-sdk-instance".into(),
                generation_id: "fixture-sdk-generation".into(),
                lifecycle: snapshot.control.lifecycle,
                control_generation: snapshot.control.control_generation,
                work_id: candidate.work_id.clone(),
                work_revision: candidate.work_revision,
                execution: ControlAttempt {
                    turn_id: peri_acp_types::session::TurnId::new(),
                    attempt_id: peri_acp_types::identity::AttemptId::new(),
                },
            },
        })
    }

    async fn entered(
        &self,
        request: EntryRequest,
    ) -> Result<EntryOutcome, ExecutionAdmissionError> {
        let snapshot = self
            .0
            .load_session_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ExecutionAdmissionError::Protocol(error.to_string()))?;
        let registration = &snapshot.state.admissions[&request.admission.admission_id];
        assert_eq!(registration.admission, request.admission);
        assert_eq!(
            registration.entering_receipt.as_ref().unwrap().mutation_id,
            request.entry_evidence_id
        );
        assert_eq!(
            snapshot.control.attempt.as_ref(),
            Some(&request.admission.execution)
        );
        Ok(EntryOutcome::Applied {
            receipt: EntryReceipt {
                admission: request.admission,
                entry_evidence_id: request.entry_evidence_id,
            },
        })
    }

    async fn settle(
        &self,
        request: SettlementRequest,
    ) -> Result<SettlementOutcome, ExecutionAdmissionError> {
        assert!(request.proof.validates(&request.admission));
        let snapshot = self
            .0
            .load_session_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| ExecutionAdmissionError::Protocol(error.to_string()))?;
        assert!(snapshot.control.attempt.is_none());
        let registration = &snapshot.state.admissions[&request.admission.admission_id];
        assert_eq!(registration.admission, request.admission);
        if let Some(receipt) = &registration.settled_receipt {
            assert_eq!(
                registration.evidence_id.as_deref(),
                Some(request.proof.evidence_id())
            );
            assert_eq!(receipt.decision, WorkDecision::Accepted);
        } else {
            let receipt = self
                .0
                .apply_work_mutation(&WorkCommand {
                    session_id: request.admission.session_id.clone(),
                    recipient_lifecycle: request.admission.lifecycle,
                    mutation_id: format!("sdk-finish:{}", request.admission.admission_id),
                    action: WorkAction::FinishAdmission {
                        admission: request.admission.clone(),
                        evidence_id: request.proof.evidence_id().into(),
                    },
                })
                .await
                .map_err(|error| ExecutionAdmissionError::Protocol(error.to_string()))?;
            assert_eq!(receipt.decision, WorkDecision::Accepted);
        }
        Ok(SettlementOutcome::Applied {
            receipt: SettlementReceipt {
                admission: request.admission,
                evidence_id: request.proof.evidence_id().into(),
            },
        })
    }
}
