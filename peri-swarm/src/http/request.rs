use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{MAX_INSTRUCTIONS_BYTES, MAX_MODEL_BYTES, MAX_TEXT_BYTES};
use crate::domain::{
    ContentBlock, Cursor, EnvironmentKind, ErrorCode, ErrorKind, InputKind, InputMode,
    InputSubmission, ItemOrder, ItemsWindow, LogicalId, SwarmError,
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
