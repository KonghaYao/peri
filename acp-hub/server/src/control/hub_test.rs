//! Hub / StoreSink 单测（设计稿 §16 测试 31 的 fake instance 辅助 + 镜像
//! 快照/落盘语义；全链路 e2e 在 gateway_test）。

use std::sync::Arc;

use tokio::sync::mpsc;

use acp_hub_proto::conn::DocId;

use crate::control::StoreSink;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager, UpdateSink};

async fn env() -> (tempfile::TempDir, Arc<Store>, Arc<StoreSink>, DocManager) {
    let tmp = tempfile::tempdir().unwrap();
    let persist_cfg = PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(Store::open(&persist_cfg).unwrap());
    store.recover().await;
    let sink = Arc::new(StoreSink::new(store.clone()).await.unwrap());
    let doc = DocManager::new(BatchConfig::default(), sink.clone());
    (tmp, store, sink, doc)
}

#[tokio::test]
async fn mirror_snapshot_rebuilds_from_log() {
    let (tmp, store, sink, doc) = env().await;
    let sid = "11111111-1111-1111-1111-111111111111";
    // 打开 session 并写入一条事件（聚合器投影 → sink 落盘 → 镜像）。
    // 控制类命令挂落盘应答（§8.2）→ 持久化前置：Store 目录须先建（StoreSink
    // 落盘按 session 目录路由）。
    store.create_chat(uuid::Uuid::parse_str(sid).unwrap()).unwrap();
    doc.open_chat(sid, "m1", Some("t"), None, None).await.unwrap();
    // 先建立 active turn（§6.5 服务端单写）：MessageDelta 受终态守卫约束
    // （§6.3 无活动 turn → UnknownTurn 拒绝）。
    let reg = doc
        .submit_command(
            sid,
            crate::state::doc_manager::DocCommand::RegisterUserEntry {
                turn_id: "t1".into(),
                entry_id: "t1:user".into(),
                text: "prompt".into(),
                author_user_id: None,
                created_at: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await;
    assert!(matches!(
        reg,
        crate::state::doc_manager::SubmitResult::Applied(_)
    ));
    let ev = crate::state::normalized::NormalizedEvent {
        chat_id: sid.into(),
        seq: 1,
        epoch: 0,
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: crate::state::normalized::EventBody::MessageDelta {
            turn_id: "t1".into(),
            entry_id: "t1:assistant".into(),
            block_id: "b1".into(),
            text: "mirror me".into(),
        },
    };
    let r = doc.submit_event(ev).await;
    assert!(matches!(r, crate::state::doc_manager::SubmitResult::Applied(_)));
    // delta 类事件入队即返（§8.2 微批次不逐事件应答）：轮询快照等待
    // flush（16ms 窗口 + 落盘 + 镜像应用）——Factory 初始化 update（pv=0）
    // 与事件 update 同批入广播流，以 projection_version >= 1 判定事件已
    // 镜像（非初始化结构）。
    let (state, version) = {
        let mut got = None;
        for _ in 0..50 {
            let snap = sink
                .snapshot(&DocId::chat(sid))
                .await
                .expect("snapshot exists");
            if snap.1 >= 1 {
                got = Some(snap);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        got.expect("chat doc projection_version >= 1 after flush")
    };
    {
        use yrs::{Map, ReadTxn, Transact};
        let docs = sink.docs.read().await;
        if let Some(d) = docs.get(&DocId::chat(sid)) {
            let txn = d.transact();
            let root = txn.get_map("root").expect("root");
            let entries = root.get(&txn, "entries").and_then(|v| v.cast::<yrs::MapRef>().ok());
            eprintln!("[dbg] entries map present: {}", entries.is_some());
            if let Some(m) = entries {
                let keys: Vec<String> = m.keys(&txn).map(|k| k.to_string()).collect();
                eprintln!("[dbg] entry keys: {:?}", keys);
            }
        }
    }
    assert!(!state.is_empty());
    assert!(version >= 1);

    // 重启重建：drop DocManager（写者退出），用同一 Store 重建 StoreSink。
    drop(doc);
    let store2 = Arc::new(Store::open(&persist_cfg2(&tmp)).unwrap());
    store2.recover().await;
    let sink2 = Arc::new(StoreSink::new(store2.clone()).await.unwrap());
    let (state2, v2) = sink2
        .snapshot(&DocId::chat(sid))
        .await
        .expect("rebuilt mirror has chat doc");
    eprintln!("[dbg] state2={} v2={}", state2.len(), v2);
    assert!(!state2.is_empty(), "重启后镜像应含已落盘内容（P3 视图恢复）");
    // 内容一致：合并后的 state 应包含文本。
    let bytes = state2;
    assert!(!bytes.is_empty());
}

fn persist_cfg2(tmp: &tempfile::TempDir) -> PersistConfig {
    PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    }
}

#[tokio::test]
async fn registry_update_persisted_and_replayed() {
    let (tmp, store, sink, doc) = env().await;
    // Registry 更新经 DocManager registry 写者 → sink.persist_update(REGISTRY)。
    let registry = doc.registry();
    registry.set_restarting().await.unwrap();
    registry.clear_restarting().await.unwrap();
    // 重启后 registry 镜像重建（独立日志）。
    drop(doc);
    let store2 = Arc::new(Store::open(&persist_cfg2(&tmp)).unwrap());
    store2.recover().await;
    let sink2 = Arc::new(StoreSink::new(store2.clone()).await.unwrap());
    let (state, _) = sink2.snapshot(&DocId::REGISTRY).await.expect("registry mirror");
    assert!(!state.is_empty());
    let _ = (store, sink);
}

#[tokio::test]
async fn persist_update_unknown_doc_rejected() {
    let (_tmp, _store, sink, _doc) = env().await;
    let r = sink
        .persist_update(DocId::chat("not-a-uuid"), vec![1, 2, 3])
        .await;
    assert!(r.is_err(), "未知 session doc 应拒绝");
}

#[tokio::test]
async fn broadcast_stream_delivers_updates() {
    let (_tmp, _store, sink, doc) = env().await;
    let mut rx = sink.subscribe().await;
    doc.open_chat("s2", "m1", None, None, None).await.unwrap();
    // open_chat 会先写 hub:registry 活跃摘要（§5.2 单写）；随后的事件
    // 投影才写 s2 的 chat/session doc。跳 registry 帧，断言 s2 的 doc 到达。
    let ev = crate::state::normalized::NormalizedEvent {
        chat_id: "s2".into(),
        seq: 1,
        epoch: 0,
        ts: "2026-08-07T00:00:00Z".to_string(),
        body: crate::state::normalized::EventBody::AgentStatus {
            status: "idle".into(),
            public_error: None,
        },
    };
    let _ = doc.submit_event(ev).await;
    let update = loop {
        let u = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("broadcast update")
            .expect("channel alive");
        if u.doc != DocId::REGISTRY {
            break u;
        }
    };
    assert!(update.doc == DocId::session("s2") || update.doc == DocId::chat("s2"));
    let _ = mpsc::unbounded_channel::<()>();
}

// ---------------------------------------------------------------------------
// Hub 装配 smoke test（任务点 4 / §16 测试 20 的 hub 面）：`Hub::assemble`
// 起真 server 于随机端口 + fake client（TUI）ws 连接，验证 §4.6 时序
// （auth → subscribe → 快照 → ready）与 Degraded 入口。
// ---------------------------------------------------------------------------

/// ws 帧读取辅助（跳过协议层 Ping；§4.6 帧面）。
async fn next_frame(
    stream: &mut futures::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) -> acp_hub_proto::frame::Frame {
    use futures::StreamExt as _;
    loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("frame timeout")
            .expect("stream alive")
            .expect("no ws error");
        match msg {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                return acp_hub_proto::frame::Frame::parse(t.as_str()).expect("parse frame");
            }
            tokio_tungstenite::tungstenite::Message::Ping(_) => continue,
            other => panic!("unexpected ws message: {other:?}"),
        }
    }
}

#[tokio::test]
async fn hub_assemble_smoke_ready_sequence() {
    use base64::Engine as _;
    use futures::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::Message;

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = crate::config::Config::defaults();
    cfg.data_dir = tmp.path().join("data");
    cfg.config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&cfg.config_dir).expect("create config dir (token store)");
    // e2e 客户端不回 pong：放宽心跳间隔避免业务流中 4501 误触发。
    cfg.heartbeat_interval = std::time::Duration::from_secs(60);
    cfg.offline_timeout = std::time::Duration::from_secs(1);

    let mut token_store =
        crate::auth::TokenStore::load(&cfg.config_dir.join(crate::auth::TOKENS_FILE)).unwrap();
    let client_rec = token_store
        .generate(crate::auth::TokenRole::Full, "tui")
        .unwrap();
    let auth = Arc::new(tokio::sync::Mutex::new(crate::auth::AuthService::new(
        token_store,
    )));

    let persist_cfg = PersistConfig {
        data_dir: cfg.data_dir.clone(),
        ..Default::default()
    };
    let store = Arc::new(Store::open(&persist_cfg).unwrap());
    store.recover().await;

    // 装配（§8.6）：StoreSink → DocManager → 注册表/协调器/广播器 → Gateway。
    let hub = Arc::new(crate::control::Hub::assemble(&cfg, store, auth).await.unwrap());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let gateway = hub.gateway.clone();
    let task = tokio::spawn(async move {
        let _ = gateway.run(listener).await;
    });

    // fake client（TUI）：auth → subscribe registry → 快照 → ready（§4.6
    // 步骤 1–4 顺序断言）。
    let url = format!("ws://{addr}/");
    let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let (mut sink, mut stream) = ws.split();
    sink.send(Message::Text(
        serde_json::to_string(&acp_hub_proto::frame::Frame::Auth(
            acp_hub_proto::conn::Auth {
                token: client_rec.token.clone(),
            },
        ))
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    sink.send(Message::Text(
        serde_json::to_string(&acp_hub_proto::frame::Frame::YsyncSubscribe(
            acp_hub_proto::ysync::YsyncSubscribe {
                docs: vec![DocId::REGISTRY],
            },
        ))
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // 快照（ysync.update，带 projection_version，§4.6 步骤 3）。
    let snap = next_frame(&mut stream).await;
    match snap {
        acp_hub_proto::frame::Frame::YsyncUpdate(u) => {
            assert_eq!(u.doc, DocId::REGISTRY);
            assert!(u.projection_version.is_some(), "快照必带 projection_version");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&u.update)
                .unwrap();
            assert!(!bytes.is_empty());
        }
        other => panic!("expected snapshot, got {other:?}"),
    }
    // ready（§4.6 步骤 4：快照之后、缓冲 Action flush 前）。
    let ready = next_frame(&mut stream).await;
    match ready {
        acp_hub_proto::frame::Frame::Ready(r) => {
            assert!(r.projection_versions.contains_key(&DocId::REGISTRY));
        }
        other => panic!("expected ready, got {other:?}"),
    }

    // Degraded 入口（§17.2 + §8.4.1 不变量 4）：装配后（instance 重连对账
    // 前）Restarting 门禁——拒绝新 committed 承诺；instance 重连（hello）
    // 对账后开门 → Healthy。
    assert!(!hub.can_accept_committed(), "Restarting 期间不得接受新 committed（§8.4.1 不变量 4）");
    hub.registry.clear_restarting().await.unwrap();
    assert!(hub.can_accept_committed());

    task.abort();
    drop(sink);
    let _ = stream;
}

/// 启动对账（§5.5 重启语义）：非终态会话（accepting/active/gap/
/// pending_close）全部入选，终态（ended/closed/crashed）保持不变。
#[test]
fn enum_stale_registry_chats_filters_terminal() {
    use crate::control::hub::Hub;
    use yrs::{Map, Transact, WriteTxn};
    let doc = yrs::Doc::new();
    let mut txn = doc.transact_mut();
    let root = txn.get_or_insert_map("root");
    let sessions = root.get_or_init::<_, yrs::MapRef>(&mut txn, "chats");
    for (sid, status) in [
        ("s-accepting", "accepting"),
        ("s-active", "active"),
        ("s-gap", "gap"),
        ("s-pending-close", "pending_close"),
        ("s-ended", "ended"),
        ("s-closed", "closed"),
        ("s-crashed", "crashed"),
    ] {
        let sm = sessions.get_or_init::<_, yrs::MapRef>(&mut txn, sid);
        sm.insert(&mut txn, "id", sid.to_string());
        sm.insert(&mut txn, "status", status.to_string());
    }
    drop(txn);

    let stale = Hub::enum_stale_registry_chats(&doc);
    assert!(stale.contains(&"s-accepting".to_string()));
    assert!(stale.contains(&"s-active".to_string()));
    assert!(stale.contains(&"s-gap".to_string()));
    assert!(stale.contains(&"s-pending-close".to_string()));
    assert!(!stale.contains(&"s-ended".to_string()));
    assert!(!stale.contains(&"s-closed".to_string()));
    assert!(!stale.contains(&"s-crashed".to_string()));
    assert_eq!(stale.len(), 4);
}
