//! V2 加载后的真实 pump 交付：首屏完成后仍须继续收到实时消息。

use super::*;
use crate::acp_client::AcpNotification;
use peri_acp::transport::{AcpTransport, mpsc::mpsc_transport_pair, types::IncomingMessage};
use peri_acp_types::history_v2::HistoryLimits;
use serde_json::{Value, json};
use serial_test::serial;
use std::time::Duration;

struct HistoryGuard;

fn initial_response(params: &Value, execution_ready: bool) -> Value {
    json!({"_meta": {"peri.history": {
        "version": 2, "kind": "initial", "requestKey": params["_meta"]["peri.history"]["requestKey"],
        "executionReady": execution_ready, "runtimeGeneration": "runtime", "deliveryId": "delivery",
        "liveBoundary": {"streamId": "stream", "afterSeq": "0"},
        "page": {
            "version": 2, "kind": "window", "sessionId": "restored", "windowId": "window",
            "snapshotToken": "snapshot", "historyRevision": "0", "committedTail": null,
            "projectionVersion": 1, "readTicket": "ticket", "beforeCursor": null, "afterCursor": null,
            "items": [], "coverage": {"kind": "emptySnapshot"},
            "progress": {"reason": "complete", "scannedEntries": 0, "consumedEntries": 0}
        }
    }}})
}

impl Drop for HistoryGuard {
    fn drop(&mut self) {
        crate::kit::history_v2::reset_state();
    }
}

#[tokio::test]
#[serial]
async fn successful_v2_load_forwards_live_notifications_after_initial() {
    let _guard = HistoryGuard;
    crate::kit::history_v2::reset_state();
    crate::kit::history_v2::consumer::reset_request_key_for_test();
    let (transport, server) = mpsc_transport_pair();
    let (client, tx, mut notifications) = AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, true, HistoryLimits::default());
    client.spawn_pump(tx);
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("restored", "/tmp", None).await });
    let request = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, params } = request else {
        panic!("expected load")
    };
    assert_eq!(method, "session/load");
    server
        .send_response(id, Ok(initial_response(&params, true)))
        .await
        .unwrap();
    assert_eq!(load.await.unwrap().unwrap(), "restored");
    for (session, text) in [
        ("old-session", "stale"),
        ("restored", "first"),
        ("restored", "second"),
    ] {
        server.send_notification("session/update", json!({
            "sessionId": session,
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}
        })).await.unwrap();
    }
    for expected in ["first", "second"] {
        let notification = tokio::time::timeout(Duration::from_secs(2), notifications.recv())
            .await
            .expect("V2 load 后实时通知不能永久留在缓冲")
            .unwrap();
        let AcpNotification::SessionUpdate { params, .. } = notification else {
            panic!("expected session update")
        };
        assert_eq!(params["sessionId"], "restored");
        assert_eq!(
            params["update"]["content"]["text"], expected,
            "只交付当前会话，且保持 wire 顺序"
        );
    }
    client.close();
}

#[tokio::test]
#[serial]
async fn view_only_initial_keeps_history_readable_without_execution_cwd() {
    let _guard = HistoryGuard;
    crate::kit::history_v2::reset_state();
    crate::kit::history_v2::consumer::reset_request_key_for_test();
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.install_history_v2_for_test(true, true, HistoryLimits::default());
    let loader = client.clone();
    let load = tokio::spawn(async move { loader.load_session("restored", "/startup", None).await });
    let request = tokio::time::timeout(Duration::from_secs(5), server.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, params } = request else {
        panic!("expected load")
    };
    assert_eq!(method, "session/load");
    server
        .send_response(id, Ok(initial_response(&params, false)))
        .await
        .unwrap();
    assert_eq!(load.await.unwrap().unwrap(), "restored");
    let history = crate::kit::history_v2::snapshot();
    assert!(history.is_ready());
    assert!(!history.execution_ready);
    assert_eq!(
        client.current_execution_cwd(),
        None,
        "只读入口不得冒用启动目录执行"
    );
    assert!(client.ensure_session("/startup", None).await.is_err());
    client.close();
}
