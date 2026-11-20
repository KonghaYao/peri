use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{MAX_INSTRUCTIONS_BYTES, MAX_MODEL_BYTES, MAX_TEXT_BYTES};
use crate::domain::{
    ContentBlock, Cursor, EnvironmentKind, ErrorCode, ErrorKind, InputKind, InputMode,
    InputSubmission, ItemOrder, ItemsWindow, LogicalId, McpHeader, McpToolSpec, SwarmError,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionBody {
    #[serde(rename = "logicalId", default)]
    pub logical_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Caller-declared tools (OpenAI Agents API shape). Kept as a raw value so
    /// a malformed entry or a non-array `tools` reports its exact field path
    /// instead of axum's generic rejection (`resolve_tools` owns normalization).
    #[serde(default)]
    pub tools: Option<Value>,
    #[serde(default)]
    pub agent: Option<AgentBody>,
    #[serde(default)]
    pub input: Option<CreateInputBody>,
    #[serde(default)]
    pub environment: Option<EnvironmentBody>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBody {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub tools: Option<Value>,
}

/// One raw tool entry. Every known field is read as a [`Value`] so the
/// normalizer can name the offending path; unknown fields are preserved in
/// [`McpToolBody::extra`] instead of being rejected.
#[derive(Debug, Deserialize)]
struct McpToolBody {
    #[serde(rename = "type", default)]
    kind: Option<Value>,
    #[serde(default)]
    server_label: Option<Value>,
    #[serde(default)]
    server_url: Option<Value>,
    #[serde(default)]
    headers: Option<Value>,
    #[serde(default)]
    require_approval: Option<Value>,
    #[serde(default)]
    allowed_tools: Option<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum CreateInputBody {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentBody {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(rename = "environment_id", alias = "environmentId", default)]
    pub environment_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitEventBody {
    #[serde(rename = "inputId", default)]
    pub input_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ContentBlock>,
    #[serde(rename = "pluginSelections", default)]
    pub plugin_selections: Vec<String>,
    #[serde(rename = "originalDraft", default)]
    pub original_draft: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    #[serde(rename = "afterSeq", default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub after: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemsQuery {
    #[serde(rename = "afterSeq", default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub after: Option<u64>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub order: Option<String>,
}

/// `logicalId` is optional for SDK clients: an `Idempotency-Key` gives a stable
/// derived identity, and a fully absent identity is generated once.
pub fn resolve_logical_id(
    body: &CreateSessionBody,
    headers: &HeaderMap,
) -> Result<LogicalId, SwarmError> {
    match body.logical_id.as_deref() {
        Some(value) => LogicalId::parse(value),
        None => match idempotency_key(headers)? {
            Some(key) => Ok(LogicalId::derived_from_idempotency_key(&key)),
            None => Ok(LogicalId::generate()),
        },
    }
}

pub fn resolve_agent(
    body: &CreateSessionBody,
) -> Result<(Option<String>, Option<String>), SwarmError> {
    let nested_model = body.agent.as_ref().and_then(|agent| agent.model.clone());
    let nested_instructions = body
        .agent
        .as_ref()
        .and_then(|agent| agent.instructions.clone());
    let model = merge_agent_field("model", body.model.clone(), nested_model)?;
    let instructions = merge_agent_field(
        "instructions",
        body.instructions.clone(),
        nested_instructions,
    )?;

    if let Some(model) = &model {
        if model.trim().is_empty() {
            return Err(SwarmError::invalid_input("model must not be empty"));
        }
        check_len("model", model, MAX_MODEL_BYTES)?;
    }
    if let Some(instructions) = &instructions {
        check_len("instructions", instructions, MAX_INSTRUCTIONS_BYTES)?;
    }
    Ok((model, instructions))
}

fn merge_agent_field(
    field: &str,
    flat: Option<String>,
    nested: Option<String>,
) -> Result<Option<String>, SwarmError> {
    match (flat, nested) {
        (Some(left), Some(right)) if left != right => Err(SwarmError::new(
            ErrorKind::BadRequest,
            ErrorCode::InvalidRequest,
            "flat and nested `agent` fields disagree",
        )
        .with_details(json!({ "field": field }))),
        (Some(value), _) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

/// Normalize caller-declared tools into typed MCP declarations (`design §2.2`).
///
/// Accepts the OpenAI Agents API MCP tool shape at both the flat and the nested
/// `agent` position. Nothing is filtered by name, URL or credential policy:
/// only shapes that cannot be represented as an [`McpToolSpec`] are rejected,
/// and every rejection names its field path. Non-MCP tool types are an
/// implementation gap reported as `unsupported_tool`, never silently dropped.
pub fn resolve_tools(body: &CreateSessionBody) -> Result<Vec<McpToolSpec>, SwarmError> {
    let flat = declared_tools(body.tools.as_ref(), "tools")?;
    let nested = match body.agent.as_ref() {
        Some(agent) => declared_tools(agent.tools.as_ref(), "agent.tools")?,
        None => &[],
    };
    let (declared, prefix) = match (flat.is_empty(), nested.is_empty()) {
        (true, true) => return Ok(Vec::new()),
        (false, true) => (flat, "tools"),
        (true, false) => (nested, "agent.tools"),
        (false, false) if flat == nested => (flat, "tools"),
        (false, false) => {
            return Err(
                SwarmError::invalid_input("flat and nested `agent` tools disagree")
                    .with_details(json!({ "field": "tools" })),
            );
        }
    };
    declared
        .iter()
        .enumerate()
        .map(|(index, value)| normalize_tool(prefix, index, value))
        .collect()
}

fn declared_tools<'a>(value: Option<&'a Value>, field: &'a str) -> Result<&'a [Value], SwarmError> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(invalid_tool(field.to_owned(), "tools must be an array")),
    }
}

fn normalize_tool(prefix: &str, index: usize, value: &Value) -> Result<McpToolSpec, SwarmError> {
    let path = format!("{prefix}[{index}]");
    let body: McpToolBody = serde_json::from_value(value.clone()).map_err(|_| {
        invalid_tool(
            path.clone(),
            "tool entry must be an object in the OpenAI Agents API tool shape",
        )
    })?;
    let kind = match body.kind.as_ref() {
        Some(Value::String(kind)) => kind.as_str(),
        Some(_) => {
            return Err(invalid_tool(
                format!("{path}.type"),
                "tool type must be a string",
            ));
        }
        None => {
            return Err(invalid_tool(
                format!("{path}.type"),
                "tool entry must declare `type`",
            ));
        }
    };
    if kind != "mcp" {
        return Err(SwarmError::unsupported_tool(kind, &format!("{path}.type")));
    }

    let mut spec = McpToolSpec::new(
        required_tool_string(body.server_label.as_ref(), &format!("{path}.server_label"))?,
        required_tool_string(body.server_url.as_ref(), &format!("{path}.server_url"))?,
    );
    spec.headers = normalize_headers(body.headers.as_ref(), &format!("{path}.headers"))?;
    spec.require_approval = verbatim_tool_value(
        body.require_approval.as_ref(),
        &format!("{path}.require_approval"),
        &[ValueKind::String, ValueKind::Object],
    )?;
    spec.allowed_tools = verbatim_tool_value(
        body.allowed_tools.as_ref(),
        &format!("{path}.allowed_tools"),
        &[ValueKind::Array, ValueKind::Object],
    )?;
    spec.extra = body.extra;
    Ok(spec)
}

fn invalid_tool(field: impl Into<String>, reason: &str) -> SwarmError {
    SwarmError::invalid_input(reason).with_details(json!({ "field": field.into() }))
}

fn required_tool_string(value: Option<&Value>, field: &str) -> Result<String, SwarmError> {
    let Some(value) = value else {
        return Err(invalid_tool(
            field,
            "field is required by the MCP tool shape",
        ));
    };
    let Some(text) = value.as_str() else {
        return Err(invalid_tool(field, "field must be a string"));
    };
    if text.trim().is_empty() {
        return Err(invalid_tool(field, "field must not be empty"));
    }
    Ok(text.to_owned())
}

fn normalize_headers(value: Option<&Value>, field: &str) -> Result<Vec<McpHeader>, SwarmError> {
    let mut headers = match value {
        None | Some(Value::Null) => Vec::new(),
        // Object form: `{"name": "value"}`. `serde_json`'s map is sorted by
        // name, so this form is canonical without further sorting.
        Some(Value::Object(map)) => map
            .iter()
            .map(|(name, value)| {
                let Some(text) = value.as_str() else {
                    return Err(invalid_tool(
                        format!("{field}.{name}"),
                        "header value must be a string",
                    ));
                };
                Ok(McpHeader {
                    name: name.clone(),
                    value: text.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        // Array form: `[{"name": ..., "value": ...}]`, repeated names allowed.
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let path = format!("{field}[{index}]");
                let Some(object) = item.as_object() else {
                    return Err(invalid_tool(
                        path,
                        "header must be an object with `name` and `value`",
                    ));
                };
                Ok(McpHeader {
                    name: required_tool_string(object.get("name"), &format!("{path}.name"))?,
                    value: required_tool_string(object.get("value"), &format!("{path}.value"))?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => {
            return Err(invalid_tool(
                field,
                "headers must be an object or an array of {name,value}",
            ));
        }
    };
    // Stable sort so both accepted shapes produce one canonical order while
    // repeated names keep their relative order.
    headers.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(headers)
}

#[derive(Clone, Copy)]
enum ValueKind {
    String,
    Array,
    Object,
}

fn verbatim_tool_value(
    value: Option<&Value>,
    field: &str,
    allowed: &[ValueKind],
) -> Result<Option<Value>, SwarmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let matches = allowed.iter().any(|kind| match kind {
        ValueKind::String => value.is_string(),
        ValueKind::Array => value.is_array(),
        ValueKind::Object => value.is_object(),
    });
    if !matches {
        return Err(invalid_tool(
            field,
            "field has a shape this contract cannot record verbatim",
        ));
    }
    Ok(Some(value.clone()))
}

pub fn resolve_create_input(body: &CreateSessionBody) -> Result<Option<String>, SwarmError> {
    let Some(input) = &body.input else {
        return Ok(None);
    };
    let text = match input {
        CreateInputBody::Text(text) => {
            check_len("input", text, MAX_TEXT_BYTES)?;
            text.clone()
        }
        CreateInputBody::Blocks(blocks) => {
            for block in blocks {
                check_len("input", block.as_text(), MAX_TEXT_BYTES)?;
            }
            let joined = blocks
                .iter()
                .map(ContentBlock::as_text)
                .collect::<Vec<_>>()
                .join("\n");
            check_len("input", &joined, MAX_TEXT_BYTES)?;
            joined
        }
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(text))
}

pub fn resolve_environment(body: &CreateSessionBody) -> Result<EnvironmentKind, SwarmError> {
    let Some(environment) = &body.environment else {
        return Ok(EnvironmentKind::None);
    };
    if let Some(id) = &environment.environment_id {
        if id.trim().is_empty() {
            return Err(SwarmError::invalid_input(
                "environment.environment_id must not be empty",
            ));
        }
    }
    let kind = environment.kind.as_deref().unwrap_or("none");
    match kind {
        "none" => {
            if environment.environment_id.is_some() {
                return Err(SwarmError::unsupported_environment(
                    "none with environment_id",
                ));
            }
            Ok(EnvironmentKind::None)
        }
        // `design §5.5`: never silently degrade hosted MCP to a local workspace.
        "openai_hosted" => Err(SwarmError::mcp_mode_unsupported()),
        other => Err(SwarmError::unsupported_environment(other)),
    }
}

pub fn resolve_submission(
    body: SubmitEventBody,
    headers: &HeaderMap,
) -> Result<InputSubmission, SwarmError> {
    let kind = match body.kind.as_deref() {
        None | Some("user") => InputKind::User,
        Some("control") => InputKind::Control,
        Some(other) => {
            return Err(
                SwarmError::invalid_input("kind must be `user` or `control`")
                    .with_details(json!({ "field": "kind", "received": other })),
            );
        }
    };
    let mode = match body.mode.as_deref() {
        None | Some("turn") => InputMode::Turn,
        Some("steer") => InputMode::Steer,
        Some(other) => {
            return Err(SwarmError::invalid_input("mode must be `turn` or `steer`")
                .with_details(json!({ "field": "mode", "received": other })));
        }
    };

    let header_id = idempotency_key(headers)?;
    let input_id = match (body.input_id.as_deref(), header_id.as_deref()) {
        (Some(body_id), Some(header_id)) if body_id != header_id => {
            return Err(SwarmError::invalid_input(
                "inputId and Idempotency-Key disagree",
            ));
        }
        (Some(body_id), _) => body_id.to_owned(),
        (None, Some(header_id)) => header_id.to_owned(),
        (None, None) => {
            return Err(SwarmError::invalid_input(
                "inputId is required; the server does not generate idempotency keys",
            ));
        }
    };
    if Uuid::parse_str(&input_id).is_err() {
        return Err(SwarmError::invalid_input("inputId must be a UUID"));
    }

    if let Some(text) = &body.text {
        check_len("text", text, MAX_TEXT_BYTES)?;
    }
    for block in &body.attachments {
        check_len("attachments", block.as_text(), MAX_TEXT_BYTES)?;
    }
    if let Some(draft) = &body.original_draft {
        check_len("originalDraft", draft, MAX_TEXT_BYTES)?;
    }

    let submission = InputSubmission {
        input_id,
        kind,
        mode,
        text: body.text,
        attachments: body.attachments,
        plugin_selections: body.plugin_selections,
        original_draft: body.original_draft,
    };
    if submission.is_empty() {
        return Err(SwarmError::empty_input());
    }
    Ok(submission)
}

pub fn resolve_events_cursor(
    query: &EventsQuery,
    headers: &HeaderMap,
) -> Result<Cursor, SwarmError> {
    let last_event_id = last_event_id(headers)?;
    Cursor::reconcile(query.after_seq, query.after, last_event_id)
}

pub fn resolve_items_window(query: &ItemsQuery) -> Result<ItemsWindow, SwarmError> {
    let after = Cursor::reconcile_optional(query.after_seq, query.after)?;
    let limit = query.limit.unwrap_or(ItemsWindow::DEFAULT_LIMIT);
    if limit == 0 || limit > ItemsWindow::MAX_LIMIT {
        return Err(SwarmError::invalid_query("limit must be within 1..=500")
            .with_details(json!({ "field": "limit", "received": limit })));
    }
    let order = match query.order.as_deref() {
        Some(value) => ItemOrder::parse(value)?,
        None => ItemOrder::Asc,
    };
    Ok(ItemsWindow {
        order,
        after,
        limit,
    })
}

pub fn last_event_id(headers: &HeaderMap) -> Result<Option<u64>, SwarmError> {
    let Some(value) = headers.get("last-event-id") else {
        return Ok(None);
    };
    let text = value.to_str().map_err(|_| {
        SwarmError::invalid_query("Last-Event-ID must be a decimal sequence number")
    })?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse::<u64>()
        .map(Some)
        .map_err(|_| SwarmError::invalid_query("Last-Event-ID must be a decimal sequence number"))
}

pub fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, SwarmError> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let text = value
        .to_str()
        .map_err(|_| SwarmError::invalid_input("Idempotency-Key must be ASCII"))?;
    if text.is_empty() {
        return Ok(None);
    }
    Ok(Some(text.to_owned()))
}

fn check_len(field: &str, value: &str, max: usize) -> Result<(), SwarmError> {
    if value.len() > max {
        return Err(
            SwarmError::invalid_input("field exceeds the contract size limit").with_details(
                json!({
                    "field": field,
                    "maxBytes": max,
                    "actualBytes": value.len(),
                }),
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "request_test.rs"]
mod tests;
