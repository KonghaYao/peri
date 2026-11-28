//! ACP v2 Preview wire projection. Does not own turn execution or session history.

use serde_json::{json, Value};

use crate::transport::types::AcpError;

pub(super) fn initialize_params(params: &Value) -> Result<Value, AcpError> {
    let Some(info) = params.get("info") else {
        return Err(AcpError::new(-32602, "ACP v2 initialize requires info"));
    };
    if !["name", "version"].into_iter().all(|key| {
        info.get(key)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    }) {
        return Err(AcpError::new(-32602, "Invalid ACP client info"));
    }
    // v2 does not grant v1 client filesystem and terminal execution capabilities.
    // The queue capabilities are exclusively internal to this adapter.
    Ok(json!({
        "protocolVersion": 1, "clientInfo": info,
        "clientCapabilities": {"_meta": {"peri.userInputQueue": true}}
    }))
}

pub(super) fn initialize_response() -> Value {
    json!({
        "protocolVersion": 2,
        "info": {"name": "peri", "title": "Peri", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"session": {
            "prompt": {"image": {}}, "mcp": {"stdio": {}, "http": {}}
        }}
    })
}

// The content conversion upstream already uses canonical message IDs. Never
// guess an ID for a real message or expose private routing metadata.
pub(super) fn project_update(
    mut update: Value,
    resetting_replay_chunk: bool,
) -> Option<Vec<Value>> {
    let kind = update.get("sessionUpdate")?.as_str()?.to_owned();
    let object = update.as_object_mut()?;
    object.remove("_meta");
    match kind.as_str() {
        "agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk" => {
            let id = object.get("messageId")?.as_str()?.to_owned();
            if resetting_replay_chunk {
                let whole = kind.strip_suffix("_chunk")?;
                return Some(vec![
                    json!({"sessionUpdate": whole, "messageId": id, "content": []}),
                    update,
                ]);
            }
        }
        "tool_call" => {
            object.insert("sessionUpdate".into(), json!("tool_call_update"));
        }
        "tool_call_update" | "session_info_update" => {}
        // Legacy plans have no stable plan ID, and several legacy update
        // shapes are incompatible with v2. Do not emit a misleading upsert.
        _ => return None,
    }
    Some(vec![update])
}

pub(super) fn lifecycle_result(method: &str, mut result: Value) -> Value {
    if method == "session/new" || method == "session/resume" {
        if let Some(object) = result.as_object_mut() {
            // This preview does not yet advertise session config controls.
            for key in ["modes", "models", "configOptions", "_meta"] {
                object.remove(key);
            }
        }
    }
    result
}

pub(super) fn state(state: &str, stop_reason: Option<&str>) -> Value {
    let mut update = json!({"sessionUpdate": "state_update", "state": state});
    match stop_reason {
        Some("error") => {
            update["stopReason"] = json!("error");
            update["error"] = json!({"code": -32603, "message": "Peri execution failed"});
        }
        Some(stop_reason) => update["stopReason"] = json!(stop_reason),
        None => {}
    }
    update
}

pub(super) fn prompt_blocks(params: &Value) -> Result<Value, AcpError> {
    let invalid = || AcpError::new(-32602, "Invalid or unsupported prompt content");
    let blocks = params
        .get("prompt")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if blocks.is_empty() {
        return Err(invalid());
    }
    let mut accepted = Vec::with_capacity(blocks.len());
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                accepted.push(json!({"type":"text","text":text}));
            }
            Some("image") => {
                let mime = block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let data = block
                    .get("data")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                accepted.push(json!({"type":"image","mimeType":mime,"data":data}));
            }
            Some("resource_link") => {
                let uri = block
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid)?;
                // A link is a reference, not an instruction to fetch it.
                accepted.push(json!({"type":"text","text":format!("{name}: {uri}")}));
            }
            _ => return Err(invalid()),
        }
    }
    Ok(Value::Array(accepted))
}

pub(super) fn peer_request(method: &str, mut params: Value) -> Result<Value, AcpError> {
    match method {
        "session/request_permission" => {
            let tool = params
                .as_object_mut()
                .and_then(|params| params.remove("toolCall"))
                .ok_or_else(|| AcpError::new(-32603, "Missing permission tool call"))?;
            params["title"] = json!(tool
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Approve tool call"));
            params["subject"] = json!({"type":"tool_call","toolCall":tool});
            Ok(params)
        }
        "elicitation/create" => Ok(params),
        _ => Err(AcpError::new(
            -32601,
            "Peer method is unavailable over ACP v2 Preview",
        )),
    }
}

#[cfg(test)]
#[path = "wire_test.rs"]
mod tests;
