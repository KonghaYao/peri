//! Reverse-request wire parsing and atomic owner/payload envelopes.

use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch, PendingInteraction};
use peri_acp_types::event_data::{AskUser, HitlPending, Question, QuestionOption};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// 处理 Elicitation 通知：原子封装 RequestId + AskUser 后推入 bridge。
pub(super) fn handle_elicitation(
    owner: crate::acp_client::InteractionOwner,
    request_id_json: String,
    params: &Value,
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
) -> bool {
    // 从 params 中提取 session_id
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let questions = parse_elicitation_questions(params);
    let ask_user = AskUser { questions };
    let event = AcpEventData::AskUser(PendingInteraction {
        owner,
        request_id_json,
        payload: ask_user,
    });
    let wrapped = AcpEventWithEpoch {
        event,
        active_session_id: session_id,
    };

    info!("kit ACP notifier: forwarding Elicitation as AskUser event");

    bridge_tx.send(wrapped).map_or_else(
        |e| {
            warn!(error = %e, "kit ACP notifier: bridge_tx closed, rejecting AskUser");
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
pub(super) fn handle_request_permission(
    owner: crate::acp_client::InteractionOwner,
    request_id_json: String,
    params: &Value,
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
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
        event,
        active_session_id: session_id,
    };

    info!("kit ACP notifier: forwarding RequestPermission as HitlPending event");

    bridge_tx.send(wrapped).map_or_else(
        |e| {
            warn!(error = %e, "kit ACP notifier: bridge_tx closed, rejecting HitlPending");
            false
        },
        |_| true,
    )
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
                    }
                }
                _ => Question {
                    id: id.clone(),
                    question,
                    header,
                    options: vec![],
                    multi_select: false,
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
        })
        .collect()
}
