//! 帧模型单元测试：§4.8 向量 6（帧集白名单的 parse 层面）+ round-trip + 判别。

use crate::ack::{AckStatus, ActionAck, ActionError, ErrorCode};
use crate::action::{
    ActionEnvelope, CancelChatPayload, CloseChatPayload, CreateChatPayload, LoadChatPayload,
    PermissionDecision, PersistedSessionCreatePayload, PersistedSessionImportPayload,
    PersistedSessionOpenPayload, PersistedSessionRenamePayload, ProjectArchivePayload,
    ProjectCreatePayload, PromptChatPayload, ResolvePermissionPayload, SubscribeEventsPayload,
    UnsubscribeEventsPayload,
};
use crate::conn::{Auth, AuthResponse, DocId, KeepAlive, Pong, Ready};
use crate::event::EventFrame;
use crate::frame::{Frame, ProtoError};
use crate::instance::{
    BufferedFrame, InstanceBufferSync, InstanceEvent, InstanceHeartbeat, InstanceHello,
    InstanceKill, InstanceKillAck, InstanceProcessExit, InstanceSpawn, InstanceSpawnAck,
};
use crate::ysync::{YsyncAwareness, YsyncSubscribe, YsyncSync, YsyncUnsubscribe, YsyncUpdate};
use std::collections::HashMap;
use std::str::FromStr;

/// 覆盖 §3.2 全表 25 帧的构造器（M1 + 保留类型 + instance/forward 系）。
fn all_frames() -> Vec<Frame> {
    let mut env = HashMap::new();
    env.insert("PATH".to_string(), "/usr/bin".to_string());
    let mut epochs = HashMap::new();
    epochs.insert("s1".to_string(), 2u64);

    vec![
        Frame::Action(ActionEnvelope::ProjectCreate {
            command_id: "pc1".into(),
            payload: ProjectCreatePayload {
                name: "demo".into(),
                cwd: "/tmp".into(),
                instance_id: None,
            },
        }),
        Frame::Action(ActionEnvelope::ProjectArchive {
            command_id: "pa1".into(),
            payload: ProjectArchivePayload {
                project_id: "p1".into(),
            },
        }),
        Frame::Action(ActionEnvelope::PersistedSessionCreate {
            command_id: "sc1".into(),
            payload: PersistedSessionCreatePayload {
                project_id: "p1".into(),
                title: Some("new".into()),
            },
        }),
        Frame::Action(ActionEnvelope::PersistedSessionOpen {
            command_id: "so1".into(),
            payload: PersistedSessionOpenPayload {
                session_id: "hs1".into(),
            },
        }),
        Frame::Action(ActionEnvelope::PersistedSessionRename {
            command_id: "sr1".into(),
            payload: PersistedSessionRenamePayload {
                session_id: "hs1".into(),
                name: "renamed".into(),
            },
        }),
        Frame::Action(ActionEnvelope::PersistedSessionImport {
            command_id: "si1".into(),
            payload: PersistedSessionImportPayload {
                project_id: "p1".into(),
                acp_session_id: "acp-s1".into(),
            },
        }),
        // --- action 方法面（§4.3，含 M2/M3 保留类型） ---
        Frame::Action(ActionEnvelope::Create {
            command_id: "c1".into(),
            payload: CreateChatPayload {
                instance_id: None,
                cwd: Some("/tmp".into()),
                title: Some("t".into()),
                acp_session_id: None,
                workspace_id: None,
            },
        }),
        Frame::Action(ActionEnvelope::Load {
            command_id: "c2".into(),
            payload: LoadChatPayload {
                chat_id: "s1".into(),
                acp_session_id: "acp-s1".into(),
            },
        }),
        Frame::Action(ActionEnvelope::Close {
            command_id: "c3".into(),
            payload: CloseChatPayload {
                chat_id: "s1".into(),
            },
        }),
        Frame::Action(ActionEnvelope::Prompt {
            command_id: "c4".into(),
            payload: PromptChatPayload {
                chat_id: "s1".into(),
                message: "hi".into(),
                effort: None,
            },
        }),
        Frame::Action(ActionEnvelope::Cancel {
            command_id: "c5".into(),
            payload: CancelChatPayload {
                chat_id: "s1".into(),
            },
        }),
        Frame::Action(ActionEnvelope::ResolvePermission {
            command_id: "c6".into(),
            payload: ResolvePermissionPayload {
                chat_id: "s1".into(),
                permission_id: "p1".into(),
                decision: PermissionDecision::Allow,
            },
        }),
        Frame::Action(ActionEnvelope::SubscribeEvents {
            command_id: "c7".into(),
            payload: SubscribeEventsPayload {
                chat_id: Some("s1".into()),
                from_seq: Some(3),
            },
        }),
        Frame::Action(ActionEnvelope::UnsubscribeEvents {
            command_id: "c8".into(),
            payload: UnsubscribeEventsPayload { chat_id: None },
        }),
        // --- Ack 与错误 ---
        Frame::ActionAck(ActionAck {
            command_id: "c1".into(),
            status: AckStatus::Committed,
            turn_id: Some("t1".into()),
            chat_id: Some("s1".into()),
            project_id: Some("p1".into()),
            session_id: Some("hs1".into()),
            acp_session_id: None,
            committed_projection_version: Some(7),
        }),
        Frame::ActionError(ActionError {
            command_id: "c1".into(),
            code: ErrorCode::AgentUnavailable,
            message: "redacted".into(),
            retryable: true,
            retry_after_ms: Some(1000),
        }),
        // --- 连接生命周期 ---
        Frame::Event(EventFrame {
            chat_id: "s1".into(),
            seq: 5,
            frame: serde_json::json!({"type": "agent_message_chunk", "text": "x"}),
        }),
        Frame::KeepAlive(KeepAlive {}),
        Frame::Pong(Pong {}),
        Frame::Ready(Ready {
            projection_versions: {
                let mut m = HashMap::new();
                m.insert(DocId::chat("s1"), 7u32);
                m.insert(DocId::REGISTRY, 2u32);
                m
            },
        }),
        Frame::Auth(Auth {
            token: "tok".into(),
        }),
        Frame::AuthResponse(AuthResponse {
            connection_context: "AA==".into(),
            hmac: "BQ==".into(),
        }),
        // --- y-sync ---
        Frame::YsyncSubscribe(YsyncSubscribe {
            docs: vec![DocId::chat("s1"), DocId::session("s1")],
        }),
        Frame::YsyncUnsubscribe(YsyncUnsubscribe {
            docs: vec![DocId::chat("s1")],
        }),
        Frame::YsyncUpdate(YsyncUpdate {
            doc: DocId::chat("s1"),
            update: "AAAA".into(),
            projection_version: Some(7),
        }),
        Frame::YsyncSync(YsyncSync { msg: "AAAA".into() }),
        Frame::YsyncAwareness(YsyncAwareness { msg: "AAAA".into() }),
        // --- instance 9 帧 ---
        Frame::InstanceHello(InstanceHello {
            token: "mt".into(),
            hostname: "host1".into(),
            caps: serde_json::json!({"acp": "1.4"}),
            buffered: Some(true),
            buffer_lost: None,
            stream_epochs: Some(epochs.clone()),
            nonce: "AAAA".into(),
        }),
        Frame::InstanceHeartbeat(InstanceHeartbeat {
            load: 42,
            alive_sessions: vec!["s1".into()],
        }),
        Frame::InstanceEvent(InstanceEvent {
            chat_id: "s1".into(),
            epoch: 2,
            seq: 9,
            frame: serde_json::json!({"type": "agent_message_chunk"}),
        }),
        Frame::InstanceBufferSync(InstanceBufferSync {
            chat_id: "s1".into(),
            epoch: 2,
            from_seq: 7,
            frames: vec![BufferedFrame {
                seq: 7,
                frame: serde_json::json!({"type": "agent_message_chunk"}),
            }],
        }),
        Frame::InstanceSpawn(InstanceSpawn {
            command_id: "c9".into(),
            chat_id: "s1".into(),
            cmd: vec!["acp".into(), "--serve".into()],
            cwd: "/tmp".into(),
            env: Some(env),
        }),
        Frame::InstanceKill(InstanceKill {
            command_id: "c10".into(),
            chat_id: "s1".into(),
            grace: Some(500),
        }),
        Frame::InstanceSpawnAck(InstanceSpawnAck {
            command_id: "c9".into(),
            chat_id: "s1".into(),
            ok: true,
            error: None,
        }),
        Frame::InstanceKillAck(InstanceKillAck {
            command_id: "c10".into(),
            chat_id: "s1".into(),
            ok: true,
        }),
        Frame::InstanceProcessExit(InstanceProcessExit {
            chat_id: "s1".into(),
            code: 0,
        }),
    ]
}

/// 每类帧：序列化 → 解析 → 再序列化 → 再解析，语义稳定（§12.1「M1 全帧
/// round-trip」）。含 `HashMap` 的帧（`ready` 等）JSON 键序不保证字节稳定，
/// 断言改为解析相等性。
#[test]
fn all_frame_tags_roundtrip() {
    for frame in all_frames() {
        let raw = serde_json::to_string(&frame).expect("serialize");
        let parsed = Frame::parse(&raw).expect("parse");
        assert_eq!(parsed, frame, "roundtrip mismatch for tag {}", frame.tag());
        let raw2 = serde_json::to_string(&parsed).expect("re-serialize");
        let parsed2 = Frame::parse(&raw2).expect("re-parse");
        assert_eq!(
            parsed2,
            parsed,
            "second roundtrip mismatch for tag {}",
            frame.tag()
        );
    }
}

/// 双层 internally tagged 序列化形态：`{"t":"action","commandId":…,"type":…,"payload":…}`（§4.3）。
#[test]
fn action_envelope_nested_tag_shape() {
    let frame = Frame::Action(ActionEnvelope::Prompt {
        command_id: "c4".into(),
        payload: PromptChatPayload {
            chat_id: "s1".into(),
            message: "hi".into(),
            effort: None,
        },
    });
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&frame).unwrap()).unwrap();
    assert_eq!(value["t"], "action");
    assert_eq!(value["commandId"], "c4");
    assert_eq!(value["type"], "chat/prompt");
    assert_eq!(value["payload"]["chatId"], "s1");
    assert_eq!(value["payload"]["message"], "hi");
}

/// payload 判别（§12.1 + §8.5）：`chat/load` 携带 `{chatId, acpSessionId}`，
/// `chat/close` 仅 `{chatId}`——经 envelope 层 `type` 判别无歧义。
#[test]
fn load_vs_close_discrimination() {
    let load_raw = r#"{"t":"action","commandId":"c2","type":"chat/load","payload":{"chatId":"s1","acpSessionId":"acp-1"}}"#;
    let close_raw =
        r#"{"t":"action","commandId":"c3","type":"chat/close","payload":{"chatId":"s1"}}"#;

    let load = Frame::parse(load_raw).unwrap();
    let close = Frame::parse(close_raw).unwrap();
    assert!(matches!(load, Frame::Action(ActionEnvelope::Load { .. })));
    assert!(matches!(close, Frame::Action(ActionEnvelope::Close { .. })));
    assert_ne!(load, close, "same-shape payloads must not collapse");
    // §8.5：缺 acpSessionId 的 chat/load 视为畸形（目标会话必填）。
    assert!(Frame::parse(
        r#"{"t":"action","commandId":"c2","type":"chat/load","payload":{"chatId":"s1"}}"#
    )
    .is_err());
}

/// 未知 `t` → `Unsupported`（§4.8 向量 6）。
#[test]
fn unknown_tag_is_unsupported() {
    for raw in [
        r#"{"t":"foo"}"#,
        r#"{"t":"ysync.foo"}"#,
        r#"{"t":"instance/unknown"}"#,
    ] {
        match Frame::parse(raw) {
            Err(ProtoError::Unsupported(_)) => {}
            other => panic!("expected Unsupported for {raw}, got {other:?}"),
        }
    }
    // 精确断言错误内容
    assert_eq!(
        Frame::parse(r#"{"t":"foo"}"#),
        Err(ProtoError::Unsupported("foo".into()))
    );
}

/// 畸形 JSON / 缺字段 → `Malformed`（不 panic，§12.1）。
#[test]
fn malformed_input_is_malformed() {
    for raw in [
        "not json",
        "",
        r#"{"t":42}"#,
        r#"{"t":"action"}"#,                     // 缺 type/payload
        r#"{"t":"instance/spawn"}"#,             // 缺必填字段
        r#"{"t":"ysync.subscribe","docs":[1]}"#, // 字段类型错误
    ] {
        match Frame::parse(raw) {
            Err(ProtoError::Malformed(_)) => {}
            other => panic!("expected Malformed for {raw:?}, got {other:?}"),
        }
    }
}

/// 已知 tag 但载荷畸形 → Malformed（区别于未知 tag 的 Unsupported）。
#[test]
fn known_tag_bad_payload_is_malformed_not_unsupported() {
    assert!(matches!(
        Frame::parse(r#"{"t":"action"}"#),
        Err(ProtoError::Malformed(_))
    ));
}

/// tag() 与 FRAME_TAGS 注册表一一对应。
#[test]
fn every_frame_tag_is_registered() {
    let registered: Vec<&str> = crate::whitelist::FRAME_TAGS.iter().map(|t| t.0).collect();
    assert_eq!(registered.len(), 26, "§3.2 全表应有 26 个 tag");
    for frame in all_frames() {
        assert!(
            registered.contains(&frame.tag().0),
            "tag {} missing from FRAME_TAGS",
            frame.tag()
        );
    }
}

/// `ysync.update` 快照/增量投影版本（§4.6 步骤 3）：快照必带
/// `projection_version`，增量不携带；序列化形态为可选字段。
#[test]
fn ysync_update_projection_version_shape() {
    // 快照：携带投影版本
    let snapshot = Frame::YsyncUpdate(YsyncUpdate {
        doc: DocId::chat("s1"),
        update: "AAAA".into(),
        projection_version: Some(7),
    });
    let v: serde_json::Value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(v["t"], "ysync.update");
    assert_eq!(v["doc"], "chat:s1");
    assert_eq!(v["update"], "AAAA");
    assert_eq!(v["projectionVersion"], 7);
    assert_eq!(
        Frame::parse(&serde_json::to_string(&snapshot).unwrap()).unwrap(),
        snapshot
    );

    // 增量：不携带投影版本
    let delta = Frame::YsyncUpdate(YsyncUpdate {
        doc: DocId::chat("s1"),
        update: "AAAA".into(),
        projection_version: None,
    });
    let v: serde_json::Value = serde_json::to_value(&delta).unwrap();
    assert!(v.get("projectionVersion").is_none(), "增量不携带投影版本");
}

/// `DocId::REGISTRY` 必须等于 `hub:registry`（§5.2 表），且与
/// `FromStr` 解析结果一致——否则订阅/快照推送的键对不上。
#[test]
fn registry_doc_id_is_hub_registry() {
    assert_eq!(DocId::REGISTRY.as_str(), "hub:registry");
    assert_eq!(DocId::REGISTRY, DocId::from_str("hub:registry").unwrap());
    let v = serde_json::to_value(&DocId::REGISTRY).unwrap();
    assert_eq!(v, "hub:registry");
    // 与 chat/control 形态互异
    assert_ne!(DocId::REGISTRY, DocId::chat("registry"));
    assert_ne!(DocId::REGISTRY, DocId::session("registry"));
}

/// #4 DocId 前缀面：`session:` 白名单注册（FromStr）+ roundtrip（§5.2 表
/// `session:{cid}` 控制状态 Doc）；`control:` 死前缀不入白名单（代码实际
/// 只有 chat/session/hub 三前缀——固化死前缀语义）。
#[test]
fn session_docid_fromstr_parses() {
    let doc = DocId::from_str("session:s1").expect("session: 前缀应解析");
    assert_eq!(doc.as_str(), "session:s1", "as_str() roundtrip");
    assert_eq!(doc, DocId::session("s1"), "与构造器一致");
    // chat/hub 白名单不受影响。
    assert_eq!(DocId::from_str("chat:c1").unwrap().as_str(), "chat:c1");
    assert_eq!(DocId::from_str("hub:registry").unwrap(), DocId::REGISTRY);
    // control: 不入白名单（死前缀语义固化）。
    assert!(
        DocId::from_str("control:x").is_err(),
        "control: 是死前缀（代码实际用 session:）"
    );
    // 空 sid 段仍拒绝（§5.2 防注入）。
    assert!(DocId::from_str("session:").is_err(), "空 sid 仍拒绝");
}
