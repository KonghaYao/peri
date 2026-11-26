use std::collections::{BTreeMap, HashMap};

use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{MAX_INSTRUCTIONS_BYTES, MAX_MODEL_BYTES, MAX_TEXT_BYTES};
use crate::domain::{
    ContentBlock, Cursor, EnvironmentKind, ErrorCode, ErrorKind, EventCursor, InputKind, InputMode,
    InputSubmission, ItemOrder, ItemsWindow, LogicalId, McpHeader, McpToolSpec, PromptCacheOptions,
    PromptCacheTtl, SessionsWindow, SwarmError, TurnsWindow, MAX_METADATA_KEY_CHARS,
    MAX_METADATA_PAIRS, MAX_METADATA_VALUE_CHARS,
};
use crate::service::{SessionUpdateCommand, UpdateField};

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
    /// Official `metadata` string map. Omitted keeps the stored value on an
    /// attach; explicit `null`/`{}` asserts "no metadata" (`sessions.update`
    /// uses the same tri-state).
    #[serde(default)]
    pub metadata: UpdateField<Value>,
    /// Official `prompt_cache_options` (`{ttl: "30m"|"12h"}`); `null` clears and
    /// `{}` uses the official default TTL `30m`.
    #[serde(rename = "prompt_cache_options", default)]
    pub prompt_cache_options: UpdateField<Value>,
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentBody {
    /// Official `agent.model` is a string; [`UpdateField`] keeps an explicit
    /// `null` distinguishable from omission so it can be rejected instead of
    /// silently treated as "not supplied".
    #[serde(default)]
    pub model: UpdateField<String>,
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
    /// Official `transport` object (`openai@7.60.0`, `agents.d.ts:3726`).
    /// Kept as a raw value so the normalizer names the offending nested path.
    #[serde(default)]
    transport: Option<Value>,
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
    /// Official Agents API envelope. When present, the native fields are
    /// rejected instead of being interpreted (`design §4.4`).
    #[serde(default)]
    pub events: Option<Value>,
    #[serde(rename = "inputId", default)]
    pub input_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// `Option` distinguishes an absent field from `[]` so the official
    /// envelope can reject any native field, including an empty one. The
    /// deserializer still rejects an explicit `null` exactly like the previous
    /// `Vec` field did.
    #[serde(default, deserialize_with = "present_vec")]
    pub attachments: Option<Vec<ContentBlock>>,
    #[serde(rename = "pluginSelections", default, deserialize_with = "present_vec")]
    pub plugin_selections: Option<Vec<String>>,
    #[serde(rename = "originalDraft", default)]
    pub original_draft: Option<String>,
}

fn present_vec<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Vec::<T>::deserialize(deserializer).map(Some)
}

/// The official message input event.
const OFFICIAL_INPUT_EVENT: &str = "agent.session.input.message";
/// The official MCP approval answer event.
const OFFICIAL_APPROVAL_RESULT_EVENT: &str = "agent.session.input.mcp_approval_request_result";

/// One accepted `POST /events` submission.
#[derive(Clone, Debug, PartialEq)]
pub enum SubmitEvent {
    /// A user/control input that opens or steers a turn.
    Input(InputSubmission),
    /// An answer to a pending MCP approval. It settles one tool call and never
    /// creates or queues a turn.
    Approval(ApprovalAnswer),
}

/// One validated approval answer.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalAnswer {
    pub request_id: String,
    pub decision: crate::domain::ApprovalDecision,
    /// `Idempotency-Key` of this answer (optional); a replay returns the same
    /// receipt instead of a conflict.
    pub input_id: Option<String>,
}

/// One entry of the official envelope. `type` stays a plain string so an
/// unsupported official event kind is reported by name instead of collapsing
/// into a serde enum rejection.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialEventBody {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    input: Option<Value>,
}

/// Approval result entry. The public id is opaque and is not parsed.
///
/// The official (`openai@7.60.0`) shape is:
///
/// ```json
/// {
///   "type": "agent.session.input.mcp_approval_request_result",
///   "request_id": "appr_...",
///   "response": { "type": "mcp_tool_call", "decision": "approve" }
/// }
/// ```
///
/// The top-level `decision` spelling is kept as a native-client extension.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialApprovalResultBody {
    #[serde(rename = "type")]
    kind: String,
    #[serde(
        rename = "request_id",
        alias = "requestId",
        alias = "approval_request_id"
    )]
    request_id: Option<Value>,
    #[serde(default)]
    response: Option<OfficialApprovalResponseBody>,
    #[serde(default)]
    decision: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialApprovalResponseBody {
    #[serde(rename = "type", default)]
    kind: Option<Value>,
    #[serde(default)]
    decision: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfficialInputMessageBody {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<Value>,
}

/// Content blocks are read as a flat object so an unsupported block type can be
/// reported at its index before its fields are interpreted.
#[derive(Debug, Deserialize)]
struct OfficialContentBlockBody {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsQuery {
    #[serde(rename = "afterSeq", default)]
    pub after_seq: Option<u64>,
    #[serde(default)]
    pub after: Option<u64>,
    /// Official `EventStreamParams.include_obfuscation` (snake_case, defaults to
    /// `true`). The gateway never emits obfuscation padding, so the value is
    /// accepted and ignored; it cannot change replay or live frames.
    #[serde(default)]
    pub include_obfuscation: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemsQuery {
    #[serde(rename = "afterSeq", default)]
    pub after_seq: Option<u64>,
    /// Decimal `seq` (our spelling) or `itim_...` item id (official
    /// `CursorPage` spelling). Parsed by [`resolve_items_window`].
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub order: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnsQuery {
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub order: Option<String>,
}

/// Raw `sessions.list` query. `metadata[key]=value` has dynamic keys, so the
/// map is validated by [`resolve_sessions_window`] instead of serde.
pub type SessionsQuery = HashMap<String, String>;

/// `POST /sessions/{id}` body (`SessionUpdateParams`).
///
/// [`UpdateField`] distinguishes omission (keep), explicit `null` (clear) and
/// a value (replace); `Option<Option<T>>` would collapse `null` into
/// "absent" on deserialization.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSessionBody {
    #[serde(default)]
    pub agent: Option<UpdateAgentBody>,
    #[serde(default)]
    pub metadata: UpdateField<Value>,
    #[serde(rename = "prompt_cache_options", default)]
    pub prompt_cache_options: UpdateField<Value>,
    #[serde(default)]
    pub review: UpdateField<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAgentBody {
    #[serde(default)]
    pub model: UpdateField<String>,
    #[serde(default)]
    pub tools: UpdateField<Value>,
    #[serde(default)]
    pub reasoning: UpdateField<Value>,
    #[serde(default)]
    pub service_tier: UpdateField<Value>,
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
    let nested_model = match body.agent.as_ref().map(|agent| &agent.model) {
        None | Some(UpdateField::Keep) => None,
        Some(UpdateField::Clear) => return Err(agent_model_not_string()),
        Some(UpdateField::Set(model)) => Some(model.clone()),
    };
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
/// `agent` position. The official `transport` object wins: `transport.type ==
/// "http"` maps its `server_url`/`headers` onto [`McpToolSpec`] and its
/// `authorization` becomes an `Authorization` header unless that header is
/// already present; a value without an auth scheme gets the `Bearer` prefix,
/// while `"Bearer ..."`/`"Basic ..."` is kept verbatim. The earlier flat
/// `server_url`/`headers` shape remains as a deprecated alias; when both shapes
/// carry a URL they must agree. `stdio` and unknown transport types are a
/// stated gap (`unsupported_tool`), never a misleading `server_url` report. Nothing is filtered by name, URL or
/// credential policy: only shapes that cannot be represented as an
/// [`McpToolSpec`] are rejected, and every rejection names its field path.
/// Non-MCP tool types are an implementation gap reported as `unsupported_tool`,
/// never silently dropped.
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

    let transport = match body.transport.as_ref() {
        None | Some(Value::Null) => None,
        Some(value) => Some(official_transport(&path, value)?),
    };
    let server_url = match &transport {
        None => required_tool_string(body.server_url.as_ref(), &format!("{path}.server_url"))?,
        Some(transport) => {
            let legacy =
                optional_tool_string(body.server_url.as_ref(), &format!("{path}.server_url"))?;
            if let Some(legacy) = legacy {
                if legacy != transport.server_url {
                    return Err(invalid_tool(
                        format!("{path}.transport"),
                        "top-level `server_url` and `transport.server_url` disagree",
                    ));
                }
            }
            transport.server_url.clone()
        }
    };
    // The official `transport.headers` object wins when both shapes supply
    // headers; the flat `headers` field is the deprecated alias.
    let mut headers = match transport
        .as_ref()
        .and_then(|transport| transport.headers.as_ref())
    {
        Some(headers) => normalize_headers(Some(headers), &format!("{path}.transport.headers"))?,
        None => normalize_headers(body.headers.as_ref(), &format!("{path}.headers"))?,
    };
    let mut spec = McpToolSpec::new(
        required_tool_string(body.server_label.as_ref(), &format!("{path}.server_label"))?,
        server_url,
    );
    if let Some(authorization) = transport.and_then(|transport| transport.authorization) {
        let has_authorization = headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("authorization"));
        if !has_authorization {
            headers.push(McpHeader {
                name: "Authorization".to_owned(),
                value: authorization_header_value(&authorization),
            });
            headers.sort_by(|left, right| left.name.cmp(&right.name));
        }
    }
    spec.headers = headers;
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

/// One official `McpTransportParam` entry (`agents.d.ts:3726`) after
/// validation. Only `type: "http"` maps onto an [`McpToolSpec`]; this
/// deployment has no process runtime for `stdio`.
struct OfficialTransport {
    server_url: String,
    headers: Option<Value>,
    authorization: Option<String>,
}

fn official_transport(path: &str, value: &Value) -> Result<OfficialTransport, SwarmError> {
    let Some(object) = value.as_object() else {
        return Err(invalid_tool(
            format!("{path}.transport"),
            "transport must be an object in the OpenAI Agents API shape",
        ));
    };
    let transport_path = format!("{path}.transport");
    let kind = match object.get("type") {
        Some(Value::String(kind)) => kind.as_str(),
        Some(_) => {
            return Err(invalid_tool(
                format!("{transport_path}.type"),
                "transport type must be a string",
            ));
        }
        None => {
            return Err(invalid_tool(
                format!("{transport_path}.type"),
                "transport must declare `type`",
            ));
        }
    };
    // `stdio` is part of the official union but cannot be executed here; an
    // unknown transport type is equally explicit instead of a misleading
    // `server_url` report.
    if kind != "http" {
        return Err(SwarmError::unsupported_mcp_transport(
            kind,
            &format!("{transport_path}.type"),
        ));
    }
    let server_url_path = format!("{transport_path}.server_url");
    let server_url = match optional_tool_string(object.get("server_url"), &server_url_path)? {
        Some(url) => url,
        None => {
            return Err(invalid_tool(
                server_url_path,
                "field is required by the MCP transport shape",
            ));
        }
    };
    Ok(OfficialTransport {
        server_url,
        headers: match object.get("headers") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.clone()),
        },
        authorization: optional_tool_string(
            object.get("authorization"),
            &format!("{transport_path}.authorization"),
        )?,
    })
}

fn required_tool_string(value: Option<&Value>, field: &str) -> Result<String, SwarmError> {
    optional_tool_string(value, field)?
        .ok_or_else(|| invalid_tool(field, "field is required by the MCP tool shape"))
}

fn optional_tool_string(value: Option<&Value>, field: &str) -> Result<Option<String>, SwarmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(text) = value.as_str() else {
        return Err(invalid_tool(field, "field must be a string"));
    };
    if text.trim().is_empty() {
        return Err(invalid_tool(field, "field must not be empty"));
    }
    Ok(Some(text.to_owned()))
}

/// Build the `Authorization` header value from official
/// `transport.authorization`.
///
/// Official clients send both the bare token (Responses API: "provide the
/// token here") and a complete credential (`"Bearer token"`, `"Basic ..."`;
/// Agents API: "the authorization value sent to the MCP server"), so the
/// `Bearer` prefix is only added when no auth scheme is present.
fn authorization_header_value(authorization: &str) -> String {
    if has_auth_scheme_prefix(authorization) {
        authorization.to_owned()
    } else {
        format!("Bearer {authorization}")
    }
}

/// Whether `value` starts with an HTTP auth scheme token (RFC 7235) followed
/// by whitespace, e.g. `Bearer ` or `Basic `. Equivalent to the anchored
/// pattern `^[A-Za-z][A-Za-z0-9!#$%&'*+.^_`|~-]*\s+`.
fn has_auth_scheme_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let is_scheme_char = byte.is_ascii_alphabetic()
            || (index > 0
                && (byte.is_ascii_digit()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                            | b'-'
                    )));
        if !is_scheme_char {
            break;
        }
        index += 1;
    }
    index > 0
        && bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_whitespace())
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

pub fn resolve_environment(
    body: &CreateSessionBody,
) -> Result<(EnvironmentKind, Option<String>), SwarmError> {
    let Some(environment) = &body.environment else {
        return Ok((EnvironmentKind::None, None));
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
            Ok((EnvironmentKind::None, None))
        }
        // S8a3a: hosted environments reserve a Bun workspace. A missing bridge
        // or an unbound environment id is diagnosed by the service, not here.
        "openai_hosted" => Ok((
            EnvironmentKind::OpenAiHosted,
            environment.environment_id.clone(),
        )),
        other => Err(SwarmError::unsupported_environment(other)),
    }
}

/// Resolve `POST /events` into either a turn input or an approval answer.
pub fn resolve_event(
    body: SubmitEventBody,
    headers: &HeaderMap,
) -> Result<SubmitEvent, SwarmError> {
    let Some(events) = body.events.as_ref() else {
        return resolve_submission(body, headers).map(SubmitEvent::Input);
    };
    let Some(items) = events.as_array() else {
        return Err(official_invalid(
            "events",
            "events must be an array containing exactly one event",
        ));
    };
    let approval = items.len() == 1
        && items[0].get("type").and_then(Value::as_str) == Some(OFFICIAL_APPROVAL_RESULT_EVENT);
    if !approval {
        return resolve_official_envelope(events, &body, headers).map(SubmitEvent::Input);
    }
    if let Some(field) = native_field(&body) {
        return Err(official_invalid(
            field,
            "`events` cannot be combined with native submission fields",
        ));
    }
    let entry: OfficialApprovalResultBody = serde_json::from_value(items[0].clone())
        .map_err(|error| approval_shape_error(&items[0], error))?;
    if entry.kind != OFFICIAL_APPROVAL_RESULT_EVENT {
        return Err(official_invalid(
            "events[0].type",
            "only one approval result event is supported",
        ));
    }
    let request_id = entry
        .request_id
        .as_ref()
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            official_invalid(
                "events[0].request_id",
                "the approval result must name the pending `request_id`",
            )
        })?
        .to_owned();
    let decision = match entry.response {
        Some(response) => {
            // `response.type` is the official discriminant for an MCP tool
            // call. Accepting another value would answer a different action
            // kind that this gateway never projected.
            if response.kind.as_ref().and_then(Value::as_str) != Some(crate::domain::MCP_TOOL_CALL)
            {
                return Err(official_invalid(
                    "events[0].response.type",
                    "`response.type` must be `mcp_tool_call`",
                ));
            }
            match (response.decision.as_ref(), entry.decision.as_ref()) {
                (Some(nested), Some(top)) if nested != top => Err(official_invalid(
                    "events[0].decision",
                    "top-level `decision` conflicts with `response.decision`",
                )),
                (Some(nested), _) => approval_decision(nested, "events[0].response.decision"),
                (None, Some(top)) => approval_decision(top, "events[0].decision"),
                (None, None) => Err(official_invalid(
                    "events[0].response.decision",
                    "the approval result must carry `response.decision`: approve, deny or cancel",
                )),
            }?
        }
        None => {
            let value = entry.decision.as_ref().ok_or_else(|| {
                official_invalid(
                    "events[0].response",
                    "the approval result must carry `response` with `type` and `decision`",
                )
            })?;
            approval_decision(value, "events[0].decision")?
        }
    };
    let input_id = idempotency_key(headers)?;
    if let Some(input_id) = input_id.as_deref() {
        if Uuid::parse_str(input_id).is_err() {
            return Err(SwarmError::invalid_input("Idempotency-Key must be a UUID"));
        }
    }
    Ok(SubmitEvent::Approval(ApprovalAnswer {
        request_id,
        decision,
        input_id,
    }))
}

pub fn resolve_submission(
    body: SubmitEventBody,
    headers: &HeaderMap,
) -> Result<InputSubmission, SwarmError> {
    if let Some(events) = body.events.as_ref() {
        return resolve_official_envelope(events, &body, headers);
    }
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
    let attachments = body.attachments.unwrap_or_default();
    for block in &attachments {
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
        attachments,
        plugin_selections: body.plugin_selections.unwrap_or_default(),
        original_draft: body.original_draft,
    };
    if submission.is_empty() {
        return Err(SwarmError::empty_input());
    }
    Ok(submission)
}

/// Parse the official `{events: [...]}` envelope into the existing acceptance
/// semantics: exactly one `agent.session.input.message` event carrying exactly
/// one user message, plain `input_text` content, accepted as one `turn` input.
/// Unsupported official features are reported explicitly, never dropped.
fn resolve_official_envelope(
    events: &Value,
    body: &SubmitEventBody,
    headers: &HeaderMap,
) -> Result<InputSubmission, SwarmError> {
    if let Some(field) = native_field(body) {
        return Err(official_invalid(
            field,
            "`events` cannot be combined with native submission fields",
        ));
    }
    let Some(items) = events.as_array() else {
        return Err(official_invalid(
            "events",
            "events must be an array containing exactly one `agent.session.input.message`",
        ));
    };
    if items.len() != 1 {
        return Err(official_invalid(
            "events",
            "only one event is supported; send a single `agent.session.input.message`",
        ));
    }

    let event: OfficialEventBody = serde_json::from_value(items[0].clone())
        .map_err(|error| official_event_shape_error(&items[0], error))?;
    if event.kind != OFFICIAL_INPUT_EVENT {
        return Err(official_invalid(
            "events[0].type",
            "only the `agent.session.input.message` event is supported",
        ));
    }
    let Some(input) = event.input else {
        return Err(official_invalid(
            "events[0].input",
            "the message event must carry `input` messages",
        ));
    };
    let Some(messages) = input.as_array() else {
        return Err(official_invalid(
            "events[0].input",
            "input must be an array containing exactly one user message",
        ));
    };
    if messages.len() != 1 {
        return Err(official_invalid(
            "events[0].input",
            "exactly one user message is supported per event",
        ));
    }

    let message: OfficialInputMessageBody =
        serde_json::from_value(messages[0].clone()).map_err(|error| {
            official_invalid(
                "events[0].input[0]",
                &format!("unsupported input message shape: {error}"),
            )
        })?;
    if let Some(kind) = message.kind.as_deref() {
        if kind != "message" {
            return Err(official_invalid(
                "events[0].input[0].type",
                "only `message` input entries are supported",
            ));
        }
    }
    match message.role.as_deref() {
        Some("user") => {}
        Some(_) => {
            return Err(official_invalid(
                "events[0].input[0].role",
                "only a `user` message is supported",
            ));
        }
        None => {
            return Err(official_invalid(
                "events[0].input[0].role",
                "the input message must declare `role: \"user\"`",
            ));
        }
    }
    let Some(content) = message.content else {
        return Err(official_invalid(
            "events[0].input[0].content",
            "the user message must carry `content`",
        ));
    };
    let Some(blocks) = content.as_array() else {
        return Err(official_invalid(
            "events[0].input[0].content",
            "content must be an array of `input_text` blocks",
        ));
    };

    let mut texts = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        let path = format!("events[0].input[0].content[{index}]");
        let block: OfficialContentBlockBody =
            serde_json::from_value(block.clone()).map_err(|error| {
                official_invalid(
                    path.clone(),
                    &format!("unsupported content block shape: {error}"),
                )
            })?;
        let Some(kind) = block.kind.as_deref() else {
            return Err(official_invalid(
                format!("{path}.type"),
                "content block must declare `type`",
            ));
        };
        if kind != "input_text" {
            return Err(official_invalid(
                path,
                "only `input_text` content blocks are supported; images and other content types are not accepted",
            ));
        }
        if let Some(extra) = block.extra.keys().next() {
            return Err(official_invalid(
                format!("{path}.{extra}"),
                "this content field is not supported by `input_text`",
            ));
        }
        let Some(text) = block.text else {
            return Err(official_invalid(
                format!("{path}.text"),
                "`input_text` content must declare `text`",
            ));
        };
        texts.push(text);
    }

    // Same join and size/blank checks as the native `input` path; an all-blank
    // envelope therefore ends at the existing `empty_input`.
    let text = texts.join("\n");
    check_len("text", &text, MAX_TEXT_BYTES)?;
    let input_id = match idempotency_key(headers)? {
        Some(input_id) => input_id,
        // Compatibility carve-out: the official envelope has no body inputId,
        // and the official API does not promise retry idempotency without a
        // caller key. The native path still never generates one.
        None => Uuid::now_v7().to_string(),
    };
    if Uuid::parse_str(&input_id).is_err() {
        return Err(SwarmError::invalid_input("inputId must be a UUID"));
    }

    let submission = InputSubmission {
        input_id,
        kind: InputKind::User,
        mode: InputMode::Turn,
        text: Some(text),
        attachments: Vec::new(),
        plugin_selections: Vec::new(),
        original_draft: None,
    };
    if submission.is_empty() {
        return Err(SwarmError::empty_input());
    }
    Ok(submission)
}

fn native_field(body: &SubmitEventBody) -> Option<&'static str> {
    if body.input_id.is_some() {
        Some("inputId")
    } else if body.kind.is_some() {
        Some("kind")
    } else if body.mode.is_some() {
        Some("mode")
    } else if body.text.is_some() {
        Some("text")
    } else if body.attachments.is_some() {
        Some("attachments")
    } else if body.plugin_selections.is_some() {
        Some("pluginSelections")
    } else if body.original_draft.is_some() {
        Some("originalDraft")
    } else {
        None
    }
}

fn official_invalid(field: impl Into<String>, message: &str) -> SwarmError {
    SwarmError::invalid_input(message).with_details(json!({ "field": field.into() }))
}

/// `OfficialEventBody` keeps `deny_unknown_fields`; this maps its rejection to
/// the event path so an unsupported official field or a non-string `type` is
/// diagnosable instead of surfacing as a generic body error.
fn official_event_shape_error(value: &Value, error: serde_json::Error) -> SwarmError {
    let Some(object) = value.as_object() else {
        return official_invalid("events[0]", "each event must be an object");
    };
    if !object.get("type").is_some_and(Value::is_string) {
        return official_invalid("events[0].type", "event must declare a string `type`");
    }
    if let Some(extra) = object
        .keys()
        .find(|key| key.as_str() != "type" && key.as_str() != "input")
    {
        return official_invalid(
            format!("events[0].{extra}"),
            "this event field is not supported by the single-message compatibility path",
        );
    }
    official_invalid("events[0]", &format!("unsupported event shape: {error}"))
}

/// Map an approval-result deserialization failure to the exact offending field.
fn approval_shape_error(value: &Value, error: serde_json::Error) -> SwarmError {
    let Some(object) = value.as_object() else {
        return official_invalid("events[0]", "each event must be an object");
    };
    if !object.get("type").is_some_and(Value::is_string) {
        return official_invalid("events[0].type", "event must declare a string `type`");
    }
    if let Some(response) = object.get("response") {
        let Some(response) = response.as_object() else {
            return official_invalid("events[0].response", "`response` must be an object");
        };
        if let Some(extra) = response
            .keys()
            .find(|key| !matches!(key.as_str(), "type" | "decision"))
        {
            return official_invalid(
                format!("events[0].response.{extra}"),
                "this approval response field is not supported",
            );
        }
    }
    if let Some(extra) = object.keys().find(|key| {
        !matches!(
            key.as_str(),
            "type" | "request_id" | "requestId" | "approval_request_id" | "response" | "decision"
        )
    }) {
        return official_invalid(
            format!("events[0].{extra}"),
            "this approval result field is not supported",
        );
    }
    official_invalid(
        "events[0]",
        &format!("unsupported approval result shape: {error}"),
    )
}

/// Read one official `decision` string at its exact field path.
fn approval_decision(
    value: &Value,
    field: &str,
) -> Result<crate::domain::ApprovalDecision, SwarmError> {
    let decision = value
        .as_str()
        .ok_or_else(|| official_invalid(field, "`decision` must be a string"))?;
    crate::domain::ApprovalDecision::parse(decision)
        .ok_or_else(|| official_invalid(field, "decision must be `approve`, `deny` or `cancel`"))
}

/// Starting point of an SSE subscription.
///
/// No cursor at all means [`EventCursor::Tail`]: the official SDK subscribes
/// before it submits the input and reads only what happens next. An explicit
/// `afterSeq`/`after`/`Last-Event-ID` (including `0`) replays the backlog.
pub fn resolve_events_cursor(
    query: &EventsQuery,
    headers: &HeaderMap,
) -> Result<EventCursor, SwarmError> {
    let last_event_id = last_event_id(headers)?;
    Cursor::reconcile_events(query.after_seq, query.after, last_event_id)
}

pub fn resolve_items_window(query: &ItemsQuery) -> Result<ItemsWindow, SwarmError> {
    let after_text = query.after.as_deref();
    let numeric_after = match after_text {
        // The official `CursorPage` sends the last item's `id`, never a seq.
        Some(value) if value.starts_with("itim_") => None,
        Some(value) => Some(value.parse::<u64>().map_err(|_| {
            SwarmError::invalid_query("after must be a decimal seq or an `itim_` item id")
                .with_details(json!({ "field": "after", "received": value }))
        })?),
        None => None,
    };
    let after = Cursor::reconcile_optional(query.after_seq, numeric_after)?;
    let after_id = after_text
        .filter(|value| value.starts_with("itim_"))
        .map(str::to_owned);
    let limit = query.limit.unwrap_or(ItemsWindow::DEFAULT_LIMIT);
    if limit == 0 || limit > ItemsWindow::MAX_LIMIT {
        return Err(SwarmError::invalid_query("limit must be within 1..=500")
            .with_details(json!({ "field": "limit", "received": limit })));
    }
    let order = match query.order.as_deref() {
        Some(value) => ItemOrder::parse(value)?,
        // `ItemListParams.order` defaults to `desc`; ascending pages must ask
        // for `order=asc` explicitly.
        None => ItemOrder::Desc,
    };
    Ok(ItemsWindow {
        order,
        after,
        after_id,
        limit,
    })
}

/// Validate the official `metadata` string map (create and update).
pub fn resolve_metadata(
    value: &Value,
    field: &str,
) -> Result<BTreeMap<String, String>, SwarmError> {
    let Value::Object(map) = value else {
        return Err(metadata_invalid(
            field,
            "metadata must be an object of string values",
        ));
    };
    if map.len() > MAX_METADATA_PAIRS {
        return Err(metadata_invalid(
            field,
            &format!("metadata must have at most {MAX_METADATA_PAIRS} pairs"),
        ));
    }
    let mut metadata = BTreeMap::new();
    for (key, value) in map {
        let path = format!("{field}.{key}");
        if key.is_empty() || key.chars().count() > MAX_METADATA_KEY_CHARS {
            return Err(metadata_invalid(
                &path,
                &format!("metadata keys must be 1..={MAX_METADATA_KEY_CHARS} characters"),
            ));
        }
        let Some(text) = value.as_str() else {
            return Err(metadata_invalid(&path, "metadata values must be strings"));
        };
        if text.chars().count() > MAX_METADATA_VALUE_CHARS {
            return Err(metadata_invalid(
                &path,
                &format!("metadata values must be at most {MAX_METADATA_VALUE_CHARS} characters"),
            ));
        }
        metadata.insert(key.clone(), text.to_owned());
    }
    Ok(metadata)
}

fn metadata_invalid(field: &str, message: &str) -> SwarmError {
    SwarmError::invalid_input(message).with_details(json!({ "field": field }))
}

/// Official `Agent.model` is a string; an explicit `null` is a request error,
/// not "keep the current model".
fn agent_model_not_string() -> SwarmError {
    SwarmError::new(
        ErrorKind::BadRequest,
        ErrorCode::InvalidRequest,
        "agent.model must be a string",
    )
    .with_details(json!({ "field": "agent.model" }))
}

/// Parse `prompt_cache_options`. `null` means "clear"; an object without `ttl`
/// uses the official `PromptCacheOptions.ttl` default of `30m`. Omission is
/// handled by the caller's [`UpdateField::Keep`].
pub fn resolve_prompt_cache_options(
    value: &Value,
) -> Result<Option<PromptCacheOptions>, SwarmError> {
    if value.is_null() {
        return Ok(None);
    }
    let Value::Object(map) = value else {
        return Err(metadata_invalid(
            "prompt_cache_options",
            "prompt_cache_options must be an object",
        ));
    };
    for key in map.keys() {
        if key != "ttl" {
            return Err(metadata_invalid(
                &format!("prompt_cache_options.{key}"),
                "only `ttl` is supported in prompt_cache_options",
            ));
        }
    }
    let ttl = match map.get("ttl") {
        // Official `PromptCacheOptions.ttl` defaults to 30 minutes.
        None => PromptCacheTtl::ThirtyMinutes,
        Some(ttl) => {
            let Some(ttl) = ttl.as_str() else {
                return Err(metadata_invalid(
                    "prompt_cache_options.ttl",
                    "ttl must be a string",
                ));
            };
            let Some(ttl) = PromptCacheTtl::parse(ttl) else {
                return Err(metadata_invalid(
                    "prompt_cache_options.ttl",
                    "ttl must be `30m` or `12h`",
                ));
            };
            ttl
        }
    };
    Ok(Some(PromptCacheOptions::new(ttl)))
}

/// `GET /v1/agents/sessions` query.
pub fn resolve_sessions_window(query: &SessionsQuery) -> Result<SessionsWindow, SwarmError> {
    let mut after = None;
    let mut limit = None;
    let mut order = None;
    let mut agent_id = None;
    let mut metadata = BTreeMap::new();
    for (name, value) in query {
        match name.as_str() {
            "after" => after = Some(value.clone()),
            "limit" => {
                limit = Some(value.parse::<u32>().map_err(|_| {
                    SwarmError::invalid_query("limit must be a positive integer")
                        .with_details(json!({ "field": "limit", "received": value }))
                })?);
            }
            "order" => order = Some(value.clone()),
            "agent_id" => agent_id = Some(value.clone()),
            other => {
                let Some(key) = other
                    .strip_prefix("metadata[")
                    .and_then(|rest| rest.strip_suffix(']'))
                else {
                    return Err(SwarmError::invalid_query(format!(
                        "unknown query parameter `{other}`"
                    ))
                    .with_details(json!({ "field": other })));
                };
                metadata.insert(key.to_owned(), value.clone());
            }
        }
    }
    if metadata.len() > MAX_METADATA_PAIRS {
        return Err(SwarmError::invalid_query(format!(
            "metadata must have at most {MAX_METADATA_PAIRS} pairs"
        ))
        .with_details(json!({ "field": "metadata" })));
    }
    for (key, value) in &metadata {
        if key.is_empty() || key.chars().count() > MAX_METADATA_KEY_CHARS {
            return Err(SwarmError::invalid_query(format!(
                "metadata keys must be 1..={MAX_METADATA_KEY_CHARS} characters"
            ))
            .with_details(json!({ "field": format!("metadata[{key}]") })));
        }
        if value.chars().count() > MAX_METADATA_VALUE_CHARS {
            return Err(SwarmError::invalid_query(format!(
                "metadata values must be at most {MAX_METADATA_VALUE_CHARS} characters"
            ))
            .with_details(json!({ "field": format!("metadata[{key}]") })));
        }
    }
    let limit = limit.unwrap_or(SessionsWindow::DEFAULT_LIMIT);
    if limit == 0 || limit > SessionsWindow::MAX_LIMIT {
        return Err(SwarmError::invalid_query(format!(
            "limit must be within 1..={}",
            SessionsWindow::MAX_LIMIT
        ))
        .with_details(json!({ "field": "limit", "received": limit })));
    }
    let order = match order.as_deref() {
        Some(value) => ItemOrder::parse(value)?,
        None => ItemOrder::Desc,
    };
    Ok(SessionsWindow {
        order,
        after,
        limit,
        agent_id,
        metadata,
    })
}

/// `GET /v1/agents/sessions/{id}/turns` query. The official default is `desc`.
pub fn resolve_turns_window(query: &TurnsQuery) -> Result<TurnsWindow, SwarmError> {
    let limit = query.limit.unwrap_or(TurnsWindow::DEFAULT_LIMIT);
    if limit == 0 || limit > TurnsWindow::MAX_LIMIT {
        return Err(SwarmError::invalid_query(format!(
            "limit must be within 1..={}",
            TurnsWindow::MAX_LIMIT
        ))
        .with_details(json!({ "field": "limit", "received": limit })));
    }
    let order = match query.order.as_deref() {
        Some(value) => ItemOrder::parse(value)?,
        None => ItemOrder::Desc,
    };
    Ok(TurnsWindow {
        order,
        after: query.after.clone(),
        limit,
    })
}

/// Validate a `tools` array supplied to `sessions.update` (same normalizer as
/// create; `null`/`[]` clears the declaration).
pub fn resolve_tools_value(value: &Value, field: &str) -> Result<Vec<McpToolSpec>, SwarmError> {
    let declared = declared_tools(Some(value), field)?;
    declared
        .iter()
        .enumerate()
        .map(|(index, value)| normalize_tool(field, index, value))
        .collect()
}

fn unsupported_official_field(field: &str) -> SwarmError {
    SwarmError::new(
        ErrorKind::BadRequest,
        ErrorCode::InvalidRequest,
        format!("official field `{field}` is not supported by this gateway"),
    )
    .with_details(json!({ "field": field, "supported": false }))
}

/// Decide whether a `sessions.update` request carries a body at all.
///
/// Official `update(sessionID, body?: SessionUpdateParams | null | undefined)`
/// (`sessions.d.ts:76`) lets the caller omit the body: the SDK then sends `{}`
/// with `content-type: application/json`, while an explicit `null` sends no
/// body and no `content-type`. Either spelling, plus an empty byte stream,
/// means "no update", so all return `None` (a no-op).
///
/// Only a non-empty body is held to the media-type and syntax rules: a
/// non-JSON content-type is `415 invalid_content_type`, and malformed JSON is
/// `400 invalid_request`. A rejection is never silently downgraded to a
/// no-op, which is why `Option<Json<T>>` (whose rejection covers both) is not
/// used here.
pub fn parse_update_body(
    bytes: &[u8],
    headers: &HeaderMap,
) -> Result<Option<UpdateSessionBody>, SwarmError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    if !has_json_content_type(headers) {
        return Err(SwarmError::new(
            ErrorKind::UnsupportedMediaType,
            ErrorCode::InvalidContentType,
            "content-type must be application/json",
        ));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        SwarmError::new(
            ErrorKind::BadRequest,
            ErrorCode::InvalidRequest,
            format!("failed to parse the JSON body: {error}"),
        )
    })?;
    if value.is_null() {
        return Ok(None);
    }
    // The official SDK's default `update(id)` call sends an empty JSON object;
    // it carries no update fields and must not enter admission or write a row.
    if value.as_object().is_some_and(|object| object.is_empty()) {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some).map_err(|error| {
        let message = format!("failed to deserialize the update body: {error}");
        if message.contains("unknown field") {
            SwarmError::new(ErrorKind::BadRequest, ErrorCode::UnknownField, message)
        } else {
            SwarmError::new(ErrorKind::BadRequest, ErrorCode::InvalidRequest, message)
        }
    })
}

/// Same JSON media types axum's `Json` extractor accepts (`application/json`,
/// `application/*+json`), so reading the raw bytes does not narrow the
/// contract.
fn has_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(axum::http::header::CONTENT_TYPE) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some((top, sub)) = value.split(';').next().unwrap_or_default().split_once('/') else {
        return false;
    };
    top.trim().eq_ignore_ascii_case("application")
        && (sub.trim().eq_ignore_ascii_case("json")
            || sub.trim().to_ascii_lowercase().ends_with("+json"))
}

/// `POST /v1/agents/sessions/{id}` body.
pub fn resolve_update(body: UpdateSessionBody) -> Result<SessionUpdateCommand, SwarmError> {
    let agent = body.agent;
    if !body.review.is_keep() {
        return Err(unsupported_official_field("review"));
    }
    if let Some(agent) = agent.as_ref() {
        if !agent.reasoning.is_keep() {
            return Err(unsupported_official_field("agent.reasoning"));
        }
        if !agent.service_tier.is_keep() {
            return Err(unsupported_official_field("agent.service_tier"));
        }
    }
    let model = match agent.as_ref().map(|agent| &agent.model) {
        None | Some(UpdateField::Keep) => None,
        Some(UpdateField::Clear) => return Err(agent_model_not_string()),
        Some(UpdateField::Set(model)) => {
            if model.trim().is_empty() {
                return Err(metadata_invalid("agent.model", "model must not be empty"));
            }
            check_len("agent.model", model, MAX_MODEL_BYTES)?;
            Some(model.clone())
        }
    };
    let mcp_tools = match agent.as_ref().map(|agent| &agent.tools) {
        None | Some(UpdateField::Keep) => None,
        Some(UpdateField::Clear) => Some(Vec::new()),
        Some(UpdateField::Set(value)) => Some(resolve_tools_value(value, "agent.tools")?),
    };
    let metadata = match body.metadata {
        UpdateField::Keep => UpdateField::Keep,
        UpdateField::Clear => UpdateField::Clear,
        UpdateField::Set(value) => UpdateField::Set(resolve_metadata(&value, "metadata")?),
    };
    let prompt_cache_options = match body.prompt_cache_options {
        UpdateField::Keep => UpdateField::Keep,
        UpdateField::Clear => UpdateField::Clear,
        UpdateField::Set(value) => match resolve_prompt_cache_options(&value)? {
            Some(options) => UpdateField::Set(options),
            // Only `null` resolves to `None`; `{}` resolves to the official
            // default TTL (`30m`).
            None => UpdateField::Clear,
        },
    };
    Ok(SessionUpdateCommand {
        model,
        mcp_tools,
        metadata,
        prompt_cache_options,
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
