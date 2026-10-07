use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, ExecutionAdmissionError, ExecutionAdmissionPort,
    SettlementOutcome, SettlementRequest, ADMIT_METHOD, SETTLE_METHOD,
};

use crate::transport::RequestTransport;
use peri_acp_types::execution_admission::{EntryOutcome, EntryRequest, ENTERED_METHOD};

pub const EXECUTION_PROTOCOL_VERSION: u32 = 2;

pub struct ReverseExecutionAdmission {
    transport: Arc<dyn RequestTransport>,
}

impl ReverseExecutionAdmission {
    pub fn new(transport: Arc<dyn RequestTransport>) -> Self {
        Self { transport }
    }
}

#[async_trait]
impl ExecutionAdmissionPort for ReverseExecutionAdmission {
    async fn entered(
        &self,
        request: EntryRequest,
    ) -> Result<EntryOutcome, ExecutionAdmissionError> {
        if request.entry_evidence_id.is_empty() {
            return Err(ExecutionAdmissionError::Protocol(
                "durable entry evidence required".into(),
            ));
        }
        let params = serde_json::to_value(&request)
            .map_err(|_| ExecutionAdmissionError::Protocol("entry request encoding".into()))?;
        let response = match self.transport.send_request(ENTERED_METHOD, params).await {
            Ok(response) => response,
            Err(error) if error.code == -32601 => return Err(ExecutionAdmissionError::Unavailable),
            Err(_) => return Ok(EntryOutcome::Unknown),
        };
        let outcome: EntryOutcome = serde_json::from_value(response)
            .map_err(|_| ExecutionAdmissionError::Protocol("entry response decoding".into()))?;
        if let EntryOutcome::Applied { receipt } = &outcome {
            if receipt.admission != request.admission
                || receipt.entry_evidence_id != request.entry_evidence_id
            {
                return Err(ExecutionAdmissionError::Protocol(
                    "entry receipt association mismatch".into(),
                ));
            }
        }
        Ok(outcome)
    }
    async fn admit(
        &self,
        request: AdmissionRequest,
    ) -> Result<AdmissionOutcome, ExecutionAdmissionError> {
        let params = serde_json::to_value(&request)
            .map_err(|_| ExecutionAdmissionError::Protocol("admission request encoding".into()))?;
        let response = match self.transport.send_request(ADMIT_METHOD, params).await {
            Ok(response) => response,
            Err(error) if error.code == -32601 => return Err(ExecutionAdmissionError::Unavailable),
            Err(_) => return Ok(AdmissionOutcome::Unknown),
        };
        let outcome: AdmissionOutcome = serde_json::from_value(response)
            .map_err(|_| ExecutionAdmissionError::Protocol("admission response decoding".into()))?;
        if let AdmissionOutcome::Admitted { admission } = &outcome {
            if request
                .existing_admission
                .as_ref()
                .is_some_and(|existing| existing != admission)
            {
                return Err(ExecutionAdmissionError::Protocol(
                    "existing admission confirmation changed the ticket".into(),
                ));
            }
            request
                .snapshot
                .validate_admission(admission)
                .map_err(|_| {
                    ExecutionAdmissionError::Protocol("admission association mismatch".into())
                })?;
        }
        Ok(outcome)
    }

    async fn settle(
        &self,
        request: SettlementRequest,
    ) -> Result<SettlementOutcome, ExecutionAdmissionError> {
        if !request.proof.validates(&request.admission) {
            return Err(ExecutionAdmissionError::Protocol(
                "stopped proof association mismatch".into(),
            ));
        }
        let params = serde_json::to_value(&request)
            .map_err(|_| ExecutionAdmissionError::Protocol("settlement request encoding".into()))?;
        let response = match self.transport.send_request(SETTLE_METHOD, params).await {
            Ok(response) => response,
            Err(error) if error.code == -32601 => return Err(ExecutionAdmissionError::Unavailable),
            Err(_) => return Ok(SettlementOutcome::Unknown),
        };
        let outcome: SettlementOutcome = serde_json::from_value(response).map_err(|_| {
            ExecutionAdmissionError::Protocol("settlement response decoding".into())
        })?;
        if let SettlementOutcome::Applied { receipt } = &outcome {
            if receipt.admission != request.admission
                || receipt.evidence_id != request.proof.evidence_id()
            {
                return Err(ExecutionAdmissionError::Protocol(
                    "settlement receipt association mismatch".into(),
                ));
            }
        }
        Ok(outcome)
    }
}

#[cfg(test)]
#[path = "execution_admission_test.rs"]
mod tests;
