use std::collections::BTreeSet;

use serde_json::Value;

use crate::{
    ContentBlock, JsonObject, ModelMessage, ModelResponse, ModelResult, StopReason, TokenUsage,
    ToolCall,
};

use super::{
    replay::{self, ReplayBinding},
    request::{invalid, valid_tool_name},
    ResponsesProfile, TOOL_NAMESPACE,
};

/// A single parser is used for completed responses and replay validation.
/// Unknown output kinds are rejected: a hosted tool must never accidentally
/// become a successful Peri assistant turn.
pub(super) fn project_items(
    items: &[Value],
    profile: ResponsesProfile,
    allowed_tools: Option<&BTreeSet<String>>,
) -> ModelResult<(Vec<ContentBlock>, Vec<ToolCall>)> {
    let mut content = Vec::new();
    let mut calls = Vec::new();
    let mut ids = BTreeSet::new();
    let mut call_ids = BTreeSet::new();
    for item in items {
        if let Some(id) = item.get("id") {
            let id = id
                .as_str()
                .ok_or_else(|| invalid("Responses item id must be a string"))?;
            if id.is_empty() || !ids.insert(id) {
                return Err(invalid(
                    "Responses output contains empty or duplicate item IDs",
                ));
            }
        }
        match required_string(item, "type")? {
            "message" => {
                if required_string(item, "role")? != "assistant" {
                    return Err(invalid("Responses output message must have assistant role"));
                }
                validate_finished_status(item)?;
                for part in required_array(item, "content")? {
                    match required_string(part, "type")? {
                        "output_text" => content.push(ContentBlock::text(
                            required_string_allow_empty(part, "text")?,
                        )),
                        "refusal" => content.push(ContentBlock::text(required_string_allow_empty(
                            part, "refusal",
                        )?)),
                        _ => return Err(invalid("Unsupported Responses message content type")),
                    }
                }
            }
            "reasoning" => {
                for part in required_array(item, "summary")? {
                    if required_string(part, "type")? != "summary_text" {
                        return Err(invalid("Unsupported Responses reasoning summary type"));
                    }
                    content.push(ContentBlock::reasoning(required_string_allow_empty(
                        part, "text",
                    )?));
                }
            }
            "function_call" => {
                let name = function_name(item, profile)?;
                if allowed_tools.is_some_and(|tools| !tools.contains(name)) {
                    return Err(invalid("Responses called a function that was not offered"));
                }
                let call_id = required_string(item, "call_id")?;
                if !call_ids.insert(call_id) {
                    return Err(invalid(
                        "Responses output contains duplicate call_id values",
                    ));
                }
                validate_finished_status(item)?;
                let arguments = required_string_allow_empty(item, "arguments")?;
                let arguments: Value = serde_json::from_str(arguments).map_err(|error| {
                    invalid("Responses function arguments are not valid JSON").with_error(&error)
                })?;
                let arguments = JsonObject::from_value(arguments)
                    .map_err(|_| invalid("Responses function arguments must be a JSON object"))?;
                calls.push(ToolCall::new(call_id, name, arguments));
            }
            _ => {
                return Err(invalid(
                    "Unsupported Responses output item; hosted/executable tools are not enabled",
                ));
            }
        }
    }
    Ok((content, calls))
}

fn validate_finished_status(item: &Value) -> ModelResult<()> {
    if item
        .get("status")
        .filter(|status| !status.is_null())
        .is_some_and(|status| status.as_str() != Some("completed"))
    {
        return Err(invalid("Responses output item was not completed"));
    }
    Ok(())
}

pub(super) fn complete(
    response: &Value,
    binding: &ReplayBinding,
    allowed_tools: &BTreeSet<String>,
    request_id: Option<String>,
) -> ModelResult<ModelResponse> {
    if response
        .get("status")
        .is_some_and(|status| status.as_str() != Some("completed"))
    {
        return Err(invalid("response.completed carried a non-completed status"));
    }
    let items = required_array(response, "output")?;
    let (mut content, calls) = project_items(items, binding.profile, Some(allowed_tools))?;
    content.push(replay::state_block(binding, items));
    let reason = if calls.is_empty() && response.get("end_turn") == Some(&Value::Bool(false)) {
        StopReason::Continue
    } else if calls.is_empty() {
        StopReason::EndTurn
    } else {
        StopReason::ToolUse
    };
    let usage = response
        .get("usage")
        .filter(|usage| !usage.is_null())
        .map(parse_usage)
        .transpose()?;
    ModelResponse::new(
        ModelMessage::assistant(content, calls),
        reason,
        usage,
        request_id,
    )
}

fn parse_usage(value: &Value) -> ModelResult<TokenUsage> {
    let number = |value: &Value| {
        value
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| invalid("Responses usage token count is not a u32"))
    };
    let input_tokens = number(&value["input_tokens"])?;
    let output_tokens = number(&value["output_tokens"])?;
    let details = value
        .get("input_tokens_details")
        .filter(|value| !value.is_null());
    if details.is_some_and(|details| !details.is_object()) {
        return Err(invalid("Responses input_tokens_details must be an object"));
    }
    let detail_count = |field| {
        details
            .and_then(|details| details.get(field))
            .filter(|value| !value.is_null())
            .map(number)
            .transpose()
    };
    let cached = detail_count("cached_tokens")?;
    let written = detail_count("cache_write_tokens")?;
    // Both are subsets of total input, not additional input tokens. Use a
    // widened sum so untrusted counters cannot overflow before validation.
    if u64::from(cached.unwrap_or(0)) + u64::from(written.unwrap_or(0)) > u64::from(input_tokens) {
        return Err(invalid(
            "Responses cache read/write tokens exceed input tokens",
        ));
    }
    Ok(TokenUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: written,
        cache_read_input_tokens: cached,
    })
}

pub(super) fn function_name(item: &Value, profile: ResponsesProfile) -> ModelResult<&str> {
    let name = required_string(item, "name")?;
    if !valid_tool_name(name) {
        return Err(invalid("Invalid Responses function name"));
    }
    match (
        profile,
        item.get("namespace").filter(|value| !value.is_null()),
    ) {
        (ResponsesProfile::Platform, None) => {}
        (ResponsesProfile::ChatGptPlan, Some(Value::String(namespace)))
            if namespace == TOOL_NAMESPACE => {}
        _ => {
            return Err(invalid(
                "Responses function namespace does not match the offered tools",
            ));
        }
    }
    Ok(name)
}

pub(super) fn required_string<'a>(value: &'a Value, field: &str) -> ModelResult<&'a str> {
    required_string_allow_empty(value, field).and_then(|text| {
        if text.is_empty() {
            Err(invalid(&format!(
                "Responses field {field} must not be empty"
            )))
        } else {
            Ok(text)
        }
    })
}

pub(super) fn required_string_allow_empty<'a>(
    value: &'a Value,
    field: &str,
) -> ModelResult<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("Responses field {field} must be a string")))
}

pub(super) fn required_array<'a>(value: &'a Value, field: &str) -> ModelResult<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(&format!("Responses field {field} must be an array")))
}
