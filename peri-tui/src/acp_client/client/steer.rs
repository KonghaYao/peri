use std::sync::atomic::Ordering;

use peri_acp::{
    event::AcpEvent,
    transport::{AcpTransport, types::AcpError},
};
use peri_acp_types::session::{
    DispatchUserInputsRequest, EnqueueUserInputRequest, TakeBackUserInputRequest,
    UserInputQueueReceipt, UserInputQueueSnapshot, UserInputQueueSnapshotRequest,
};
use serde::{Serialize, de::DeserializeOwned};

use super::{AcpNotification, AcpTuiClient};

impl AcpTuiClient {
    pub fn supports_user_input_queue(&self) -> bool {
        self.user_input_queue.load(Ordering::Acquire)
    }

    async fn input_request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        session_id: &str,
        params: &P,
    ) -> Result<R, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.input_request_under_gate(method, session_id, params)
            .await
    }

    async fn input_request_under_gate<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        session_id: &str,
        params: &P,
    ) -> Result<R, AcpError> {
        if self.lifecycle.current_session_id().as_deref() != Some(session_id) {
            return Err(AcpError::new(-32602, "user input session changed"));
        }
        let params = serde_json::to_value(params)
            .map_err(|error| AcpError::new(-32602, error.to_string()))?;
        let result = self.transport.send_request(method, params).await?;
        serde_json::from_value(result)
            .map_err(|error| AcpError::new(-32603, format!("invalid user input receipt: {error}")))
    }

    pub async fn user_input_snapshot(
        &self,
        session_id: &str,
    ) -> Result<UserInputQueueSnapshot, AcpError> {
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.user_input_snapshot_under_gate(session_id).await
    }

    pub(super) async fn user_input_snapshot_under_gate(
        &self,
        session_id: &str,
    ) -> Result<UserInputQueueSnapshot, AcpError> {
        let (bound_session, local_generation) = self
            .lifecycle
            .stable_identity()
            .ok_or_else(|| AcpError::new(-32602, "no stable user input session"))?;
        if bound_session != session_id {
            return Err(AcpError::new(-32602, "user input session changed"));
        }
        let snapshot: UserInputQueueSnapshot = self
            .input_request_under_gate(
                "session/input/snapshot",
                session_id,
                &UserInputQueueSnapshotRequest {
                    session_id: session_id.to_owned(),
                    generation: None,
                },
            )
            .await?;
        if snapshot.session_id != session_id {
            return Err(AcpError::new(
                -32603,
                "user input snapshot session mismatch",
            ));
        }
        if !self.lifecycle.bind_user_input_generation(
            session_id,
            local_generation,
            &snapshot.generation,
        ) {
            return Err(AcpError::new(-32602, "user input snapshot owner changed"));
        }
        if let Some(request_id) = &snapshot.active_request_id
            && let Some(claims) =
                self.lifecycle
                    .open_user_input_run(session_id, &snapshot.generation, request_id)
        {
            self.settle_claims_owned(claims).await;
            self.flush_buffered(vec![AcpNotification::AgentEvent {
                session_id: session_id.to_owned(),
                event: AcpEvent::UserInputRunStarted {
                    generation: snapshot.generation.clone(),
                    request_id: request_id.clone(),
                },
            }]);
        }
        Ok(snapshot)
    }

    pub async fn enqueue_user_input(
        &self,
        request: &EnqueueUserInputRequest,
    ) -> Result<UserInputQueueReceipt, AcpError> {
        let receipt = self
            .input_request("session/input/enqueue", &request.session_id, request)
            .await?;
        validate_receipt(receipt, &request.session_id, &request.generation)
    }

    pub async fn dispatch_user_inputs(
        &self,
        request: &DispatchUserInputsRequest,
    ) -> Result<UserInputQueueReceipt, AcpError> {
        let receipt = self
            .input_request("session/input/dispatch", &request.session_id, request)
            .await?;
        validate_receipt(receipt, &request.session_id, &request.generation)
    }

    pub async fn take_back_user_input(
        &self,
        request: &TakeBackUserInputRequest,
    ) -> Result<UserInputQueueReceipt, AcpError> {
        let receipt = self
            .input_request("session/input/takeback", &request.session_id, request)
            .await?;
        validate_receipt(receipt, &request.session_id, &request.generation)
    }
}

fn validate_receipt(
    receipt: UserInputQueueReceipt,
    session_id: &str,
    generation: &str,
) -> Result<UserInputQueueReceipt, AcpError> {
    if receipt.snapshot.session_id != session_id || receipt.snapshot.generation != generation {
        return Err(AcpError::new(
            -32603,
            "user input receipt instance mismatch",
        ));
    }
    Ok(receipt)
}

#[cfg(test)]
#[path = "steer_test.rs"]
mod tests;
