use std::{future::Future, sync::atomic::Ordering, task::Poll};

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
    pub(crate) const USER_INPUT_RECEIPT_TIMEOUT: std::time::Duration =
        std::time::Duration::from_secs(10);
    pub fn supports_user_input_queue(&self) -> bool {
        self.user_input_queue.load(Ordering::Acquire)
    }

    async fn input_request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        session_id: &str,
        params: &P,
    ) -> Result<R, AcpError> {
        let mut epoch = self.session_load_reservations.epoch_tx.subscribe();
        let gate_started = peri_time::monotonic_now();
        loop {
            let operation = self.lifecycle.operation_gate().lock().await;
            let pending = *self.session_load_reservations.pending.lock().unwrap() > 0;
            if pending {
                drop(operation);
                self.wait_for_session_load(&mut epoch).await?;
                continue;
            }
            self.check_restore_error()?;
            tracing::debug!(
                method,
                session_id,
                stage = "gate",
                elapsed_ms = gate_started.elapsed().as_millis() as u64,
                "user input timing"
            );
            let identity = self.lifecycle.stable_identity();
            if identity.as_ref().map(|identity| identity.0.as_str()) != Some(session_id) {
                return Err(AcpError::new(-32602, "user input session changed"));
            }
            let params = serde_json::to_value(params)
                .map_err(|error| AcpError::new(-32602, error.to_string()))?;
            let request_started = peri_time::monotonic_now();
            let mut request = Box::pin(self.transport.send_request(method, params));
            let first_poll =
                std::future::poll_fn(|context| Poll::Ready(request.as_mut().poll(context))).await;
            drop(operation);
            tracing::debug!(
                method,
                session_id,
                stage = "request",
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "user input timing"
            );
            let receipt_started = peri_time::monotonic_now();
            let result = match first_poll {
                Poll::Ready(result) => result,
                Poll::Pending => peri_time::timeout(Self::USER_INPUT_RECEIPT_TIMEOUT, request)
                    .await
                    .unwrap_or_else(|_| Err(AcpError::new(-32603, "user input receipt timed out"))),
            };
            tracing::debug!(
                method,
                session_id,
                stage = "receipt",
                elapsed_ms = receipt_started.elapsed().as_millis() as u64,
                "user input timing"
            );
            let result = result?;
            if self.lifecycle.stable_identity() != identity {
                return Err(AcpError::new(-32603, "user input receipt owner changed"));
            }
            return serde_json::from_value(result).map_err(|error| {
                AcpError::new(-32603, format!("invalid user input receipt: {error}"))
            });
        }
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
        let mut epoch = self.session_load_reservations.epoch_tx.subscribe();
        let operation = loop {
            let operation = self.lifecycle.operation_gate().lock().await;
            if *self.session_load_reservations.pending.lock().unwrap() == 0 {
                break operation;
            }
            drop(operation);
            self.wait_for_session_load(&mut epoch).await?;
        };
        self.check_restore_error()?;
        let identity = self
            .lifecycle
            .stable_identity()
            .ok_or_else(|| AcpError::new(-32602, "no stable user input session"))?;
        let snapshot_identity = self.lifecycle.user_input_snapshot_identity(session_id);
        if snapshot_identity.is_none() {
            return peri_time::timeout(
                Self::USER_INPUT_RECEIPT_TIMEOUT,
                self.user_input_snapshot_under_gate(session_id),
            )
            .await
            .unwrap_or_else(|_| Err(AcpError::new(-32603, "user input snapshot timed out")));
        }
        drop(operation);
        let snapshot_started = peri_time::monotonic_now();
        let snapshot: UserInputQueueSnapshot = self
            .input_request(
                "session/input/snapshot",
                session_id,
                &UserInputQueueSnapshotRequest {
                    session_id: session_id.to_owned(),
                    generation: None,
                },
            )
            .await?;
        tracing::debug!(
            stage = "snapshot",
            elapsed_ms = snapshot_started.elapsed().as_millis() as u64,
            "user input timing"
        );
        let _operation = self.lifecycle.operation_gate().lock().await;
        self.accept_user_input_snapshot(
            session_id,
            identity.1,
            snapshot,
            self.lifecycle.user_input_snapshot_identity(session_id) == snapshot_identity,
        )
        .await
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
        self.accept_user_input_snapshot(session_id, local_generation, snapshot, true)
            .await
    }

    async fn accept_user_input_snapshot(
        &self,
        session_id: &str,
        local_generation: u64,
        snapshot: UserInputQueueSnapshot,
        restore_run: bool,
    ) -> Result<UserInputQueueSnapshot, AcpError> {
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
        if restore_run
            && let Some(request_id) = &snapshot.active_request_id
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

#[cfg(test)]
#[path = "latency_test.rs"]
mod latency_tests;
