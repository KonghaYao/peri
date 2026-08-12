//! Translator 出站映射单测（设计稿 §16 测试 6–7）。

use serde_json::json;

use acp_hub_proto::action::{
    ActionEnvelope, CancelChatPayload, PermissionDecision, PromptChatPayload,
    ResolvePermissionPayload,
};

use super::*;

fn ctx() -> OutboundCtx {
    OutboundCtx {
        cwd: "/srv/work".to_string(),
        acp_session_id: "acp-1".to_string(),
    }
}

#[test]
fn prompt_translation() {
    let t = Translator::new();
    let action = ActionEnvelope::Prompt {
        command_id: "c1".into(),
        payload: PromptChatPayload {
            chat_id: "hub-s1".into(),
            message: "hello".into(),
            effort: None,
        },
    };
    let msg = t.translate(&action, &ctx()).unwrap();
    match msg {
        OutboundMessage::JsonRpc(v) => {
            assert_eq!(v["jsonrpc"], json!("2.0"));
            assert_eq!(v["method"], json!("session/prompt"));
            assert_eq!(v["params"]["sessionId"], json!("acp-1"));
            // 官方 PromptRequest = {sessionId, prompt}（schema v1，#7）：无
            // cwd（spawn/会话绑定目录隐含）——不得出现在出站帧。
            assert!(v["params"].get("cwd").is_none());
            // agent-client-protocol（peri acp 实测）：prompt 为 ContentBlock
            // 序列，非 message 字符串；无 turnId（宿主侧归位，§7.2）。
            assert_eq!(v["params"]["prompt"], json!([{ "type": "text", "text": "hello" }]));
            assert!(v["params"].get("message").is_none());
            assert!(v["params"].get("turnId").is_none());
            // effort 缺省 → 不写入 params（agent 默认档位，跨任务契约 §2）。
            assert!(v["params"].get("effort").is_none());
            assert!(v["id"].as_str().unwrap().starts_with("hub-"));
            // id 必带（避免被当作 notification，§6.1）。
            assert!(v["id"].is_string());
        }
        _ => panic!("expected json rpc"),
    }
}

#[test]
fn prompt_translation_ignores_effort() {
    // #7：官方 PromptRequest 无 effort 字段——payload.effort 即使为
    // Some 也不写入出站帧（agent 侧默认档位；proto/web 端保留，仅
    // translator 不写，遗留标注见 02-plan.md Slice 2）。
    let t = Translator::new();
    let action = ActionEnvelope::Prompt {
        command_id: "c1".into(),
        payload: PromptChatPayload {
            chat_id: "hub-s1".into(),
            message: "hi".into(),
            effort: Some("high".into()),
        },
    };
    match t.translate(&action, &ctx()).unwrap() {
        OutboundMessage::JsonRpc(v) => {
            assert!(v["params"].get("effort").is_none());
            assert!(v["params"].get("cwd").is_none());
        }
        _ => panic!("expected json rpc"),
    }
}

#[test]
fn outbound_ctx_without_turn_id() {
    // #6：OutboundCtx 仅 cwd + acp_session_id（字段删除的编译期验证——
    // 若残留 turn_id 字段此构造不通过）；prompt 帧 params 精确等于
    // `{sessionId, prompt}`（无任何多余键，官方 PromptRequest 形状）。
    let t = Translator::new();
    let action = ActionEnvelope::Prompt {
        command_id: "c1".into(),
        payload: PromptChatPayload {
            chat_id: "hub-s1".into(),
            message: "hello".into(),
            effort: None,
        },
    };
    match t.translate(&action, &ctx()).unwrap() {
        OutboundMessage::JsonRpc(v) => {
            assert_eq!(
                v["params"],
                json!({
                    "sessionId": "acp-1",
                    "prompt": [{ "type": "text", "text": "hello" }],
                }),
                "params 无多余键（cwd/effort/turnId 均不得出现）"
            );
        }
        _ => panic!("expected json rpc"),
    }
}

#[test]
fn cancel_translation() {
    let t = Translator::new();
    let action = ActionEnvelope::Cancel {
        command_id: "c2".into(),
        payload: CancelChatPayload {
            chat_id: "hub-s1".into(),
        },
    };
    match t.translate(&action, &ctx()).unwrap() {
        OutboundMessage::JsonRpc(v) => {
            assert_eq!(v["method"], json!("session/cancel"));
            assert_eq!(v["params"]["sessionId"], json!("acp-1"));
            // 官方 CancelNotification = {sessionId}（schema v1，#7）：无 cwd。
            assert_eq!(
                v["params"],
                json!({ "sessionId": "acp-1" }),
                "params 仅保留 sessionId"
            );
            // 真实 peri 实测：session/cancel 是 notification——无 id、必带
            // jsonrpc 版本。
            assert_eq!(v["jsonrpc"], json!("2.0"));
            assert!(v.get("id").is_none());
        }
        _ => panic!("expected json rpc"),
    }
}

#[test]
fn session_new_translation() {
    let t = Translator::new();
    let action = ActionEnvelope::SessionNew {
        command_id: "c2".into(),
        payload: acp_hub_proto::action::SessionNewChatPayload {
            chat_id: "hub-s1".into(),
        },
    };
    match t.translate(&action, &ctx()).unwrap() {
        OutboundMessage::JsonRpc(v) => {
            assert_eq!(v["jsonrpc"], json!("2.0"));
            assert_eq!(v["method"], json!("session/new"));
            // cwd 由 server 注入（与 spawn/会话绑定目录一致，§6.3）。
            assert_eq!(v["params"]["cwd"], json!("/srv/work"));
            // mcpServers 必填（agent-client-protocol；空数组 = 无 MCP）。
            assert_eq!(v["params"]["mcpServers"], json!([]));
            // 带 id 的 request（非 notification）——coordinator 以帧 id 为
            // register_rpc 键（§6.1）；id 为本次 translate 分配的 rpc_id。
            assert!(v["id"].as_str().unwrap().starts_with("hub-"));
            // 无 title（会话标题由后续 session_update/服务端单写补齐）。
            assert!(v["params"].get("title").is_none());
        }
        _ => panic!("expected json rpc"),
    }
}

#[test]
fn resolve_translation() {
    let t = Translator::new();
    let action = ActionEnvelope::ResolvePermission {
        command_id: "c3".into(),
        payload: ResolvePermissionPayload {
            chat_id: "hub-s1".into(),
            permission_id: "p1".into(),
            decision: PermissionDecision::Allow,
        },
    };
    match t.translate(&action, &ctx()).unwrap() {
        OutboundMessage::JsonRpc(v) => {
            assert_eq!(v["method"], json!("permission.resolve"));
            assert_eq!(v["params"]["permissionId"], json!("p1"));
            assert_eq!(v["params"]["decision"], json!("allow"));
            // resolve 同带 cwd（ACP 请求字段一致性）。
            assert_eq!(v["params"]["cwd"], json!("/srv/work"));
        }
        _ => panic!("expected json rpc"),
    }
}

// ---------------------------------------------------------------------------
// #1 官方 request_permission 响应构造（schema v1；响应帧无回执，§4.4 以
// forward_ack 为确认点）
// ---------------------------------------------------------------------------

/// ①Allow + [allow_once option] → `selected` + optionId 回显。
#[test]
fn permission_response_rpc_allow_selects_allow_option() {
    let t = Translator::new();
    let options = json!([
        {"optionId": "reject-once", "name": "拒绝一次", "kind": "reject_once"},
        {"optionId": "allow-once", "name": "允许一次", "kind": "allow_once"}
    ]);
    let v = t.permission_response_rpc(
        &json!(5),
        PermissionDecision::Allow,
        options.as_array().unwrap(),
    );
    assert_eq!(v["jsonrpc"], json!("2.0"));
    assert_eq!(v["id"], json!(5), "agent request id 原样回显（number）");
    assert!(v.get("method").is_none(), "响应帧无 method");
    assert_eq!(v["result"]["outcome"]["outcome"], json!("selected"));
    assert_eq!(v["result"]["outcome"]["optionId"], json!("allow-once"));
}

/// ②Deny 无 reject 类 option → `cancelled` 且无 optionId。
#[test]
fn permission_response_rpc_deny_without_reject_cancelled() {
    let t = Translator::new();
    let options = json!([
        {"optionId": "allow-session", "name": "始终允许", "kind": "allowSession"}
    ]);
    let v = t.permission_response_rpc(
        &json!("req-9"),
        PermissionDecision::Deny,
        options.as_array().unwrap(),
    );
    assert_eq!(v["id"], json!("req-9"));
    assert_eq!(v["result"]["outcome"]["outcome"], json!("cancelled"));
    assert!(v["result"]["outcome"].get("optionId").is_none());
}

/// ③Deny + [reject_once option] → `selected` + reject optionId（保留
/// 「拒绝并记住」语义）。
#[test]
fn permission_response_rpc_deny_selects_reject_option() {
    let t = Translator::new();
    let options = json!([
        {"optionId": "allow-once", "name": "允许", "kind": "allow_once"},
        {"optionId": "reject-always", "name": "始终拒绝", "kind": "reject_always"}
    ]);
    let v = t.permission_response_rpc(
        &json!(5),
        PermissionDecision::Deny,
        options.as_array().unwrap(),
    );
    assert_eq!(v["result"]["outcome"]["outcome"], json!("selected"));
    assert_eq!(v["result"]["outcome"]["optionId"], json!("reject-always"));
}

/// ④Allow + 空 options（入站校验允许空数组，评审 P2-1）→ `cancelled`
/// 且无 `optionId: null`（官方契约 selected 分支 optionId 必须为 string）。
#[test]
fn permission_response_rpc_allow_empty_options_cancelled() {
    let t = Translator::new();
    let v = t.permission_response_rpc(&json!(5), PermissionDecision::Allow, &[]);
    assert_eq!(v["id"], json!(5));
    assert_eq!(v["result"]["outcome"]["outcome"], json!("cancelled"));
    assert!(
        v["result"]["outcome"].get("optionId").is_none(),
        "空 options 时不得序列化 optionId: null"
    );
    let serialized = serde_json::to_string(&v["result"]["outcome"]).unwrap();
    assert!(!serialized.contains("null"), "序列化结果不得含 null: {serialized}");
}

#[test]
fn rpc_id_monotonic_and_unique() {
    let t = Translator::new();
    let a = t.alloc_rpc_id();
    let b = t.alloc_rpc_id();
    assert_eq!(a, "hub-1");
    assert_eq!(b, "hub-2");
    assert_ne!(a, b);
}

#[test]
fn unsupported_actions() {
    let t = Translator::new();
    let load = ActionEnvelope::Load {
        command_id: "c".into(),
        payload: acp_hub_proto::action::LoadChatPayload {
            chat_id: "s".into(),
            acp_session_id: "acp-s".into(),
        },
    };
    assert!(matches!(
        t.translate(&load, &ctx()),
        Err(TranslateError::UnsupportedAction(_))
    ));
    // create/close 不在此入口（两段式 / instance/kill）。
    let create = ActionEnvelope::Create {
        command_id: "c".into(),
        payload: acp_hub_proto::action::CreateChatPayload::default(),
    };
    assert!(matches!(
        t.translate(&create, &ctx()),
        Err(TranslateError::UnsupportedAction(_))
    ));
}

#[test]
fn create_two_phase_rpcs() {
    let t = Translator::new();
    let (init_id, init) = t.initialize_rpc("/srv/work");
    assert_eq!(init["method"], json!("initialize"));
    // 官方 InitializeRequest = {protocolVersion, ...}（schema v1，#7）：
    // 无 cwd；protocolVersion 官方 integer，值 1 合法。
    assert_eq!(init["params"]["protocolVersion"], json!(1));
    assert!(init["params"].get("cwd").is_none());
    assert_eq!(init["id"].as_str().unwrap(), init_id.as_str());

    let (new_id, new) = t.session_new_rpc("/srv/work", Some("my session"));
    assert_eq!(new["method"], json!("session/new"));
    assert_eq!(new["params"]["cwd"], json!("/srv/work"));
    assert_eq!(new["params"]["title"], json!("my session"));
    assert_eq!(new["id"].as_str().unwrap(), new_id.as_str());
    assert_ne!(init_id, new_id);
}

#[test]
fn session_load_rpc_frame_shape() {
    let t = Translator::new();
    let (load_id, load) = t.session_load_rpc("/srv/work", "019fe709-3097-7f23-8266-9e5ceda78f4b");
    assert_eq!(load["jsonrpc"], json!("2.0"));
    assert_eq!(load["method"], json!("session/load"));
    // 目标会话 id 由请求参数携带（§8.5：load 响应体不含 sessionId，binding
    // 以请求参数为准）。
    assert_eq!(
        load["params"]["sessionId"],
        json!("019fe709-3097-7f23-8266-9e5ceda78f4b")
    );
    assert_eq!(load["params"]["cwd"], json!("/srv/work"));
    // 带 id 的 request（非 notification）——rpcId 由 server 分配（§6.1）。
    assert_eq!(load["id"].as_str().unwrap(), load_id.as_str());
    assert_eq!(load_id, "hub-1", "同一 translator 内 rpcId 单调");
}

#[test]
fn session_load_rpc_rejects_bad_cwd() {
    let t = Translator::new();
    // cwd 缺省/非法时 panic（server 注入路径的防御；同 initialize_rpc）。
    let r = std::panic::catch_unwind(|| t.session_load_rpc("", "acp-1"));
    assert!(r.is_err(), "空 cwd 应 panic（防御）");
}

// ---------------------------------------------------------------------------
// cwd 校验（§4.3 裁决）
// ---------------------------------------------------------------------------

#[test]
fn cwd_default_ok() {
    assert!(validate_cwd("/Users/me/work").is_ok());
}

#[test]
fn cwd_relative_rejected() {
    assert!(matches!(
        validate_cwd("relative/path"),
        Err(TranslateError::BadCwd(_))
    ));
}

#[test]
fn cwd_nul_rejected() {
    assert!(matches!(
        validate_cwd("/work\0x"),
        Err(TranslateError::BadCwd(_))
    ));
}

#[test]
fn cwd_control_chars_rejected() {
    assert!(matches!(
        validate_cwd("/work\nx"),
        Err(TranslateError::BadCwd(_))
    ));
}

#[test]
fn cwd_too_long_rejected() {
    let long = format!("/{}", "a".repeat(CWD_MAX_BYTES + 1));
    assert!(matches!(
        validate_cwd(&long),
        Err(TranslateError::BadCwd(_))
    ));
}

#[test]
fn cwd_empty_rejected() {
    assert!(matches!(
        validate_cwd(""),
        Err(TranslateError::MissingCwd)
    ));
}
