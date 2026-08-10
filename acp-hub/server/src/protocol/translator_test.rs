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
        turn_id: "turn-1".to_string(),
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
        },
    };
    let msg = t.translate(&action, &ctx()).unwrap();
    match msg {
        OutboundMessage::JsonRpc(v) => {
            assert_eq!(v["jsonrpc"], json!("2.0"));
            assert_eq!(v["method"], json!("session/prompt"));
            assert_eq!(v["params"]["sessionId"], json!("acp-1"));
            // cwd 是 ACP 请求的严谨字段：出站帧必须与 spawn/会话绑定目录
            // 一致（§6.3 workspace 扩展）。
            assert_eq!(v["params"]["cwd"], json!("/srv/work"));
            // agent-client-protocol（peri acp 实测）：prompt 为 ContentBlock
            // 序列，非 message 字符串；无 turnId（宿主侧归位，§7.2）。
            assert_eq!(v["params"]["prompt"], json!([{ "type": "text", "text": "hello" }]));
            assert!(v["params"].get("message").is_none());
            assert!(v["params"].get("turnId").is_none());
            assert!(v["id"].as_str().unwrap().starts_with("hub-"));
            // id 必带（避免被当作 notification，§6.1）。
            assert!(v["id"].is_string());
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
            // cancel 同带 cwd（ACP 请求字段一致性）。
            assert_eq!(v["params"]["cwd"], json!("/srv/work"));
            // 真实 peri 实测：session/cancel 是 notification——无 id、必带
            // jsonrpc 版本。
            assert_eq!(v["jsonrpc"], json!("2.0"));
            assert!(v.get("id").is_none());
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
    assert_eq!(init["params"]["cwd"], json!("/srv/work"));
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
