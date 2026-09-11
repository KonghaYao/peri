//! Short user input RPCs; execution is admitted separately by the Agent mailbox.

use std::{collections::HashMap, sync::Arc};

use peri_acp_types::session::{
    DispatchUserInputsRequest, EnqueueUserInputRequest, TakeBackUserInputRequest,
    UserInputQueueSnapshotRequest,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use super::super::{AcpServerConfig, SessionState};
use crate::transport::{types::AcpError, AcpTransport};

pub(super) fn handle_user_input(
    method: &str,
    params: &Value,
    cfg: &AcpServerConfig,
    sessions: &HashMap<String, SessionState>,
    transport: &Arc<dyn AcpTransport>,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?;
    if !cfg.session_manager.get_caps(session_id).user_input_queue {
        return Err(AcpError::new(
            -32601,
            "user input queue capability not negotiated",
        ));
    }
    let state = sessions
        .get(session_id)
        .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
    if method != "session/input/snapshot" && !state.lease.is_writer("default") {
        return Err(AcpError::new(
            -32602,
            "read-only observer cannot change user input queue",
        ));
    }
    let mailbox = super::super::user_input::ensure_mailbox(session_id, cfg, transport)?;
    let rejected = |error: String| {
        AcpError::new(-32602, error).with_data(serde_json::json!({
            "rejected": true,
            "snapshot": mailbox.snapshot(),
        }))
    };
    let receipt = match method {
        "session/input/enqueue" => {
            let request: EnqueueUserInputRequest = decode(params)?;
            mailbox.enqueue(&request)
        }
        "session/input/dispatch" => {
            let request: DispatchUserInputsRequest = decode(params)?;
            mailbox.dispatch(&request)
        }
        "session/input/takeback" => {
            let request: TakeBackUserInputRequest = decode(params)?;
            mailbox.take_back(&request)
        }
        "session/input/snapshot" => {
            let request: UserInputQueueSnapshotRequest = decode(params)?;
            let snapshot = mailbox.snapshot();
            if request
                .generation
                .is_some_and(|generation| generation != snapshot.generation)
            {
                return Err(rejected("user input queue generation changed".into()));
            }
            return encode(snapshot);
        }
        _ => return Err(AcpError::new(-32601, "unknown user input method")),
    };
    encode(receipt.map_err(|error| rejected(error.to_string()))?)
}

fn decode<T: DeserializeOwned>(params: &Value) -> Result<T, AcpError> {
    serde_json::from_value(params.clone())
        .map_err(|error| AcpError::new(-32602, format!("invalid user input request: {error}")))
}

fn encode<T: Serialize>(value: T) -> Result<Value, AcpError> {
    serde_json::to_value(value)
        .map_err(|_| AcpError::new(-32603, "user input response serialization failed"))
}
