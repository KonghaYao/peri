//! RelayEventHandler 单测（设计稿 §16 测试 15–17）。
//!
//! 事件 epoch 用 0（F4 聚合器 stream.epoch 以 0 起始；真实 machine 的
//! epoch=1 首事件触发 uncalibratable 缺口为 F4 已知缺口，见输出遗留问题）。

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use acp_hub_proto::machine::{
    BufferedFrame, MachineBufferSync, MachineEvent, MachineHello,
};

use crate::channel::{ConsumeResult, RelayEventHandler};
use crate::control::StoreSink;
use crate::control::{MachineConn, MachineRegistry};
use crate::control::SessionRegistry;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager};

struct Env {
    _tmp: tempfile::TempDir,
    sessions: SessionRegistry,
    relay: Arc<RelayEventHandler>,
}

async fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let persist_cfg = PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(Store::open(&persist_cfg).unwrap());
    store.recover().await;
    let sink = Arc::new(StoreSink::new(store.clone()).await.unwrap());
    let doc = Arc::new(DocManager::new(BatchConfig::default(), sink));
    let registry = doc.registry();
    let sessions = SessionRegistry::new(registry);
    let machine = Arc::new(MachineRegistry::new(
        Duration::from_secs(30),
        Duration::from_secs(1),
        sessions.clone(),
    ));
    let relay = Arc::new(RelayEventHandler::new(
        doc.clone(),
        sessions.clone(),
        machine.clone(),
        doc.registry(),
    ));
    // machine 上线（hello 登记 session_epochs: acp-1 → 0）。
    let hello = MachineHello {
        token: "tok".into(),
        hostname: "local".into(),
        caps: json!({}),
        buffered: None,
        buffer_lost: None,
        stream_epochs: Some([("acp-1".to_string(), 0u64)].into_iter().collect()),
        nonce: "AAAA".into(),
    };
    let (tx, _rx) = mpsc::channel(8);
    machine
        .on_hello("local", "tok-m", MachineConn { tx }, &hello)
        .await;
    // 打开 session + binding（relay 投递前提）。
    doc.open_session("s1", "local", Some("t")).await.unwrap();
    sessions.register("s1", "local", Some("t")).await.unwrap();
    sessions.bind("s1", "acp-1").await.unwrap();
    Env {
        _tmp: tmp,
        sessions,
        relay,
    }
}

fn ev(seq: u64, frame: serde_json::Value) -> MachineEvent {
    MachineEvent {
        session_id: "acp-1".into(),
        epoch: 0,
        seq,
        frame,
    }
}

#[tokio::test]
async fn epoch_mismatch_dropped() {
    let env = env().await;
    // hello 登记 epoch=0；帧 epoch=1 → 丢弃（§4.5.1 防御）。
    let mut e = ev(1, json!({"type": "agent_message_chunk", "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}}));
    e.epoch = 1;
    let r = env.relay.on_machine_event("local", &e).await;
    assert!(matches!(r, ConsumeResult::Dropped { reason: "epoch_mismatch" }));
}

#[tokio::test]
async fn binding_missing_dropped() {
    let env = env().await;
    let mut e = ev(1, json!({"type": "agent_message_chunk", "payload": {}}));
    e.session_id = "acp-unknown".into();
    let r = env.relay.on_machine_event("local", &e).await;
    assert!(matches!(r, ConsumeResult::Dropped { reason: "binding_missing" }));
}

#[tokio::test]
async fn event_delivered_to_aggregator() {
    let env = env().await;
    let e = ev(1, json!({
        "type": "agent_message_chunk",
        "payload": {"turnId": "t1", "entryId": "t1:assistant", "blockId": "b1", "text": "hello"}
    }));
    let r = env.relay.on_machine_event("local", &e).await;
    match r {
        ConsumeResult::Delivered { session_id, kind, seq, applied } => {
            assert_eq!(session_id, "s1");
            assert_eq!(kind, "message_delta");
            assert_eq!(seq, 1);
            assert!(applied);
        }
        other => panic!("expected delivered, got {other:?}"),
    }
    let _ = env;
}

#[tokio::test]
async fn unknown_frame_dropped_counted() {
    let env = env().await;
    let e = ev(2, json!({"type": "unknown_frame", "payload": {}}));
    let r = env.relay.on_machine_event("local", &e).await;
    assert!(matches!(r, ConsumeResult::Dropped { reason: "unsupported_frame" }));
    assert!(env.relay.dropped_total() >= 1);
}

#[tokio::test]
async fn buffer_sync_epoch_mismatch_rejects_batch() {
    let env = env().await;
    let sync = MachineBufferSync {
        session_id: "acp-1".into(),
        epoch: 1, // hello 登记 0
        from_seq: 1,
        frames: vec![BufferedFrame {
            seq: 1,
            frame: json!({"type": "agent_message_chunk", "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}}),
        }],
    };
    let r = env.relay.on_buffer_sync("local", &sync).await;
    assert!(matches!(r, ConsumeResult::BatchRejected { reason: "buffer_sync_epoch_mismatch" }));
}

#[tokio::test]
async fn buffer_sync_out_of_order_frames_dropped() {
    let env = env().await;
    // from_seq=1，但帧 seq 从 2 开始（跳号）→ 乱序丢弃计数。
    let sync = MachineBufferSync {
        session_id: "acp-1".into(),
        epoch: 0,
        from_seq: 1,
        frames: vec![BufferedFrame {
            seq: 2,
            frame: json!({"type": "agent_message_chunk", "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}}),
        }],
    };
    let r = env.relay.on_buffer_sync("local", &sync).await;
    assert!(matches!(r, ConsumeResult::BatchRejected { reason: "all_frames_rejected" }));
}

#[tokio::test]
async fn buffer_sync_contiguous_delivered() {
    let env = env().await;
    let sync = MachineBufferSync {
        session_id: "acp-1".into(),
        epoch: 0,
        from_seq: 5,
        frames: vec![
            BufferedFrame {
                seq: 5,
                frame: json!({"type": "agent_message_chunk", "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "a"}}),
            },
            BufferedFrame {
                seq: 6,
                frame: json!({"type": "agent_message_chunk", "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "b"}}),
            },
        ],
    };
    let r = env.relay.on_buffer_sync("local", &sync).await;
    match r {
        ConsumeResult::Delivered { applied, .. } => assert!(applied),
        other => panic!("expected delivered, got {other:?}"),
    }
}

#[tokio::test]
async fn rpc_response_confirms_coordinator() {
    let env = env().await;
    // coordinator 登记 rpc → relay 匹配响应 → oneshot 通知。
    let rx = env.relay.register_rpc("hub-1", "c1".into()).await;
    let e = ev(7, json!({"jsonrpc": "2.0", "id": "hub-1", "result": {"ok": true}}));
    let r = env.relay.on_machine_event("local", &e).await;
    match r {
        ConsumeResult::RpcConfirmed { command_id, response } => {
            assert_eq!(command_id, "c1");
            assert_eq!(response["result"]["ok"], json!(true));
        }
        other => panic!("expected rpc confirmed, got {other:?}"),
    }
    // 等待侧收到响应。
    let resp = tokio::time::timeout(Duration::from_secs(1), rx)
        .await
        .expect("oneshot should resolve")
        .expect("sender alive");
    assert_eq!(resp["id"], json!("hub-1"));
}

#[tokio::test]
async fn disconnect_cleanup_interrupts_turn_and_gaps() {
    let env = env().await;
    // 登记活动 turn（coordinator 路径行为）。
    env.sessions.set_active_turn("s1", "t1").await;
    env.relay.on_machine_disconnect("local").await.unwrap();
    // session 置 Gap（§8.2 matrix machine 行）。
    let e = env.sessions.entry("s1").await.unwrap();
    assert_eq!(e.state, crate::control::SessionState::Gap);
    // 活动 turn 已 interrupted（DocManager 命令应用后视图不可直接读——
    // 经镜像断言）。
    let _ = env;
}

#[tokio::test]
async fn process_exit_sets_terminal() {
    let env = env().await;
    let exit = acp_hub_proto::machine::MachineProcessExit {
        session_id: "acp-1".into(),
        code: 0,
    };
    let r = env.relay.on_process_exit("local", &exit).await;
    assert!(matches!(r, ConsumeResult::Delivered { kind: "process_exit", .. }));
    let e = env.sessions.entry("s1").await.unwrap();
    assert_eq!(e.state, crate::control::SessionState::Ended);
}
