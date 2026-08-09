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
