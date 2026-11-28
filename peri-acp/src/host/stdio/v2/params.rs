//! Parameter translation between the ACP v2 wire and the shared v1 host.

use std::path::Path;

use serde_json::{json, Value};

use crate::transport::types::AcpError;

use super::errors;

/// Translate a v2 `initialize` request into the v1 host's parameter shape.
///
/// v2 grants no v1 client filesystem/terminal execution capabilities. The
/// client's elicitation declaration (`capabilities.elicitation` on the v2 wire)
/// is carried through as the v1 `clientCapabilities` field the host reads, so
/// the host-side broker sees the same facts; everything the adapter needs itself
/// (`peri.*`) is added under the internal `_meta`. `peri.replay` is set because
/// the preview always serves replay; `peri.userInputQueue` because prompts go
/// through the durable mailbox.
pub(super) fn initialize_params(params: &Value) -> Result<Value, AcpError> {
    let Some(info) = params.get("info") else {
        return Err(errors::invalid_params("ACP v2 initialize requires info"));
    };
    if !["name", "version"].into_iter().all(|key| {
        info.get(key)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    }) {
        return Err(errors::invalid_params("Invalid ACP client info"));
    }
    let mut capabilities = serde_json::Map::new();
    if let Some(elicitation) = params
        .get("capabilities")
        .and_then(|capabilities| capabilities.get("elicitation"))
        .filter(|elicitation| elicitation.is_object())
    {
        capabilities.insert("elicitation".to_owned(), elicitation.clone());
    }
    capabilities.insert(
        "_meta".to_owned(),
        json!({"peri.userInputQueue": true, "peri.replay": true}),
    );
    Ok(json!({
        "protocolVersion": 1,
        "clientInfo": info,
        "clientCapabilities": Value::Object(capabilities)
    }))
}

/// Validate the setup parameters both session entry points share.
///
/// `cwd` is required and must be absolute (pinned `AbsolutePath`). The preview
/// advertises neither `additionalDirectories` nor `systemPrompt`, so a client
/// that sends them is told so rather than having them silently dropped.
pub(super) fn setup_params(params: &Value) -> Result<(), AcpError> {
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .ok_or_else(|| errors::invalid_params("cwd is required"))?;
    if !Path::new(cwd).is_absolute() {
        return Err(errors::invalid_params("cwd must be an absolute path"));
    }
    if params
        .get("additionalDirectories")
        .and_then(Value::as_array)
        .is_some_and(|roots| !roots.is_empty())
    {
        return Err(errors::invalid_params(
            "additionalDirectories is not advertised by Peri ACP v2 Preview",
        ));
    }
    if params
        .get("systemPrompt")
        .is_some_and(|prompt| !prompt.is_null())
    {
        return Err(errors::invalid_params(
            "systemPrompt is not advertised by Peri ACP v2 Preview",
        ));
    }
    Ok(())
}

/// Translate the v2 prompt content blocks the preview accepts.
///
/// `resource_link` is passed as a text reference (never fetched); `audio` and
/// embedded `resource` blocks are rejected with Invalid params, matching the
/// agent capabilities advertised in `initialize`.
pub(super) fn prompt_blocks(params: &Value) -> Result<Value, AcpError> {
    let invalid = || errors::invalid_params("Invalid or unsupported prompt content");
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

/// Translate an agent-initiated peer request into its v2 shape.
pub(super) fn peer_request(method: &str, mut params: Value) -> Result<Value, AcpError> {
    match method {
        "session/request_permission" => {
            let tool = params
                .as_object_mut()
                .and_then(|params| params.remove("toolCall"))
                .ok_or_else(|| AcpError::new(errors::INTERNAL, "Missing permission tool call"))?;
            params["title"] = json!(tool
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Approve tool call"));
            params["subject"] = json!({"type":"tool_call","toolCall":tool});
            Ok(params)
        }
        "elicitation/create" => Ok(params),
        _ => Err(AcpError::new(
            errors::METHOD_NOT_FOUND,
            "Peer method is unavailable over ACP v2 Preview",
        )),
    }
}

#[cfg(test)]
#[path = "params_test.rs"]
mod tests;
