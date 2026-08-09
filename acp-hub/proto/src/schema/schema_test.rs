//! Y.Doc schema 类型结构完整性测试（§12.1 可选）：serde round-trip + 枚举形态。

use std::collections::HashMap;

use crate::action::PermissionDecision;
use crate::schema::{
    ActiveTurnProjection, AgentStatusProjection, BlockVisibility, ChatDocRoot, ChatEntry,
    ContentBlock, EntryKind, EntryRole, EntryStatus, GlobalStatus, InstanceStatus,
    InstanceView, PermissionOptions, PermissionProjection, PermissionStatus, PublicError,
    RegistryDocRoot, RegistryGlobal, ControlDocRoot, ChatInfoProjection,
    ChatStatus, ChatSummary, SessionSummaryProjection, ToolCallProjection,
    ToolCallStatus, TurnStatus,
};

fn chat_root() -> ChatDocRoot {
    let mut blocks = HashMap::new();
    blocks.insert(
        "b1".into(),
        ContentBlock::Reasoning {
            block_id: "b1".into(),
            text: "think".into(),
            visibility: BlockVisibility::Summary,
        },
    );
    let mut entries = HashMap::new();
    entries.insert(
        "t1:assistant".into(),
        ChatEntry {
            entry_id: "t1:assistant".into(),
            turn_id: Some("t1".into()),
            kind: EntryKind::Message,
            role: EntryRole::Assistant,
            status: EntryStatus::Completed,
            author_user_id: None,
            created_at: "2026-08-07T00:00:00Z".into(),
            completed_at: Some("2026-08-07T00:00:01Z".into()),
            block_order: vec!["b1".into()],
            blocks,
            error: None,
        },
    );
    let mut tool_calls = HashMap::new();
    tool_calls.insert(
        "tc1".into(),
        ToolCallProjection {
            tool_call_id: "tc1".into(),
            turn_id: "t1".into(),
            name: "bash".into(),
            status: ToolCallStatus::Completed,
            arguments: Some(serde_json::json!({"cmd": "ls"})),
            result: Some(serde_json::json!({"output": "x"})),
            public_error: None,
            permission_id: None,
        },
    );
    ChatDocRoot {
        schema_version: crate::version::CHAT_DOC_SCHEMA_VERSION,
        projection_version: 3,
        entry_order: vec!["t1:assistant".into()],
        entries,
        tool_calls,
    }
}

fn control_root() -> ControlDocRoot {
    let mut pending = HashMap::new();
    pending.insert(
        "p1".into(),
        PermissionProjection {
            permission_id: "p1".into(),
            turn_id: "t1".into(),
            tool_call_id: Some("tc1".into()),
            title: "run bash".into(),
            description: None,
            options: vec![PermissionOptions::AllowOnce, PermissionOptions::Deny],
            status: PermissionStatus::Pending,
            expires_at: "2026-08-07T00:05:00Z".into(),
            decision: None,
        },
    );
    let mut chats = HashMap::new();
    chats.insert(
        "old1".into(),
        SessionSummaryProjection {
            session_id: "old1".into(),
            title: "old".into(),
            status: "ended".into(),
            updated_at: "2026-08-06T00:00:00Z".into(),
        },
    );
    ControlDocRoot {
        schema_version: crate::version::CONTROL_DOC_SCHEMA_VERSION,
        projection_version: 2,
        chat: ChatInfoProjection {
            chat_id: "s1".into(),
            title: "demo".into(),
            status: ChatStatus::Active,
            active_turn_id: Some("t1".into()),
            created_at: "2026-08-07T00:00:00Z".into(),
            updated_at: "2026-08-07T00:00:01Z".into(),
        },
        agent: AgentStatusProjection {
            instance_id: "i1".into(),
            session_id: "acp-1".into(),
            status: "running".into(),
            capabilities: vec!["bash".into()],
            last_activity_at: "2026-08-07T00:00:01Z".into(),
            public_error: None,
        },
        active_turn: Some(ActiveTurnProjection {
            turn_id: "t1".into(),
            turn_status: TurnStatus::Running,
            updated_at: "2026-08-07T00:00:01Z".into(),
        }),
        pending_permissions: pending,
        sessions: chats,
    }
}

fn registry_root() -> RegistryDocRoot {
    let mut instances = HashMap::new();
    instances.insert(
        "i1".into(),
        InstanceView {
            id: "i1".into(),
            hostname: "host1".into(),
            status: InstanceStatus::Online,
            token_id: "tok1".into(),
            registered_at: "2026-08-01T00:00:00Z".into(),
            last_heartbeat: "2026-08-07T00:00:01Z".into(),
            chat_count: 2,
        },
    );
    let mut chats = HashMap::new();
    chats.insert(
        "s1".into(),
        ChatSummary {
            id: "s1".into(),
            instance_id: "i1".into(),
            title: "demo".into(),
            status: "active".into(),
            gap: None,
            updated_at: "2026-08-07T00:00:01Z".into(),
        },
    );
    RegistryDocRoot {
        schema_version: crate::version::REGISTRY_DOC_SCHEMA_VERSION,
        instances,
        chats,
        global: RegistryGlobal {
            status: GlobalStatus::Healthy,
        },
    }
}

/// 三 Doc 根对象字段名 camelCase 形态（§5.3–5.5 与 §2 序列化约定）。
#[test]
fn doc_root_camel_case_field_names() {
    let chat = serde_json::to_value(chat_root()).unwrap();
    assert_eq!(chat["schemaVersion"], 1);
    assert_eq!(chat["projectionVersion"], 3);
    assert_eq!(chat["entryOrder"][0], "t1:assistant");
    assert!(chat.get("entries").is_some());
    assert_eq!(chat["toolCalls"]["tc1"]["name"], "bash");

    let session = serde_json::to_value(control_root()).unwrap();
    assert_eq!(session["chat"]["chatId"], "s1");
    assert_eq!(session["chat"]["activeTurnId"], "t1");
    assert_eq!(session["agent"]["sessionId"], "acp-1");
    assert_eq!(session["activeTurn"]["turnStatus"], "running");
    assert_eq!(session["pendingPermissions"]["p1"]["options"][0], "allowOnce");
    assert!(session.get("sessions").is_some());

    let registry = serde_json::to_value(registry_root()).unwrap();
    assert_eq!(registry["instances"]["i1"]["tokenId"], "tok1");
    assert_eq!(registry["instances"]["i1"]["chatCount"], 2);
    assert_eq!(registry["chats"]["s1"]["instanceId"], "i1");
    assert_eq!(registry["global"]["status"], "healthy");
}

/// 跨 Doc 枚举序列化形态（camelCase；`PermissionDecision` 为 snake_case，§4.2）。
#[test]
fn enum_serialized_shapes() {
    assert_eq!(serde_json::to_string(&EntryKind::Message).unwrap(), "\"message\"");
    assert_eq!(serde_json::to_string(&EntryRole::Assistant).unwrap(), "\"assistant\"");
    assert_eq!(serde_json::to_string(&EntryStatus::Streaming).unwrap(), "\"streaming\"");
    assert_eq!(serde_json::to_string(&ToolCallStatus::AwaitingPermission).unwrap(), "\"awaitingPermission\"");
    assert_eq!(serde_json::to_string(&TurnStatus::AwaitingPermission).unwrap(), "\"awaitingPermission\"");
    assert_eq!(serde_json::to_string(&ChatStatus::Crashed).unwrap(), "\"crashed\"");
    assert_eq!(serde_json::to_string(&InstanceStatus::Offline).unwrap(), "\"offline\"");
    assert_eq!(serde_json::to_string(&GlobalStatus::Degraded).unwrap(), "\"degraded\"");
    assert_eq!(serde_json::to_string(&PermissionOptions::AllowSession).unwrap(), "\"allowSession\"");
    assert_eq!(serde_json::to_string(&PermissionStatus::Expired).unwrap(), "\"expired\"");
    assert_eq!(serde_json::to_string(&BlockVisibility::Hidden).unwrap(), "\"hidden\"");
    // PermissionDecision 供 §7 schema 复用（§4.2：snake_case）
    assert_eq!(
        serde_json::to_string(&PermissionDecision::Deny).unwrap(),
        "\"deny\""
    );
}

/// ContentBlock：tag = "kind" 的 internally tagged 判别（§5.3 镜像形态）。
#[test]
fn content_block_kind_tag() {
    let block = ContentBlock::ToolCall {
        block_id: "b1".into(),
        tool_call_id: "tc1".into(),
    };
    let v: serde_json::Value = serde_json::to_value(&block).unwrap();
    // §7.2 设计文档明确：ContentBlock 用 snake_case（镜像内部判别形态，非线协议）；
    // internally tagged 模式下 rename_all 只作用于 variant 名（kind 值），
    // 字段名保持 rust 名（block_id/tool_call_id）
    assert_eq!(v["kind"], "tool_call");
    assert_eq!(v["tool_call_id"], "tc1");

    let text: serde_json::Value =
        serde_json::to_value(ContentBlock::Text {
            block_id: "b2".into(),
            text: "hi".into(),
        })
        .unwrap();
    assert_eq!(text["kind"], "text");

    // 反序列化判别（字段名 = rust 名，见上注）
    let back: ContentBlock =
        serde_json::from_value(serde_json::json!({"kind": "resource", "block_id": "b3",
            "resource_id": "r1", "media_type": "text/plain", "name": "a.txt"}))
            .unwrap();
    assert_eq!(
        back,
        ContentBlock::Resource {
            block_id: "b3".into(),
            resource_id: "r1".into(),
            media_type: "text/plain".into(),
            name: "a.txt".into(),
        }
    );
}

/// 三 Doc 完整 round-trip（serde_json round-trip 每类根对象）。
#[test]
fn schema_roots_full_roundtrip() {
    let chat = chat_root();
    let back: ChatDocRoot =
        serde_json::from_str(&serde_json::to_string(&chat).unwrap()).unwrap();
    assert_eq!(back, chat);

    let session = control_root();
    let back: ControlDocRoot =
        serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
    assert_eq!(back, session);

    let registry = registry_root();
    let back: RegistryDocRoot =
        serde_json::from_str(&serde_json::to_string(&registry).unwrap()).unwrap();
    assert_eq!(back, registry);
}

/// PublicError 脱敏公开错误（§9.3）round-trip。
#[test]
fn public_error_roundtrip() {
    let err = PublicError {
        code: "AGENT_UNAVAILABLE".into(),
        message: "redacted".into(),
    };
    let v = serde_json::to_value(&err).unwrap();
    assert_eq!(v["code"], "AGENT_UNAVAILABLE");
    assert_eq!(v["message"], "redacted");
    assert_eq!(
        serde_json::from_value::<PublicError>(v).unwrap(),
        err
    );
}
