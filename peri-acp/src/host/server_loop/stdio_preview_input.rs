//! Map stdio Preview submissions onto the existing durable user input mailbox.

use serde_json::{json, Value};

use peri_acp_types::session::{
    DispatchUserInputsRequest, EnqueueUserInputRequest, TakeBackUserInputRequest, UserInputState,
};

use super::ServerLoop;
use crate::transport::types::{AcpError, RequestId};

impl ServerLoop<'_> {
    pub(super) async fn handle_stdio_preview_input(
        &self,
        id: RequestId,
        method: &str,
        params: Value,
    ) {
        let result = self.stdio_preview_input(method, &params).await;
        let _ = self.transport.send_response(id, result).await;
    }

    pub(super) async fn handle_stdio_preview_close(&self, id: RequestId, params: Value) {
        let sid = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if params
            .get("inputIds")
            .and_then(Value::as_array)
            .is_some_and(|ids| !ids.is_empty())
        {
            match self
                .stdio_preview_input(crate::host::stdio::v2::CLOSE, &params)
                .await
            {
                Ok(value) => {
                    let withdrawn = confirmed_withdrawals(&value);
                    self.transport.settle_withdrawn(&sid, &withdrawn).await;
                }
                Err(error) => {
                    // Withdrawals confirmed before the failure are already
                    // committed in the store and can no longer be delivered;
                    // settle them even though the close request fails.
                    settle_failed_close(self.transport.as_ref(), &sid, &error).await;
                    let _ = self.transport.send_response(id, Err(error)).await;
                    return;
                }
            }
        }
        self.dispatch_request(
            id,
            "session/close".into(),
            json!({"sessionId": params["sessionId"]}),
        )
        .await;
    }

    async fn stdio_preview_input(&self, method: &str, params: &Value) -> Result<Value, AcpError> {
        let sid = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let environment = {
            let sessions = self.sessions.lock().await;
            let state = sessions
                .get(sid)
                .filter(|state| !state.closing && state.close_in_flight == 0)
                .ok_or_else(|| AcpError::new(-32002, "Session not active"))?;
            state.environment.clone()
        };
        let cfg = environment.as_ref().map(|env| &env.cfg).unwrap_or(self.cfg);
        if !cfg.session_manager.get_caps(sid).user_input_queue {
            return Err(AcpError::new(
                -32002,
                "Session has no ACP Preview input endpoint",
            ));
        }
        crate::host::session_endpoint::register_session_endpoint(
            self.sessions,
            sid,
            self.connection,
            self.transport,
        )
        .await;
        let mailbox = crate::host::user_input::ensure_mailbox(sid, cfg, self.transport).await?;
        let generation = mailbox.generation().to_owned();
        let command = || uuid::Uuid::now_v7().to_string();
        let withdraw = |input_id: String| {
            let request = TakeBackUserInputRequest {
                session_id: sid.to_owned(),
                generation: generation.clone(),
                command_id: command(),
                input_id,
            };
            let mailbox = &mailbox;
            async move {
                mailbox
                    .withdraw_uncommitted(&request)
                    .await
                    .map(|receipt| {
                        receipt
                            .results
                            .iter()
                            .any(|item| item.state == UserInputState::Withdrawn)
                    })
                    .map_err(mailbox_error)
            }
        };

        if matches!(
            method,
            crate::host::stdio::v2::CANCEL | crate::host::stdio::v2::CLOSE
        ) {
            let mut withdrawn = Vec::new();
            let mut first_error = None;
            if let Some(inputs) = params.get("inputIds").and_then(Value::as_array) {
                for input_id in inputs.iter().filter_map(Value::as_str) {
                    match withdraw(input_id.to_owned()).await {
                        Ok(true) => withdrawn.push(input_id.to_owned()),
                        Ok(false) => {}
                        Err(error) if method == crate::host::stdio::v2::CLOSE => {
                            // Keep withdrawing. A confirmed withdrawal can no
                            // longer deliver, and the adapter must learn that
                            // even if this close ultimately fails.
                            if first_error.is_none() {
                                first_error = Some(error);
                            }
                        }
                        Err(error) => {
                            tracing::warn!(code = error.code, "ACP prompt withdrawal failed")
                        }
                    }
                }
            }
            if method == crate::host::stdio::v2::CANCEL {
                self.dispatch_notification("session/cancel".to_owned(), json!({"sessionId": sid}))
                    .await;
                return Ok(json!(withdrawn));
            }
            return match first_error {
                Some(error) => Err(close_error(error, &withdrawn)),
                None => Ok(json!(withdrawn)),
            };
        }

        let input_id = params
            .get("inputId")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if method == crate::host::stdio::v2::WITHDRAW {
            if withdraw(input_id.to_owned()).await? {
                return Ok(json!({}));
            }
            return Err(AcpError::new(-32602, "Prompt is no longer withdrawable"));
        }

        let (_, content, _) = crate::dispatch::extract_prompt_params(params)?;
        crate::host::workspace::reassert_expected(cfg, sid, None).await?;
        mailbox
            .enqueue(&EnqueueUserInputRequest {
                session_id: sid.to_owned(),
                generation: generation.clone(),
                command_id: command(),
                input_id: input_id.to_owned(),
                original_draft: content.text_content(),
                content,
                plugin_selections: Vec::new(),
            })
            .await
            .map_err(mailbox_error)?;
        if let Err(error) = mailbox
            .dispatch_non_interrupting(&DispatchUserInputsRequest {
                session_id: sid.to_owned(),
                generation: generation.clone(),
                command_id: command(),
                input_ids: vec![input_id.to_owned()],
            })
            .await
        {
            // The store could not confirm whether the input was handed off.
            // Withdrawal decides the race: a confirmed withdrawal means
            // nothing was inserted, so an error is safe and a same-key retry
            // cannot duplicate it. Otherwise the input may still be received;
            // keep the request bound to the input and let delivery,
            // cancellation, or close settle it.
            match withdraw(input_id.to_owned()).await {
                Ok(true) => return Err(dispatch_error(error)),
                Ok(false) => {}
                Err(withdrawal) => tracing::warn!(
                    code = withdrawal.code,
                    "ACP prompt withdrawal after an unconfirmed dispatch failed"
                ),
            }
            return Ok(json!({}));
        }

        crate::host::user_input::schedule_mailbox(
            sid,
            self.sessions,
            self.prompt_locks,
            self.cfg,
            self.transport,
            self.cont_tx,
        );
        Ok(json!({}))
    }
}

fn invalid() -> AcpError {
    AcpError::new(-32602, "Invalid internal ACP Preview input")
}

/// Error reply for a close whose withdrawals confirmed inputs before the first
/// failure: the adapter settles exactly `withdrawn`, and the host reads the
/// same list back through [`confirmed_withdrawals`].
fn close_error(error: AcpError, withdrawn: &[String]) -> AcpError {
    error.with_data(json!({"withdrawn": withdrawn}))
}

/// Inputs a Preview withdrawal reply confirmed.
///
/// A successful close reply is the bare array of input ids; a failed one keeps
/// the same list under `withdrawn` (see [`close_error`]). Both shapes decode
/// here, so no caller has to know which reply it holds.
fn confirmed_withdrawals(value: &Value) -> Vec<String> {
    let list = match value {
        Value::Array(_) => Some(value),
        Value::Object(_) => value.get("withdrawn"),
        _ => None,
    };
    list.and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A failed close still reports the withdrawals confirmed before its first
/// error. Those inputs can no longer be delivered, so settle exactly those
/// before the error reaches the adapter.
async fn settle_failed_close(
    transport: &dyn crate::transport::AcpTransport,
    session_id: &str,
    error: &AcpError,
) {
    let confirmed = error
        .data
        .as_ref()
        .map(confirmed_withdrawals)
        .unwrap_or_default();
    transport.settle_withdrawn(session_id, &confirmed).await;
}

#[cfg(test)]
#[path = "stdio_preview_input_test.rs"]
mod tests;

fn mailbox_error(error: impl std::fmt::Display) -> AcpError {
    AcpError::new(-32602, error.to_string())
}

/// A confirmed withdrawal means the dispatch failure did not insert anything.
/// A storage failure is then a retryable internal error, not invalid params.
fn dispatch_error(error: peri_agent::session::user_input_mailbox::UserInputQueueError) -> AcpError {
    use peri_agent::session::user_input_mailbox::UserInputQueueError;
    let code = match &error {
        UserInputQueueError::Store(_) => -32603,
        _ => -32602,
    };
    AcpError::new(code, error.to_string())
}
