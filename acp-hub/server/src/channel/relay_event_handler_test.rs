//! RelayEventHandler 单测（设计稿 §16 测试 15–17）。
//!
//! 事件 epoch 用 0（F4 聚合器 stream.epoch 以 0 起始；真实 instance 的
//! epoch=1 首事件触发 uncalibratable 缺口为 F4 已知缺口，见输出遗留问题）。

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use acp_hub_proto::instance::{
    BufferedFrame, InstanceBufferSync, InstanceEvent, InstanceHello,
};

use crate::channel::{ConsumeResult, RelayEventHandler};
use crate::control::StoreSink;
use crate::control::{InstanceConn, InstanceRegistry};
use crate::control::ChatRegistry;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager};

/// 测试 chat id（UUID 形态——StoreSink 落盘按 UUID 解析 chat 归属；非 UUID
/// 的 chat id 在控制类事件落盘时返回 PersistFailed）。
const S1: &str = "00000000-0000-0000-0000-000000000001";

struct Env {
    _tmp: tempfile::TempDir,
    chats: ChatRegistry,
    relay: Arc<RelayEventHandler>,
    sink: Arc<StoreSink>,
    /// instance 注册表（uncalibratable 测试需重新 hello 对账 epoch）。
    instance: Arc<InstanceRegistry>,
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
    let doc = Arc::new(DocManager::new(BatchConfig::default(), sink.clone()));
    let registry = doc.registry();
    let chats = ChatRegistry::new(registry);
    let instance = Arc::new(InstanceRegistry::new(
        Duration::from_secs(30),
        Duration::from_secs(1),
        chats.clone(),
    ));
    let relay = Arc::new(RelayEventHandler::new(
        doc.clone(),
        chats.clone(),
        instance.clone(),
        doc.registry(),
    ));
    // instance 上线（hello 登记 session_epochs: s1 → 0；instance 侧按进程
    // 归属 = hub session id 上报，§4.5.1）。
    let hello = InstanceHello {
        token: "tok".into(),
        hostname: "local".into(),
        caps: json!({}),
        buffered: None,
        buffer_lost: None,
        stream_epochs: Some([(S1.to_string(), 0u64)].into_iter().collect()),
        nonce: "AAAA".into(),
    };
    let (tx, _rx) = mpsc::channel(8);
    instance
        .on_hello("local", "tok-m", InstanceConn { tx }, &hello)
        .await;
    // 打开 session + binding（relay 投递前提）。chat store 记录须存在
    // （StoreSink 落盘按 chat 归属解析；生产中由 create 流程建立）。
    let _ = store.create_chat(uuid::Uuid::parse_str(S1).unwrap());
    doc.open_chat(S1, "local", Some("t"), None, None).await.unwrap();
    chats.register(S1, "local", Some("t"), "/", None).await.unwrap();
    chats.bind(S1, "acp-1").await.unwrap();
    Env {
        _tmp: tmp,
        chats,
        relay,
        sink,
        instance,
    }
}

/// instance/event 信封：chat_id = 进程归属（hub session id，§4.5.1）；
/// 帧内 sessionId = acp_session_id（binding 校验键，§495）。
fn ev(seq: u64, frame: serde_json::Value) -> InstanceEvent {
    InstanceEvent {
        chat_id: S1.into(),
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
    let r = env.relay.on_instance_event("local", &e).await;
    assert!(matches!(r, ConsumeResult::Dropped { reason: "epoch_mismatch" }));
}

#[tokio::test]
async fn binding_missing_dropped() {
    let env = env().await;
    // 帧内 sessionId 未命中可信 binding（acp-unknown 无映射）→ 丢弃。
    let e = ev(
        1,
        json!({
            "type": "agent_message_chunk",
            "sessionId": "acp-unknown",
            "payload": {}
        }),
    );
    let r = env.relay.on_instance_event("local", &e).await;
    assert!(matches!(r, ConsumeResult::Dropped { reason: "binding_missing" }));
}

#[tokio::test]
async fn binding_mismatch_dropped() {
    let env = env().await;
    // 帧内 sessionId 命中 binding（acp-1 → s1）但信封是另一 session（s2）：
    // 帧与进程归属不符 → 丢弃（§6.1 规则 5「与 binding 不一致直接丢弃」）。
    let mut e = ev(
        1,
        json!({
            "type": "agent_message_chunk",
            "sessionId": "acp-1",
            "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}
        }),
    );
    e.chat_id = "s2".into();
    let r = env.relay.on_instance_event("local", &e).await;
    assert!(matches!(r, ConsumeResult::Dropped { reason: "binding_missing" }));
}

#[tokio::test]
async fn event_delivered_to_aggregator() {
    let env = env().await;
    let e = ev(
        1,
        json!({
            "type": "agent_message_chunk",
            "sessionId": "acp-1",
            "payload": {"turnId": "t1", "entryId": "t1:assistant", "blockId": "b1", "text": "hello"}
        }),
    );
    let r = env.relay.on_instance_event("local", &e).await;
    match r {
        ConsumeResult::Delivered { chat_id, kind, seq, applied } => {
            assert_eq!(chat_id, S1);
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
    // binding 命中（帧内 acp-1 → s1）→ 正常 normalize → 未知 type 计数。
    let e = ev(
        2,
        json!({
            "type": "unknown_frame",
            "sessionId": "acp-1",
            "payload": {}
        }),
    );
    let r = env.relay.on_instance_event("local", &e).await;
    assert!(matches!(r, ConsumeResult::Dropped { reason: "unsupported_frame" }));
    assert!(env.relay.dropped_total() >= 1);
}

#[tokio::test]
async fn buffer_sync_epoch_mismatch_rejects_batch() {
    let env = env().await;
    let sync = InstanceBufferSync {
        chat_id: S1.into(),
        epoch: 1, // hello 登记 0
        from_seq: 1,
        frames: vec![BufferedFrame {
            seq: 1,
            frame: json!({
                "type": "agent_message_chunk",
                "sessionId": "acp-1",
                "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}
            }),
        }],
    };
    let r = env.relay.on_buffer_sync("local", &sync).await;
    assert!(matches!(r, ConsumeResult::BatchRejected { reason: "buffer_sync_epoch_mismatch" }));
}

#[tokio::test]
async fn buffer_sync_out_of_order_frames_dropped() {
    let env = env().await;
    // from_seq=1，但帧 seq 从 2 开始（跳号）→ 乱序丢弃计数。
    let sync = InstanceBufferSync {
        chat_id: S1.into(),
        epoch: 0,
        from_seq: 1,
        frames: vec![BufferedFrame {
            seq: 2,
            frame: json!({
                "type": "agent_message_chunk",
                "sessionId": "acp-1",
                "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}
            }),
        }],
    };
    let r = env.relay.on_buffer_sync("local", &sync).await;
    assert!(matches!(r, ConsumeResult::BatchRejected { reason: "all_frames_rejected" }));
}

#[tokio::test]
async fn buffer_sync_contiguous_delivered() {
    let env = env().await;
    let sync = InstanceBufferSync {
        chat_id: S1.into(),
        epoch: 0,
        from_seq: 5,
        frames: vec![
            BufferedFrame {
                seq: 5,
                frame: json!({
                    "type": "agent_message_chunk",
                    "sessionId": "acp-1",
                    "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "a"}
                }),
            },
            BufferedFrame {
                seq: 6,
                frame: json!({
                    "type": "agent_message_chunk",
                    "sessionId": "acp-1",
                    "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "b"}
                }),
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
    let r = env.relay.on_instance_event("local", &e).await;
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
    // 投影活动 turn（session doc active_turn 建立：permission 关联检查前提）
    // + 一条 pending 权限（断链清理输入，§7.1）。
    let r = env
        .relay
        .on_instance_event(
            "local",
            &ev(1, json!({"type": "user_message_chunk", "sessionId": "acp-1", "payload": {"turnId": "t1", "entryId": "t1:user", "text": "你好"}})),
        )
        .await;
    assert!(matches!(r, ConsumeResult::Delivered { applied: true, .. }));
    let r = env
        .relay
        .on_instance_event(
            "local",
            &ev(2, json!({"type": "permission_request", "sessionId": "acp-1", "payload": {"permissionId": "p1", "turnId": "t1", "title": "允许执行", "options": ["allowOnce"]}})),
        )
        .await;
    assert!(matches!(r, ConsumeResult::Delivered { applied: true, .. }));
    // 登记活动 turn（coordinator 路径行为）。
    env.chats.set_active_turn(S1, "t1").await;
    env.relay.on_instance_disconnect("local").await.unwrap();
    // session 置 Gap（§8.2 matrix instance 行）。
    let e = env.chats.entry(S1).await.unwrap();
    assert_eq!(e.state, crate::control::ChatState::Gap);
    // 断链清理生效：活动 turn interrupted + pending 权限批量 expired
    // （StoreSink 镜像快照 → 应用断言，与 doc_manager 测试同款 mirror）。
    let (snapshot, _) = env
        .sink
        .snapshot(&acp_hub_proto::conn::DocId::session(S1))
        .await
        .expect("session 镜像快照");
    use yrs::updates::decoder::Decode as _;
    use yrs::{Map as _, ReadTxn as _, Transact as _};
    let mirror = yrs::Doc::new();
    let parsed = yrs::Update::decode_v1(&snapshot).unwrap();
    mirror.transact_mut().apply_update(parsed).unwrap();
    let txn = mirror.transact();
    let root = txn.get_map("root").unwrap();
    let sm = root
        .get(&txn, "session")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    assert_eq!(
        sm.get(&txn, "active_turn_status").unwrap().cast::<String>().unwrap(),
        "interrupted",
        "断链 → 活动 turn interrupted（§7.1）"
    );
    let perms = root
        .get(&txn, "pending_permissions")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    let pm = perms.get(&txn, "p1").unwrap().cast::<yrs::MapRef>().unwrap();
    assert_eq!(
        pm.get(&txn, "status").unwrap().cast::<String>().unwrap(),
        "expired",
        "断链 → pending 权限批量过期（§7.1 expireTurnPermissions）"
    );
}

#[tokio::test]
async fn process_exit_sets_terminal() {
    let env = env().await;
    let exit = acp_hub_proto::instance::InstanceProcessExit {
        chat_id: S1.into(),
        code: 0,
    };
    let r = env.relay.on_process_exit("local", &exit).await;
    assert!(matches!(r, ConsumeResult::Delivered { kind: "process_exit", .. }));
    let e = env.chats.entry(S1).await.unwrap();
    assert_eq!(e.state, crate::control::ChatState::Ended);
}

// ---------------------------------------------------------------------------
// 断链追平恢复（§7.3/§8.5）：断链 → ChatState Gap + registry gap 占位 →
// buffer_sync 补推 → 恢复 Accepting + gap 清除；不可校准（epoch 变化，
// §4.5.1）→ 保持 Gap（只能经 session/load 显式重建消除）
// ---------------------------------------------------------------------------

/// 读取 Registry Doc 镜像中 `chats[S1].gap`：`Some(None)` = 追平（Null/
/// 缺字段）、`Some(Some(n))` = 缺口占位/计数、`None` = chat 条目缺失。
async fn registry_chat_gap(env: &Env) -> Option<Option<f64>> {
    let (snapshot, _) = env
        .sink
        .snapshot(&acp_hub_proto::conn::DocId::REGISTRY)
        .await
        .expect("registry 镜像快照");
    use yrs::updates::decoder::Decode as _;
    use yrs::{Map as _, ReadTxn as _, Transact as _};
    let mirror = yrs::Doc::new();
    let parsed = yrs::Update::decode_v1(&snapshot).unwrap();
    mirror.transact_mut().apply_update(parsed).unwrap();
    let txn = mirror.transact();
    let root = txn.get_map("root").unwrap();
    let chats = root
        .get(&txn, "chats")
        .unwrap()
        .cast::<yrs::MapRef>()
        .unwrap();
    let sm = chats.get(&txn, S1)?.cast::<yrs::MapRef>().ok()?;
    match sm.get(&txn, "gap") {
        None | Some(yrs::Out::Any(yrs::Any::Null)) => Some(None),
        Some(yrs::Out::Any(yrs::Any::Number(n))) => Some(Some(n)),
        other => panic!("unexpected gap value: {other:?}"),
    }
}

#[tokio::test]
async fn buffer_sync_after_disconnect_recovers_chat_from_gap() {
    let env = env().await;
    // 活动 turn 基线（user_message 建立 active_turn；seq=1）。
    let r = env
        .relay
        .on_instance_event(
            "local",
            &ev(1, json!({"type": "user_message_chunk", "sessionId": "acp-1", "payload": {"turnId": "t1", "entryId": "t1:user", "text": "你好"}})),
        )
        .await;
    assert!(matches!(r, ConsumeResult::Delivered { applied: true, .. }));
    env.chats.set_active_turn(S1, "t1").await;
    // 断链 → ChatState Gap + registry gap 占位 Some(0)（缺口数量由补推时
    // 聚合器精确计算，§8.2/§7.3）。
    env.relay.on_instance_disconnect("local").await.unwrap();
    assert_eq!(
        env.chats.entry(S1).await.unwrap().state,
        crate::control::ChatState::Gap
    );
    assert_eq!(
        registry_chat_gap(&env).await,
        Some(Some(0.0)),
        "断链 → registry gap 占位"
    );
    // 补推（from_seq = server last_seq + 1 = 2）：delta 帧——断链后
    // active_turn 已 interrupted，聚合器拒该帧（applied=false）；relay
    // 以投递成功计数（delta 入队即返），恢复判定在 writer 内以聚合器
    // 事实源进行（ResumeAfterGap 可校准 → 追平）。
    let sync = InstanceBufferSync {
        chat_id: S1.into(),
        epoch: 0,
        from_seq: 2,
        frames: vec![BufferedFrame {
            seq: 2,
            frame: json!({
                "type": "agent_message_chunk",
                "sessionId": "acp-1",
                "payload": {"turnId": "t1", "entryId": "e", "blockId": "b", "text": "x"}
            }),
        }],
    };
    let r = env.relay.on_buffer_sync("local", &sync).await;
    assert!(matches!(r, ConsumeResult::Delivered { .. }), "补推投递成功");
    // 恢复：ChatState Gap → Accepting（可开新 turn）+ registry gap 清除。
    assert_eq!(
        env.chats.entry(S1).await.unwrap().state,
        crate::control::ChatState::Accepting,
        "补推追平 → 恢复 Accepting（§7.3）"
    );
    assert_eq!(
        registry_chat_gap(&env).await,
        Some(None),
        "追平 → registry gap 标记清除"
    );
}

#[tokio::test]
async fn buffer_sync_uncalibratable_keeps_gap() {
    let env = env().await;
    // 流基线（last_seq=1）。
    let r = env
        .relay
        .on_instance_event(
            "local",
            &ev(1, json!({"type": "user_message_chunk", "sessionId": "acp-1", "payload": {"turnId": "t1", "entryId": "t1:user", "text": "你好"}})),
        )
        .await;
    assert!(matches!(r, ConsumeResult::Delivered { applied: true, .. }));
    env.chats.set_active_turn(S1, "t1").await;
    // daemon 重启：新 hello 幂等替换 chat_epochs（s1 → 1，§4.5.1）——
    // relay epoch 校验放行 epoch=1 帧。
    let hello = InstanceHello {
        token: "tok".into(),
        hostname: "local".into(),
        caps: json!({}),
        buffered: None,
        buffer_lost: None,
        stream_epochs: Some([(S1.to_string(), 1u64)].into_iter().collect()),
        nonce: "BBBB".into(),
    };
    let (tx, _rx) = mpsc::channel(8);
    env.instance
        .on_hello("local", "tok-m", InstanceConn { tx }, &hello)
        .await;
    // epoch 变化帧：聚合器置不可校准缺口并拒绝投影（§4.5.1；补推契约
    // 失效——历史缓冲无法校准）。
    let mut e = ev(
        2,
        json!({"type": "user_message_chunk", "sessionId": "acp-1", "payload": {"turnId": "t2", "entryId": "t2:user", "text": "hi"}}),
    );
    e.epoch = 1;
    let r = env.relay.on_instance_event("local", &e).await;
    assert!(
        matches!(r, ConsumeResult::Delivered { applied: false, .. }),
        "epoch 变化帧应被聚合器拒绝（uncalibratable）"
    );
    // 断链 → Gap + registry gap 占位。
    env.relay.on_instance_disconnect("local").await.unwrap();
    assert_eq!(
        env.chats.entry(S1).await.unwrap().state,
        crate::control::ChatState::Gap
    );
    // 补推（epoch=1, from_seq=2）：帧被聚合器拒（UncalibratableGap）——
    // relay 以投递成功计数仍会尝试恢复，但 writer 内 ResumeAfterGap 检查
    // stream.uncalibratable → Rejected → 不迁移、不误标追平。
    let sync = InstanceBufferSync {
        chat_id: S1.into(),
        epoch: 1,
        from_seq: 2,
        frames: vec![BufferedFrame {
            seq: 2,
            frame: json!({
                "type": "agent_message_chunk",
                "sessionId": "acp-1",
                "payload": {"turnId": "t2", "entryId": "e", "blockId": "b", "text": "x"}
            }),
        }],
    };
    let r = env.relay.on_buffer_sync("local", &sync).await;
    assert!(matches!(r, ConsumeResult::Delivered { .. }), "补推帧投递（聚合器拒绝，relay 不感知）");
    // 保持 Gap + gap 占位（不可校准缺口只能经 session/load 显式重建消除）。
    assert_eq!(
        env.chats.entry(S1).await.unwrap().state,
        crate::control::ChatState::Gap,
        "uncalibratable 拒绝恢复"
    );
    assert_eq!(
        registry_chat_gap(&env).await,
        Some(Some(0.0)),
        "gap 标记保留（不得误标为已追平）"
    );
}
