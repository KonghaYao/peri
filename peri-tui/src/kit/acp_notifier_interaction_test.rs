use super::*;
use serde_json::json;
use serial_test::serial;
fn spawn_test_notifier() -> (
    mpsc::UnboundedSender<AcpNotification>,
    mpsc::UnboundedReceiver<AcpEventWithEpoch>,
    CancellationToken,
) {
    let (notif_tx, notif_rx) = mpsc::unbounded_channel::<AcpNotification>();
    let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
    let shutdown = CancellationToken::new();
    let _handle = spawn_kit_notifier(notif_rx, bridge_tx, shutdown.clone());
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
        .unwrap();

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
                request_id_json: serde_json::to_string(&id).unwrap(),
                params: json!({
                    "sessionId": "s1",
                    "toolCall": {"title": title, "rawInput": {"marker": title}},
                    "options": []
                }),
            })
            .unwrap();
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
                request_id_json: serde_json::to_string(&id).unwrap(),
                params: json!({
                    "sessionId": "s1",
                    "requestedSchema": {"type": "object", "properties": {
                        (question_id): {"type": "string", "title": question_id}
                    }}
                }),
            })
            .unwrap();
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
        payload: AskUser { questions: vec![] },
    });
    let (notif_tx, mut bridge_rx, shutdown) = spawn_test_notifier();
    notif_tx
        .send(AcpNotification::RequestPermission {
            owner: Default::default(),
            request_id_json: "1".into(),
            params: json!({"sessionId":"s1","toolCall":{"title":"new","rawInput":null}}),
        })
        .unwrap();
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
        .unwrap();
    bridge_rx
        .recv()
        .await
        .expect("应完成 elicitation conversion");
    assert_eq!(
        crate::kit::atoms::HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel-hitl\""
    );
    assert_eq!(
        crate::kit::atoms::ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel-ask\""
    );
    shutdown.cancel();
    *crate::kit::atoms::HITL_PENDING.state().write() = old_hitl;
    *crate::kit::atoms::ASK_USER_PENDING.state().write() = old_ask;
}
