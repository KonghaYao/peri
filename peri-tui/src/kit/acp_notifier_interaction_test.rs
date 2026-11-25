use super::AcpEventData;
use super::AcpEventWithEpoch;
use super::AcpNotification;
use super::CancellationToken;
use super::mpsc;
use super::{BridgeSink, spawn_kit_notifier};

use serde_json::json;
use serial_test::serial;
fn spawn_test_notifier() -> (
    mpsc::Sender<AcpNotification>,
    mpsc::Receiver<AcpEventWithEpoch>,
    CancellationToken,
) {
    let (notif_tx, notif_rx) =
        mpsc::channel(crate::acp_client::client::NOTIFICATION_QUEUE_CAPACITY);
    let (bridge_tx, bridge_rx) = mpsc::channel(crate::kit::acp_bridge::BRIDGE_QUEUE_CAPACITY);
    let shutdown = CancellationToken::new();
    let _handle = spawn_kit_notifier(notif_rx, BridgeSink::direct(bridge_tx), shutdown.clone());
    (notif_tx, bridge_rx, shutdown)
}

/// [回归测试] RequestPermission 的 payload 与 JSON-RPC RequestId 必须原子排队。
#[tokio::test]
#[serial]
async fn test_request_permission_forwards_hitl_pending_event() {
    crate::kit::atoms::init_atoms();
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();

    notif_tx
        .send(AcpNotification::RequestPermission {
            owner: Default::default(),
            request_id_json: "\"req-123\"".into(),
            params: json!({
                "sessionId": "s1",
                "toolCall": {
                    "title": "Bash",
                    "rawInput": {"command": "rm -rf /"}
                },
                "options": []
            }),
        })
        .await
        .expect("request permission forwards hitl pending event: fixture task and request must complete successfully");

    let bridge_event = bridge_rx.recv().await.expect("bridge 应收到 HitlPending");
    match bridge_event.event {
        AcpEventData::HitlPending(pending) => {
            assert_eq!(pending.request_id_json, "\"req-123\"");
            assert_eq!(pending.payload.tool_name, "Bash");
            assert_eq!(pending.payload.tool_input["command"], "rm -rf /");
        }
        other => panic!("expected HitlPending, got {other:?}"),
    }

    shutdown.cancel();
}

/// [回归测试] Number(7) 与 String("7") 必须保留 variant，且连续通知保持 FIFO。
#[tokio::test]
async fn test_request_permission_queued_events_keep_number_and_string_ids_atomic() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    for (id, title) in [
        (peri_acp::transport::types::RequestId::Number(7), "A"),
        (
            peri_acp::transport::types::RequestId::String("7".to_string()),
            "B",
        ),
    ] {
        notif_tx
            .send(AcpNotification::RequestPermission {
                owner: Default::default(),
                request_id_json: serde_json::to_string(&id).expect("request permission queued events keep number and string ids atomic: fixture must serialize to JSON"),
                params: json!({
                    "sessionId": "s1",
                    "toolCall": {"title": title, "rawInput": {"marker": title}},
                    "options": []
                }),
            })
            .await
            .expect("request permission queued events keep number and string ids atomic: fixture must serialize to JSON");
    }
    let a = bridge_rx.recv().await.expect("应收到 A");
    let b = bridge_rx.recv().await.expect("应收到 B");
    let AcpEventData::HitlPending(a) = a.event else {
        panic!("expected A HITL")
    };
    let AcpEventData::HitlPending(b) = b.event else {
        panic!("expected B HITL")
    };
    assert_eq!(
        (a.request_id_json.as_str(), a.payload.tool_name.as_str()),
        ("7", "A")
    );
    assert_eq!(
        (b.request_id_json.as_str(), b.payload.tool_name.as_str()),
        ("\"7\"", "B")
    );
    shutdown.cancel();
}

/// [回归测试] Elicitation 的 Number/String RequestId 与问题 payload 保持 FIFO 同行。
#[tokio::test]
async fn test_elicitation_queued_events_keep_number_and_string_ids_atomic() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    for (id, question_id) in [
        (peri_acp::transport::types::RequestId::Number(7), "a"),
        (
            peri_acp::transport::types::RequestId::String("7".to_string()),
            "b",
        ),
    ] {
        notif_tx
            .send(AcpNotification::Elicitation {
                owner: crate::acp_client::InteractionOwner {
                    kind: crate::acp_client::ReverseInteractionKind::Elicitation,
                    ..Default::default()
                },
                request_id_json: serde_json::to_string(&id).expect("elicitation queued events keep number and string ids atomic: fixture must serialize to JSON"),
                params: json!({
                    "sessionId": "s1",
                    "requestedSchema": {"type": "object", "properties": {
                        (question_id): {"type": "string", "title": question_id}
                    }}
                }),
            })
            .await
            .expect("elicitation queued events keep number and string ids atomic: fixture must serialize to JSON");
    }
    let a = bridge_rx.recv().await.expect("应收到 A");
    let b = bridge_rx.recv().await.expect("应收到 B");
    let AcpEventData::AskUser(a) = a.event else {
        panic!("expected A AskUser")
    };
    let AcpEventData::AskUser(b) = b.event else {
        panic!("expected B AskUser")
    };
    assert_eq!(
        (
            a.request_id_json.as_str(),
            a.payload.questions[0].id.as_str()
        ),
        ("7", "a")
    );
    assert_eq!(
        (
            b.request_id_json.as_str(),
            b.payload.questions[0].id.as_str()
        ),
        ("\"7\"", "b")
    );
    shutdown.cancel();
}

/// [回归测试] notifier 在 bridge gate 前不得修改 interaction active state。
#[tokio::test]
#[serial]
async fn test_interaction_notifier_has_no_pending_atom_side_effect() {
    use crate::kit::acp_types::PendingInteraction;
    use peri_acp_types::event_data::{AskUser, HitlPending};
    let old_hitl = crate::kit::atoms::HITL_PENDING.state().read().clone();
    let old_ask = crate::kit::atoms::ASK_USER_PENDING.state().read().clone();
    *crate::kit::atoms::HITL_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel-hitl\"".into(),
        payload: HitlPending {
            tool_name: "sentinel".into(),
            tool_input: json!(null),
            batch: None,
        },
    });
    *crate::kit::atoms::ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel-ask\"".into(),
        payload: AskUser {
            questions: vec![],
            elicitation: None,
        },
    });
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::RequestPermission {
            owner: Default::default(),
            request_id_json: "1".into(),
            params: json!({"sessionId":"s1","toolCall":{"title":"new","rawInput":null}}),
        })
        .await
        .expect("interaction notifier has no pending atom side effect: fixture task and request must complete successfully");
    bridge_rx.recv().await.expect("应完成 notifier conversion");
    notif_tx
        .send(AcpNotification::Elicitation {
            owner: crate::acp_client::InteractionOwner {
                kind: crate::acp_client::ReverseInteractionKind::Elicitation,
                ..Default::default()
            },
            request_id_json: "2".into(),
            params: json!({"sessionId":"s1","requestedSchema":{"type":"object","properties":{}}}),
        })
        .await
        .expect("interaction notifier has no pending atom side effect: fixture task and request must complete successfully");
    bridge_rx
        .recv()
        .await
        .expect("应完成 elicitation conversion");
    assert_eq!(
        crate::kit::atoms::HITL_PENDING
            .state()
            .read()
            .as_ref()
            .expect("interaction notifier has no pending atom side effect: the fixture storage and layout must support the requested operation")
            .request_id_json,
        "\"sentinel-hitl\""
    );
    assert_eq!(
        crate::kit::atoms::ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .expect("interaction notifier has no pending atom side effect: the fixture storage and layout must support the requested operation")
            .request_id_json,
        "\"sentinel-ask\""
    );
    shutdown.cancel();
    *crate::kit::atoms::HITL_PENDING.state().write() = old_hitl;
    *crate::kit::atoms::ASK_USER_PENDING.state().write() = old_ask;
}

/// 外部 MCP form：`_meta` marker 触发 typed field，不再落回内置 AskUser 解析。
#[tokio::test]
async fn test_mcp_form_elicitation_keeps_types_and_origin() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::Elicitation {
            owner: Default::default(),
            request_id_json: "\"mcp-1\"".into(),
            params: json!({
                "mode": "form",
                "sessionId": "s1",
                "message": "Approve this expense?",
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "approve": {"type": "boolean", "title": "Approve"},
                        "note": {"type": "string", "title": "Note"},
                        "amount": {"type": "integer", "title": "Amount"},
                        "colors": {
                            "type": "array",
                            "title": "Colors",
                            "items": {"enum": ["red", "blue"]}
                        }
                    },
                    "required": ["approve"]
                },
                "_meta": {
                    "io.peri/mcpElicitation": {"server": "expense", "tool": "submit"}
                }
            }),
        })
        .await
        .expect("send mcp elicitation");

    let event = bridge_rx.recv().await.expect("bridge 应收到 AskUser");
    let AcpEventData::AskUser(pending) = event.event else {
        panic!("expected AskUser")
    };
    assert!(matches!(
        pending.payload.elicitation,
        Some(peri_acp_types::event_data::ElicitationPresentation::Form {
            ref server,
            ref tool
        }) if server == "expense" && tool == "submit"
    ));
    let by_id = |id: &str| {
        pending
            .payload
            .questions
            .iter()
            .find(|q| q.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
    };
    let approve = by_id("approve");
    assert_eq!(approve.field_type.as_deref(), Some("boolean"));
    assert!(approve.required);
    assert_eq!(approve.options.len(), 2);
    assert_eq!(approve.options[0].value, Some(json!(true)));
    assert!(!approve.allows_custom_text());

    let note = by_id("note");
    assert_eq!(note.field_type.as_deref(), Some("string"));
    assert!(!note.required);
    assert!(note.allows_custom_text());

    let amount = by_id("amount");
    assert_eq!(amount.field_type.as_deref(), Some("integer"));
    assert!(amount.options.is_empty());

    let colors = by_id("colors");
    assert_eq!(colors.field_type.as_deref(), Some("array"));
    assert!(colors.multi_select);
    assert_eq!(colors.options[1].value, Some(json!("blue")));

    shutdown.cancel();
}

/// 外部 MCP URL：单确认项 + 完整 URL/域名，TUI 不自动打开。
#[tokio::test]
async fn test_mcp_url_elicitation_carries_server_and_url() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::Elicitation {
            owner: Default::default(),
            request_id_json: "\"mcp-2\"".into(),
            params: json!({
                "mode": "url",
                "sessionId": "s1",
                "message": "Authorize in browser",
                "url": "https://example.com/connect?token=secret",
                "elicitationId": "e-1",
                "_meta": {
                    "io.peri/mcpElicitation": {"server": "oauth", "tool": "connect"}
                }
            }),
        })
        .await
        .expect("send mcp url elicitation");
    let event = bridge_rx.recv().await.expect("bridge 应收到 AskUser");
    let AcpEventData::AskUser(pending) = event.event else {
        panic!("expected AskUser")
    };
    match pending.payload.elicitation {
        Some(peri_acp_types::event_data::ElicitationPresentation::Url {
            server,
            tool,
            url,
            elicitation_id,
            ..
        }) => {
            assert_eq!(server, "oauth");
            assert_eq!(tool, "connect");
            assert_eq!(url, "https://example.com/connect?token=secret");
            assert_eq!(elicitation_id, "e-1");
        }
        other => panic!("expected URL presentation, got {other:?}"),
    }
    assert_eq!(pending.payload.questions.len(), 1);
    assert_eq!(
        pending.payload.questions[0].field_type.as_deref(),
        Some("url")
    );
    shutdown.cancel();
}

/// 无 marker 的内置 AskUserQuestion 仍走原路径（不误标 elicitation）。
#[tokio::test]
async fn test_builtin_ask_user_without_marker_stays_legacy() {
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::Elicitation {
            owner: Default::default(),
            request_id_json: "\"ask-1\"".into(),
            params: json!({
                "mode": "form",
                "sessionId": "s1",
                "message": "Pick",
                "requestedSchema": {
                    "type": "object",
                    "properties": {
                        "q": {"type": "string", "oneOf": [{"const": "a", "title": "A"}]}
                    }
                }
            }),
        })
        .await
        .expect("send legacy elicitation");
    let event = bridge_rx.recv().await.expect("bridge 应收到 AskUser");
    let AcpEventData::AskUser(pending) = event.event else {
        panic!("expected AskUser")
    };
    assert!(pending.payload.elicitation.is_none());
    assert_eq!(pending.payload.questions[0].field_type, None);
    assert_eq!(pending.payload.questions[0].options[0].label, "a");
    assert_eq!(pending.payload.questions[0].options[0].value, None);
    shutdown.cancel();
}
