//! Reverse-request wire parsing and atomic owner/payload envelopes.

use crate::kit::acp_notifier::BridgeSink;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch, PendingInteraction};

use serde_json::Value;
use tracing::{info, warn};

/// 处理 Elicitation 通知：原子封装 RequestId + AskUser 后推入 bridge。
pub(super) async fn handle_elicitation(
    owner: crate::acp_client::InteractionOwner,
    request_id_json: String,
    params: &Value,
    sink: &BridgeSink,
) -> bool {
    // 从 params 中提取 session_id
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (questions, elicitation) = if let Some(marker) = mcp_elicitation_marker(params) {
        parse_mcp_elicitation(params, marker)
    } else {
        (parse_elicitation_questions(params), None)
    };
    let ask_user = AskUser {
        questions,
        elicitation,
    };
    let event = AcpEventData::AskUser(PendingInteraction {
        owner,
        request_id_json,
        payload: ask_user,
    });
    let wrapped = AcpEventWithEpoch {
        child_origin: None,
        view_epoch: crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
        event,
        active_session_id: session_id,
    };

    info!("kit ACP notifier: forwarding Elicitation as AskUser event");

    sink.send(wrapped).await.map_or_else(
        |e| {
            warn!(error = %e, "kit ACP notifier: bridge closed, rejecting AskUser");
            false
        },
        |_| true,
    )
}

/// 处理 RequestPermission：原子封装 RequestId + HITL payload 后推入 bridge。
///
/// JSON 结构（CreatePermissionRequest ACP schema）:
/// ```json
/// {"sessionId": "sess_1", "toolCall": {"title": "Bash", "rawInput": {...}},
///  "options": [{"id": "allow_once", ...}, ...]}
/// ```
pub(super) async fn handle_request_permission(
    owner: crate::acp_client::InteractionOwner,
    request_id_json: String,
    params: &Value,
    sink: &BridgeSink,
) -> bool {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 从 params.toolCall 提取 tool_name + tool_input
    let tool_call = params.get("toolCall").unwrap_or(&Value::Null);
    let tool_name = tool_call
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let tool_input = tool_call.get("rawInput").cloned().unwrap_or(Value::Null);
    let hp = HitlPending {
        tool_name,
        tool_input,
        batch: None,
    };

    let event = AcpEventData::HitlPending(PendingInteraction {
        owner,
        request_id_json,
        payload: hp,
    });
    let wrapped = AcpEventWithEpoch {
        child_origin: None,
        view_epoch: crate::kit::atoms::BRIDGE_RESET_COUNTER.get(),
        event,
        active_session_id: session_id,
    };

    info!("kit ACP notifier: forwarding RequestPermission as HitlPending event");

    sink.send(wrapped).await.map_or_else(
        |e| {
            warn!(error = %e, "kit ACP notifier: bridge closed, rejecting HitlPending");
            false
        },
        |_| true,
    )
}

/// 外部 MCP elicitation 的 `_meta` 标记（peri-acp broker 写入）。
const MCP_ELICITATION_META_KEY: &str = "io.peri/mcpElicitation";

fn mcp_elicitation_marker(params: &Value) -> Option<(&str, &str)> {
    let marker = params.get("_meta")?.get(MCP_ELICITATION_META_KEY)?;
    Some((
        marker.get("server")?.as_str()?,
        marker.get("tool")?.as_str()?,
    ))
}

/// MCP form → TUI 字段；URL → 单个「打开」确认项。
///
/// 与内置 AskUserQuestion 的差别只在 `elicitation` 标记与 `field_type`：
/// 内置路径继续走原解析，不改断言。
fn parse_mcp_elicitation(
    params: &Value,
    (server, tool): (&str, &str),
) -> (Vec<Question>, Option<ElicitationPresentation>) {
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match params.get("mode").and_then(Value::as_str).unwrap_or("form") {
        "url" => {
            let url = params
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let elicitation_id = params
                .get("elicitationId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            (
                vec![Question {
                    id: "__mcp_url__".to_string(),
                    header: "URL".to_string(),
                    question: message.clone(),
                    options: Vec::new(),
                    multi_select: false,
                    field_type: Some("url".to_string()),
                    required: false,
                }],
                Some(ElicitationPresentation::Url {
                    server: server.to_string(),
                    tool: tool.to_string(),
                    message,
                    url,
                    elicitation_id,
                }),
            )
        }
        _ => {
            let required: std::collections::BTreeSet<String> = params
                .get("requestedSchema")
                .and_then(|schema| schema.get("required"))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let mut questions = Vec::new();
            if let Some(props) = params
                .get("requestedSchema")
                .and_then(|schema| schema.get("properties"))
                .and_then(Value::as_object)
            {
                for (id, prop) in props {
                    questions.push(mcp_form_question(id, prop, &required, &message));
                }
            }
            (
                questions,
                Some(ElicitationPresentation::Form {
                    server: server.to_string(),
                    tool: tool.to_string(),
                }),
            )
        }
    }
}

fn mcp_form_question(
    id: &str,
    prop: &Value,
    required: &std::collections::BTreeSet<String>,
    message: &str,
) -> Question {
    let header = prop
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty())
        .unwrap_or(id)
        .to_string();
    let question = prop
        .get("description")
        .and_then(Value::as_str)
        .filter(|description| !description.is_empty())
        .unwrap_or(message)
        .to_string();
    let field_type = prop
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string")
        .to_string();
    let is_required = required.contains(id);
    let mut options = Vec::new();
    let mut multi_select = false;
    match field_type.as_str() {
        "boolean" => {
            options.push(QuestionOption {
                label: "Yes".to_string(),
                description: String::new(),
                value: Some(Value::Bool(true)),
            });
            options.push(QuestionOption {
                label: "No".to_string(),
                description: String::new(),
                value: Some(Value::Bool(false)),
            });
        }
        "string" => {
            options = extract_enum_options(prop, prop.get("oneOf").or_else(|| prop.get("enum")));
        }
        "array" => {
            multi_select = true;
            options = prop
                .get("items")
                .and_then(|items| items.get("anyOf").or_else(|| items.get("enum")))
                .map(|source| enum_options_to_questions(source, true))
                .unwrap_or_default();
        }
        _ => {}
    }
    Question {
        id: id.to_string(),
        header,
        question,
        options,
        multi_select,
        field_type: Some(field_type),
        required: is_required,
    }
}

fn extract_enum_options(prop: &Value, source: Option<&Value>) -> Vec<QuestionOption> {
    source
        .map(|source| enum_options_to_questions(source, false))
        .unwrap_or_else(|| {
            prop.get("oneOf")
                .map(|source| enum_options_to_questions(source, false))
                .unwrap_or_default()
        })
}

fn enum_options_to_questions(source: &Value, values_are_strings: bool) -> Vec<QuestionOption> {
    let Some(options) = source.as_array() else {
        return Vec::new();
    };
    options
        .iter()
        .map(|option| {
            if values_are_strings {
                let value = option
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        option
                            .get("const")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                QuestionOption {
                    label: value.clone(),
                    description: String::new(),
                    value: Some(Value::String(value)),
                }
            } else {
                let value = option
                    .get("const")
                    .cloned()
                    .or_else(|| {
                        option
                            .as_str()
                            .map(|value| Value::String(value.to_string()))
                    })
                    .unwrap_or(Value::Null);
                let label = option
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| value.as_str().map(str::to_string).unwrap_or_default());
                QuestionOption {
                    label,
                    description: option
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    value: Some(value),
                }
            }
        })
        .collect()
}

/// 从 CreateElicitationRequest JSON 中解析问题列表。
///
/// JSON 结构（CreateElicitationRequest 序列化后，#[serde(flatten)] 展开）:
/// ```json
/// {"mode": "form", "sessionId": "sess_1", "message": "...",
///  "requestedSchema": {"type": "object", "properties": {
///   "q_id": {"type": "string", "title": "Header", "description": "Question text",
///            "oneOf": [{"const": "label", "title": "label"}]},
///   "multi_q_id": {"type": "array", "title": "...", "description": "...",
///                  "items": {"anyOf": [{"const": "label", "title": "..."}]}}
/// }}}
/// ```
///
/// 解析失败时返回空 Vec（弹窗显示 "0 questions"）。
pub(super) fn parse_elicitation_questions(params: &Value) -> Vec<Question> {
    let props = match params
        .get("requestedSchema")
        .and_then(|rs| rs.get("properties"))
        .and_then(|p| p.as_object())
    {
        Some(p) => p,
        None => {
            warn!("kit ACP notifier: elicitation params missing requestedSchema.properties");
            return vec![];
        }
    };

    props
        .iter()
        .map(|(id, prop)| {
            let header = prop
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let question = prop
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();

            let prop_type = prop.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match prop_type {
                "array" => {
                    // multi_select: options 在 items.anyOf
                    let options = extract_options_from_oneof(prop, "anyOf", true);
                    Question {
                        id: id.clone(),
                        question,
                        header,
                        options,
                        multi_select: true,
                        field_type: None,
                        required: false,
                    }
                }
                "string" => {
                    // single select: options 在 oneOf
                    let options = extract_options_from_oneof(prop, "oneOf", false);
                    Question {
                        id: id.clone(),
                        question,
                        header,
                        options,
                        multi_select: false,
                        field_type: None,
                        required: false,
                    }
                }
                _ => Question {
                    id: id.clone(),
                    question,
                    header,
                    options: vec![],
                    multi_select: false,
                    field_type: None,
                    required: false,
                },
            }
        })
        .collect()
}

/// 从 prop["items"][key] 或 prop[key] 中提取 QuestionOption 列表。
/// - `nested=true`：选项在 `prop["items"][key]`（multi_select / anyOf）
/// - `nested=false`：选项在 `prop[key]`（single_select / oneOf）
fn extract_options_from_oneof(prop: &Value, key: &str, nested: bool) -> Vec<QuestionOption> {
    let arr = if nested {
        prop.get("items").and_then(|items| items.get(key))
    } else {
        prop.get(key)
    }
    .and_then(|v| v.as_array());

    let Some(arr) = arr else {
        return vec![];
    };

    arr.iter()
        .map(|opt| QuestionOption {
            label: opt
                .get("const")
                .or_else(|| opt.get("title"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            description: opt
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            value: None,
        })
        .collect()
}

use peri_acp_types::event_data::{AskUser, ElicitationPresentation, Question, QuestionOption};

use peri_acp_types::event_data::HitlPending;
