//! ACPChannel 纯函数单测（设计稿 §16 测试 1–5）。

use serde_json::{Value, json};

use acp_hub_proto::schema::{BlockVisibility, TurnStatus};

use super::*;

fn ch() -> AcpChannel {
    AcpChannel::default()
}

fn norm(frame: Value) -> NormalizeOutcome {
    ch().normalize("hub-s1", 1, 7, "2026-08-07T00:00:00Z", &frame)
}

// ---------------------------------------------------------------------------
// 1. 双格式 sessionId 提取
// ---------------------------------------------------------------------------

#[test]
fn extract_raw_payload_session_id() {
    let f = json!({"type": "agent_message_chunk", "payload": {"sessionId": "acp-1"}});
    assert_eq!(extract_session_id(&f).as_deref(), Some("acp-1"));
}

#[test]
fn extract_raw_payload_session_id_snake() {
    let f = json!({"type": "agent_message_chunk", "payload": {"session_id": "acp-2"}});
    assert_eq!(extract_session_id(&f).as_deref(), Some("acp-2"));
}

#[test]
fn extract_raw_top_level_session_id() {
    let f = json!({"type": "agent_message_chunk", "sessionId": "acp-3"});
    assert_eq!(extract_session_id(&f).as_deref(), Some("acp-3"));
}

#[test]
fn extract_jsonrpc_params_session_id() {
    let f = json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {"sessionId": "acp-4", "type": "agent_message_chunk", "payload": {}}
    });
    assert_eq!(extract_session_id(&f).as_deref(), Some("acp-4"));
}

#[test]
fn extract_jsonrpc_params_session_id_snake() {
    let f = json!({
        "jsonrpc": "2.0", "method": "session/update",
        "params": {"session_id": "acp-5", "type": "agent_message_chunk", "payload": {}}
    });
    assert_eq!(extract_session_id(&f).as_deref(), Some("acp-5"));
}

#[test]
fn extract_jsonrpc_response_id_path() {
    let f = json!({"jsonrpc": "2.0", "id": "hub-1", "result": {"sessionId": "acp-6"}});
    // response 的 result.sessionId 不作为连接级 sessionId（§3.3 只规定
    // params 路径）；但 result 内 sessionId 供 create binding 解析。
    assert_eq!(extract_session_id(&f), None);
    let body = f["result"].as_object().unwrap();
    assert_eq!(super::field(body, &["sessionId"]).as_deref(), Some("acp-6"));
}

#[test]
fn extract_missing_session_id() {
    let f = json!({"type": "agent_message_chunk", "payload": {}});
    assert_eq!(extract_session_id(&f), None);
    assert!(matches!(
        norm(f),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
}

// ---------------------------------------------------------------------------
// 2. 事件映射全表（§6.1 14 行 + RpcResponse）
// ---------------------------------------------------------------------------

#[test]
fn map_message_delta() {
    let f = json!({
        "type": "agent_message_chunk",
        "payload": {"turnId": "t1", "entryId": "t1:assistant", "blockId": "b1", "text": "hi"}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => {
            assert_eq!(ev.chat_id, "hub-s1");
            assert_eq!(ev.seq, 7);
            assert_eq!(ev.epoch, 1);
            assert_eq!(
                ev.body,
                EventBody::MessageDelta {
                    turn_id: "t1".into(),
                    entry_id: "t1:assistant".into(),
                    block_id: "b1".into(),
                    text: "hi".into(),
                }
            );
        }
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_reasoning_delta_visibility() {
    let f = json!({
        "type": "agent_thought_chunk",
        "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "think", "visibility": "hidden"}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => {
            assert_eq!(ev.body.kind(), "reasoning_delta");
            match ev.body {
                EventBody::ReasoningDelta { visibility, .. } => {
                    assert_eq!(visibility, BlockVisibility::Hidden)
                }
                _ => panic!("expected reasoning delta"),
            }
        }
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_reasoning_alias() {
    // 任务描述 agent_reasoning_chunk 别名 → 同一 ReasoningDelta（§3.2 冲突裁决）。
    let f = json!({
        "type": "agent_reasoning_chunk",
        "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => assert_eq!(ev.body.kind(), "reasoning_delta"),
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_user_message() {
    let f = json!({
        "type": "user_message_chunk",
        "payload": {"turnId": "t1", "entryId": "t1:user", "text": "hello", "authorUserId": "me"}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::UserMessage {
                turn_id,
                entry_id,
                text,
                author_user_id,
                created_at,
            } => {
                assert_eq!(turn_id, "t1");
                assert_eq!(entry_id, "t1:user");
                assert_eq!(text, "hello");
                assert_eq!(author_user_id.as_deref(), Some("me"));
                assert_eq!(created_at, "2026-08-07T00:00:00Z");
            }
            _ => panic!("expected user message"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_turn_terminal_completed() {
    for kind in ["prompt_complete", "agent_message_complete"] {
        let f = json!({"type": kind, "payload": {"turnId": "t1"}});
        match norm(f) {
            NormalizeOutcome::Event(ev) => match ev.body {
                EventBody::TurnTerminal { status, .. } => {
                    assert_eq!(status, TurnStatus::Completed)
                }
                _ => panic!("expected turn terminal"),
            },
            other => panic!("expected event, got {other:?}"),
        }
    }
}

#[test]
fn map_session_error_failed() {
    let f = json!({
        "type": "session_error",
        "payload": {"turnId": "t1", "publicError": {"code": "AGENT_UNAVAILABLE", "message": "boom"}}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::TurnTerminal {
                status,
                public_error,
                ..
            } => {
                assert_eq!(status, TurnStatus::Failed);
                let pe = public_error.unwrap();
                assert_eq!(pe.code, "AGENT_UNAVAILABLE");
                assert_eq!(pe.message, "boom");
            }
            _ => panic!("expected turn terminal"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_tool_call_started() {
    let f = json!({
        "type": "tool_call",
        "payload": {"turnId": "t1", "toolCallId": "tc1", "name": "bash", "arguments": {"cmd": "ls"}}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::ToolCallStarted {
                tool_call_id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(tool_call_id, "tc1");
                assert_eq!(name, "bash");
                assert_eq!(arguments, Some(json!({"cmd": "ls"})));
            }
            _ => panic!("expected tool call started"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_tool_call_update_running() {
    let f = json!({
        "type": "tool_call_update",
        "payload": {"turnId": "t1", "toolCallId": "tc1", "status": "running", "arguments": {"x": 1}}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::ToolCallUpdated { arguments, .. } => {
                assert_eq!(arguments, Some(json!({"x": 1})))
            }
            _ => panic!("expected tool call updated"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_tool_call_update_completed() {
    let f = json!({
        "type": "tool_call_update",
        "payload": {"turnId": "t1", "toolCallId": "tc1", "status": "completed", "result": {"ok": true}}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::ToolCallCompleted { result, .. } => {
                assert_eq!(result, Some(json!({"ok": true})))
            }
            _ => panic!("expected tool call completed"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_permission_request_expires_at_injected() {
    let f = json!({
        "type": "permission_request",
        "payload": {"permissionId": "p1", "turnId": "t1", "title": "run cmd", "options": ["allow_once", "deny"]}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::PermissionRequested {
                permission_id,
                options,
                expires_at,
                ..
            } => {
                assert_eq!(permission_id, "p1");
                assert_eq!(options.len(), 2);
                // 注入时钟 + 5min（§4.7/§16）。
                let t = chrono::DateTime::parse_from_rfc3339(&expires_at).unwrap();
                assert_eq!(
                    t,
                    chrono::DateTime::parse_from_rfc3339("2026-08-07T00:05:00Z").unwrap()
                );
            }
            _ => panic!("expected permission requested"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_permission_response_allow() {
    let f = json!({"type": "permission_response", "payload": {"permissionId": "p1", "decision": "allow"}});
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::PermissionResolved { decision, .. } => {
                assert_eq!(decision, acp_hub_proto::action::PermissionDecision::Allow)
            }
            _ => panic!("expected permission resolved"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// #1 官方 session/request_permission（权限机制官方化）
// ---------------------------------------------------------------------------

#[test]
fn map_request_permission_official() {
    // 官方帧（id=5 number + toolCall + options）→ PermissionRequest：
    // request_id 原样（number 不得丢弃）、permission_id server 生成、
    // tool_call_id/title/options 透传。
    let f = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "session/request_permission",
        "params": {
            "sessionId": "acp-1",
            "toolCall": {"toolCallId": "tc1", "title": "run cmd"},
            "options": [
                {"optionId": "allow-once", "name": "允许一次", "kind": "allow_once"},
                {"optionId": "reject-once", "name": "拒绝一次", "kind": "reject_once"}
            ]
        }
    });
    match norm(f) {
        NormalizeOutcome::PermissionRequest(req) => {
            assert_eq!(req.request_id, json!(5), "agent request id 原样（number）");
            assert!(!req.permission_id.is_empty(), "permission_id 由 server 生成");
            assert_eq!(req.tool_call_id.as_deref(), Some("tc1"));
            assert_eq!(req.title, "run cmd");
            assert_eq!(req.description, None, "官方无 description 字段");
            assert_eq!(req.options.len(), 2, "官方 options 原样保留");
            assert_eq!(req.session_id, "acp-1");
        }
        other => panic!("expected permission request, got {other:?}"),
    }
    // string id 同样原样保留；title 回退 toolCallId。
    let s = json!({
        "jsonrpc": "2.0",
        "id": "req-7",
        "method": "session/request_permission",
        "params": {
            "sessionId": "acp-1",
            "toolCall": {"toolCallId": "tc2"},
            "options": [{"optionId": "o1", "name": "x", "kind": "allow_once"}]
        }
    });
    match norm(s) {
        NormalizeOutcome::PermissionRequest(req) => {
            assert_eq!(req.request_id, json!("req-7"));
            assert_eq!(req.title, "tc2", "title 回退 toolCall.toolCallId");
            assert_eq!(req.tool_call_id.as_deref(), Some("tc2"));
        }
        other => panic!("expected permission request, got {other:?}"),
    }
}

#[test]
fn request_permission_missing_fields_dropped() {
    // 缺 options / toolCallId / sessionId / option.optionId / id →
    // MissingField（§6.3 同源拒绝；无 id 无法回响应，非 notification）。
    let no_options = json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/request_permission",
        "params": {"sessionId": "acp-1", "toolCall": {"toolCallId": "tc1"}}
    });
    assert!(matches!(
        norm(no_options),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
    let no_tool_call_id = json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/request_permission",
        "params": {"sessionId": "acp-1", "toolCall": {"title": "x"},
                   "options": [{"optionId": "o1", "name": "n", "kind": "allow_once"}]}
    });
    assert!(matches!(
        norm(no_tool_call_id),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
    let no_session = json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/request_permission",
        "params": {"toolCall": {"toolCallId": "tc1"},
                   "options": [{"optionId": "o1", "name": "n", "kind": "allow_once"}]}
    });
    assert!(matches!(
        norm(no_session),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
    let bad_option = json!({
        "jsonrpc": "2.0", "id": 1, "method": "session/request_permission",
        "params": {"sessionId": "acp-1", "toolCall": {"toolCallId": "tc1"},
                   "options": [{"name": "n", "kind": "allow_once"}]}
    });
    assert!(matches!(
        norm(bad_option),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
    let no_id = json!({
        "jsonrpc": "2.0", "method": "session/request_permission",
        "params": {"sessionId": "acp-1", "toolCall": {"toolCallId": "tc1"},
                   "options": [{"optionId": "o1", "name": "n", "kind": "allow_once"}]}
    });
    assert!(matches!(
        norm(no_id),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
}

/// #2：官方 ToolCallStatus `failed` 终态映射（包裹格式）——同为
/// ToolCallCompleted + public_error（code=agent_error，message 缺省
/// "Tool call failed"）。
#[test]
fn map_tool_call_update_failed_terminal() {
    let f = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "acp-1",
            "update": {"sessionUpdate": "tool_call_update", "toolCallId": "tc1", "status": "failed"}
        }
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::ToolCallCompleted { result, public_error, .. } => {
                assert_eq!(result, None);
                let pe = public_error.unwrap();
                assert_eq!(pe.code, "agent_error");
                assert_eq!(pe.message, "Tool call failed");
            }
            _ => panic!("expected tool call completed"),
        },
        other => panic!("expected event, got {other:?}"),
    }
    // error 兼容别名回归（既有行为）。
    let g = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "acp-1",
            "update": {"sessionUpdate": "tool_call_update", "toolCallId": "tc2", "status": "error"}
        }
    });
    match norm(g) {
        NormalizeOutcome::Event(ev) => assert_eq!(ev.body.kind(), "tool_call_completed"),
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_session_update_partial() {
    let f = json!({"type": "session_update", "payload": {"title": "new title"}});
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::SessionInfo { title, status, .. } => {
                assert_eq!(title.as_deref(), Some("new title"));
                assert!(status.is_none());
            }
            _ => panic!("expected session info"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_capabilities() {
    let f = json!({"type": "available_commands_update", "payload": {"commands": ["bash", "read"]}});
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::Capabilities { capabilities } => {
                assert_eq!(capabilities, vec!["bash".to_string(), "read".to_string()])
            }
            _ => panic!("expected capabilities"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_agent_status_raw_and_jsonrpc() {
    let raw = json!({"type": "agent_status", "payload": {"status": "idle"}});
    match norm(raw) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::AgentStatus {
                status,
                model,
                context_window,
                context_used,
                ..
            } => {
                assert_eq!(status, "idle");
                // 缺省字段 → None（不覆盖 agent map，§6.3 部分更新）。
                assert_eq!(model, None);
                assert_eq!(context_window, None);
                assert_eq!(context_used, None);
            }
            _ => panic!("expected agent status"),
        },
        other => panic!("expected event, got {other:?}"),
    }
    let rpc = json!({
        "jsonrpc": "2.0",
        "method": "agent/status",
        "params": {
            "status": "busy",
            "model": "claude-sonnet-4-5",
            "contextWindow": 200000,
            "contextUsed": 42000,
        },
    });
    match norm(rpc) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::AgentStatus {
                status,
                model,
                context_window,
                context_used,
                ..
            } => {
                assert_eq!(status, "busy");
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-5"));
                assert_eq!(context_window, Some(200_000));
                assert_eq!(context_used, Some(42_000));
            }
            _ => panic!("expected agent status"),
        },
        other => panic!("expected event, got {other:?}"),
    }
    // snake_case 回退 + 负数/超上限 → None（缺省语义）。
    let snake = json!({
        "jsonrpc": "2.0",
        "method": "agent/status",
        "params": { "status": "error", "context_window": -1, "contextUsed": 99999999999u64 },
    });
    match norm(snake) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::AgentStatus {
                context_window,
                context_used,
                ..
            } => {
                assert_eq!(context_window, None, "负数 → None");
                assert_eq!(context_used, None, "超 u32 → None");
            }
            _ => panic!("expected agent status"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_config_option_update() {
    // config_option_update（跨任务契约）：model 从 options 匹配项的
    // name 提取括号内模型名；thinking_effort 取 currentValue。
    let f = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "acp-1",
            "update": {
                "sessionUpdate": "config_option_update",
                "configOptions": [
                    {"id": "model", "name": "Model", "type": "select", "currentValue": "default",
                     "options": [
                         {"value": "default", "name": "default (claude-sonnet-4-5)"},
                         {"value": "opus", "name": "opus (claude-opus-4-1)"}
                     ], "category": "model"},
                    {"id": "thinking_effort", "name": "Effort", "type": "select",
                     "currentValue": "high", "options": [], "category": "reasoning"}
                ]
            }
        }
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::AgentConfig { model, effort } => {
                assert_eq!(model.as_deref(), Some("claude-sonnet-4-5"));
                assert_eq!(effort.as_deref(), Some("high"));
            }
            _ => panic!("expected agent config"),
        },
        other => panic!("expected event, got {other:?}"),
    }
    // name 无括号 → 回退整个 name。
    let no_paren = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "acp-1",
            "update": {
                "sessionUpdate": "config_option_update",
                "configOptions": [
                    {"id": "model", "name": "Model", "type": "select", "currentValue": "default",
                     "options": [{"value": "default", "name": "default"}],
                     "category": "model"}
                ]
            }
        }
    });
    match norm(no_paren) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::AgentConfig { model, effort } => {
                assert_eq!(model.as_deref(), Some("default"));
                assert_eq!(effort, None);
            }
            _ => panic!("expected agent config"),
        },
        other => panic!("expected event, got {other:?}"),
    }
    // 缺 configOptions / 无匹配 option → 字段 None（部分更新，不覆盖）。
    let partial = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": "acp-1", "update": {"sessionUpdate": "config_option_update"}}
    });
    match norm(partial) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::AgentConfig { model, effort } => {
                assert_eq!(model, None);
                assert_eq!(effort, None);
            }
            _ => panic!("expected agent config"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_usage_update() {
    let f = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "acp-1",
            "update": {"sessionUpdate": "usage_update", "used": 42000, "size": 200000}
        }
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::AgentUsage {
                context_window,
                context_used,
            } => {
                assert_eq!(context_window, 200_000);
                assert_eq!(context_used, 42_000);
            }
            _ => panic!("expected agent usage"),
        },
        other => panic!("expected event, got {other:?}"),
    }
    // 缺 used/size → MissingField（必填，§6.3 同源拒绝）。
    let missing = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "acp-1",
            "update": {"sessionUpdate": "usage_update", "used": 1}
        }
    });
    assert!(matches!(
        norm(missing),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
}

#[test]
fn map_session_list_response() {
    let f = json!({
        "type": "session_list",
        "payload": {"sessions": [{"sessionId": "s1", "title": "t", "status": "ended", "updatedAt": "x"}]}
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::SessionListResponse { entries } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].session_id, "s1");
            }
            _ => panic!("expected session list"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}

#[test]
fn map_jsonrpc_wrapped_event() {
    let f = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "acp-1",
            "type": "agent_message_chunk",
            "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}
        }
    });
    match norm(f) {
        NormalizeOutcome::Event(ev) => assert_eq!(ev.body.kind(), "message_delta"),
        other => panic!("expected event, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 5. RpcResponse（L3 输入）
// ---------------------------------------------------------------------------

#[test]
fn rpc_response_ok() {
    let f = json!({"jsonrpc": "2.0", "id": "hub-3", "result": {"ok": true}});
    match norm(f) {
        NormalizeOutcome::RpcResponse { id, is_error } => {
            assert_eq!(id, "hub-3");
            assert!(!is_error);
        }
        other => panic!("expected rpc response, got {other:?}"),
    }
}

#[test]
fn rpc_response_error() {
    let f = json!({"jsonrpc": "2.0", "id": "hub-4", "error": {"code": -32601, "message": "unknown"}});
    match norm(f) {
        NormalizeOutcome::RpcResponse { id, is_error } => {
            assert_eq!(id, "hub-4");
            assert!(is_error);
        }
        other => panic!("expected rpc response, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 4. 未知帧 / 畸形帧 / 缺失字段
// ---------------------------------------------------------------------------

#[test]
fn unknown_type_dropped() {
    let f = json!({"type": "unknown_frame", "payload": {}});
    assert!(matches!(
        norm(f),
        NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
    ));
}

#[test]
fn unknown_jsonrpc_method_dropped() {
    let f = json!({"jsonrpc": "2.0", "method": "unknown/method", "params": {}});
    assert!(matches!(
        norm(f),
        NormalizeOutcome::Dropped(DropReason::UnsupportedFrame)
    ));
}

#[test]
fn non_object_frame_malformed() {
    assert!(matches!(
        norm(json!("just a string")),
        NormalizeOutcome::Dropped(DropReason::Malformed)
    ));
    assert!(matches!(
        norm(json!(42)),
        NormalizeOutcome::Dropped(DropReason::Malformed)
    ));
}

#[test]
fn payload_non_object_malformed() {
    let f = json!({"type": "agent_message_chunk", "payload": "nope"});
    assert!(matches!(
        norm(f),
        NormalizeOutcome::Dropped(DropReason::Malformed)
    ));
}

#[test]
fn missing_required_field_dropped() {
    // 无 turn_id 的增量（§6.3 同源拒绝）。
    let f = json!({"type": "agent_message_chunk", "payload": {"entryId": "e", "blockId": "b", "text": "x"}});
    assert!(matches!(
        norm(f),
        NormalizeOutcome::Dropped(DropReason::MissingField)
    ));
}

#[test]
fn truncated_text() {
    let long = "a".repeat(10_000);
    let f = json!({"type": "agent_message_chunk", "payload": {"turnId": "t", "entryId": "e", "blockId": "b", "text": long}});
    match norm(f) {
        NormalizeOutcome::Event(ev) => match ev.body {
            EventBody::MessageDelta { text, .. } => assert_eq!(text.len(), TEXT_MAX_BYTES),
            _ => panic!("expected message delta"),
        },
        other => panic!("expected event, got {other:?}"),
    }
}
