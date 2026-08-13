//! InstanceRegistry 单测（设计稿 §16 测试 24–27）。

use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;

use acp_hub_proto::instance::{InstanceForwardAck, InstanceHello, InstanceSpawn, InstanceSpawnAck};

use super::*;
use crate::channel::OutboundMsg;
use crate::control::chat_registry::ChatRegistry;
use crate::state::registry::RegistryState;

/// 无 Registry 的 ChatRegistry 替身（测试不写 Registry Doc）。
fn test_registry() -> (RegistryState, mpsc::Receiver<()>) {
    // 直接构造一个「死 registry」：channel closed → 方法返回 Err，测试避免
    // 依赖 Registry。
    let (rtx, _rrx) = mpsc::channel::<crate::state::registry::RegistryMsg>(8);
    let registry = RegistryState::new(rtx);
    let (_drop_tx, drop_rx) = mpsc::channel::<()>(8);
    (registry, drop_rx)
}

fn hello(instance_id: &str) -> InstanceHello {
    InstanceHello {
        token: "tok".into(),
        hostname: instance_id.into(),
        caps: json!({}),
        buffered: None,
        buffer_lost: None,
        stream_epochs: None,
        nonce: "AAAA".into(),
    }
}

#[tokio::test]
async fn lifecycle_hello_heartbeat_offline() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_secs(10), chats);
    let (tx, _rx) = mpsc::channel(8);
    let outcome = reg
        .on_hello("m1", "tok-1", InstanceConn { tx }, &hello("m1"))
        .await;
    assert!(!outcome.fenced_previous);
    assert_eq!(reg.state("m1").await, Some(InstanceState::Online));

    // 心跳续期。
    reg.on_heartbeat(
        "m1",
        &InstanceHeartbeat {
            load: 10,
            alive_sessions: vec![],
        },
    )
    .await
    .unwrap();

    // 30s 无心跳 → OFFLINE（注入时钟）。
    let t0 = Instant::now();
    assert!(reg
        .sweep_offline(t0 + Duration::from_secs(29))
        .await
        .is_empty());
    let offline = reg.sweep_offline(t0 + Duration::from_secs(31)).await;
    assert_eq!(offline, vec!["m1".to_string()]);
    assert_eq!(reg.state("m1").await, Some(InstanceState::Offline));

    // 重连心跳 → ONLINE。
    reg.on_heartbeat(
        "m1",
        &InstanceHeartbeat {
            load: 0,
            alive_sessions: vec![],
        },
    )
    .await
    .unwrap();
    assert_eq!(reg.state("m1").await, Some(InstanceState::Online));
    let _ = _drop;
}

#[tokio::test]
async fn hello_fencing_replaces_connection() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_secs(10), chats);
    let (tx1, mut rx1) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx: tx1 }, &hello("m1"))
        .await;
    // 新 hello（同 instance_id）→ 旧连接 fencing（关闭信号）。
    let (tx2, _rx2) = mpsc::channel(8);
    let outcome = reg
        .on_hello("m1", "tok-1", InstanceConn { tx: tx2 }, &hello("m1"))
        .await;
    assert!(outcome.fenced_previous);
    // 旧连接收到 Close(1011)。
    let msg = rx1.recv().await.expect("old connection should be closed");
    assert!(matches!(msg, OutboundMsg::Close(1011)));
    let _ = _drop;
}

#[tokio::test]
async fn spawn_ack_tracking_and_timeout() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    // 指令超时用极短值。
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_millis(50), chats);
    let (tx, mut rx) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx }, &hello("m1"))
        .await;

    let spawn_cmd = InstanceSpawn {
        command_id: "c1".into(),
        chat_id: "s1".into(),
        cmd: vec!["peri".into()],
        cwd: "/".into(),
        env: None,
    };
    // 无 ack → 超时（AgentUnavailable 语义）。
    let spawned = tokio::spawn({
        let reg = reg.clone();
        let cmd = spawn_cmd.clone();
        async move { reg.send_spawn("m1", cmd).await }
    });
    // instance 侧先收到 spawn 帧。
    match rx.recv().await {
        Some(OutboundMsg::Frame(Frame::InstanceSpawn(_))) => {}
        other => panic!("expected spawn frame, got {other:?}"),
    }
    // 回 ack → 回填成功。
    let r = reg
        .on_ack(
            "m1",
            "c1",
            InstanceAck::Spawn(InstanceSpawnAck {
                command_id: "c1".into(),
                chat_id: "s1".into(),
                ok: true,
                error: None,
            }),
        )
        .await;
    assert!(r);
    let result = spawned.await.unwrap().unwrap();
    match result {
        SpawnOutcome::Acked(a) => assert!(a.ok),
    }
    // 超时路径。
    let cmd2 = InstanceSpawn {
        command_id: "c2".into(),
        chat_id: "s2".into(),
        cmd: vec!["peri".into()],
        cwd: "/".into(),
        env: None,
    };
    let r2 = reg.send_spawn("m1", cmd2).await;
    assert!(matches!(r2, Err(InstanceError::Timeout)));
    let _ = _drop;
}

#[tokio::test]
async fn offline_instance_rejects_commands() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_secs(10), chats);
    let (tx, _rx) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx: tx.clone() }, &hello("m1"))
        .await;
    // 断开（当前连接句柄）→ OFFLINE。
    assert!(reg.on_disconnect("m1", &InstanceConn { tx }).await);
    let cmd = InstanceSpawn {
        command_id: "c1".into(),
        chat_id: "s1".into(),
        cmd: vec!["peri".into()],
        cwd: "/".into(),
        env: None,
    };
    assert!(matches!(
        reg.send_spawn("m1", cmd).await,
        Err(InstanceError::Offline)
    ));
    let _ = _drop;
}

#[tokio::test]
async fn forward_rpc_and_offline() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_secs(10), chats);
    let (tx, mut rx) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx: tx.clone() }, &hello("m1"))
        .await;
    let msg = json!({"jsonrpc":"2.0","id":"hub-1","method":"session/prompt","params":{}});
    // 回 forward_ack（L1+L2 确认）：测试侧等 rx 收到 forward 帧后 on_ack。
    let reg2 = reg.clone();
    let ack_task = tokio::spawn(async move {
        match rx.recv().await {
            Some(OutboundMsg::Frame(Frame::InstanceForward(f))) => {
                assert_eq!(f.command_id, "hub-1");
                assert_eq!(f.chat_id, "s1");
                assert_eq!(f.frame["id"], json!("hub-1"));
                reg2.on_ack(
                    "m1",
                    &f.command_id,
                    InstanceAck::Forward(InstanceForwardAck {
                        command_id: f.command_id.clone(),
                        chat_id: f.chat_id.clone(),
                        ok: true,
                        error: None,
                    }),
                )
                .await;
            }
            other => panic!("expected forward frame, got {other:?}"),
        }
    });
    reg.forward_rpc("m1", "s1", &msg).await.unwrap();
    ack_task.await.unwrap();
    // 未知 instance。
    assert!(matches!(
        reg.forward_rpc("nope", "s1", &msg).await,
        Err(InstanceError::UnknownInstance(_))
    ));
    // 断开后 OFFLINE。
    reg.on_disconnect("m1", &InstanceConn { tx: tx.clone() })
        .await;
    assert!(matches!(
        reg.forward_rpc("m1", "s1", &msg).await,
        Err(InstanceError::Offline)
    ));
    let _ = _drop;
}

#[tokio::test]
async fn chat_epoch_from_hello() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_secs(10), chats);
    let mut h = hello("m1");
    h.stream_epochs = Some([("acp-1".to_string(), 1u64)].into_iter().collect());
    let (tx, _rx) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx }, &h).await;
    assert_eq!(reg.chat_epoch("m1", "acp-1").await, Some(1));
    assert_eq!(reg.chat_epoch("m1", "acp-2").await, None);
    let _ = _drop;
}

/// 孤儿清理钩子（§16 测试 27 + §7.5/§7.6）：意外存活（server 已标记终态但
/// instance 声称存活）+ pending_close 补发 → 下发 kill 断言（fake conn 收
/// kill 并回 ack）。
///
/// 注意：M1 `instance/hello` 无 alive_sessions 字段（§4.5 表），`on_hello`
/// 产出的 [`HelloOutcome`] 存活清单恒空——此处手工构造 [`HelloOutcome`]
/// 驱动对账，等价于未来版本/心跳驱动的对账输入（§8.3 步骤 5）。
#[tokio::test]
async fn cleanup_orphans_kill_decision() {
    // 真实 registry（register/transition/request_close_offline 写回 Registry
    // Doc 需要存活写者，§5.2 单写）。
    let tmp = tempfile::tempdir().unwrap();
    let persist_cfg = crate::persist::PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(crate::persist::Store::open(&persist_cfg).unwrap());
    store.recover().await;
    let sink = Arc::new(crate::control::StoreSink::new(store.clone()).await.unwrap());
    let doc = Arc::new(crate::state::doc_manager::DocManager::new(
        crate::state::doc_manager::BatchConfig::default(),
        sink,
    ));
    let chats = ChatRegistry::new(doc.registry());
    // 下发超时 2s：ack 回填需在窗口内。
    let reg = InstanceRegistry::new(
        Duration::from_secs(30),
        Duration::from_secs(2),
        chats.clone(),
    );
    let (tx, mut rx) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx }, &hello("m1"))
        .await;

    // s1：终态（意外存活裁决目标，§7.5）；s2：pending_close（补发目标，
    // §7.6）；s3：正常存活（不 kill）。
    chats.register("s1", "m1", None, "/", None).await.unwrap();
    chats.transition("s1", ChatState::Closed).await.unwrap();
    chats.register("s2", "m1", None, "/", None).await.unwrap();
    chats.request_close_offline("s2").await.unwrap();
    chats.register("s3", "m1", None, "/", None).await.unwrap();

    let outcome = HelloOutcome {
        fenced_previous: false,
        buffer_lost: false,
        alive_sessions: vec!["s1".to_string(), "s3".to_string()],
        chat_epochs: Default::default(),
    };
    // cleanup 内 send_kill 等 ack：放独立 task，fake instance 侧回填。
    let cleanup = tokio::spawn({
        let reg = reg.clone();
        let outcome = outcome.clone();
        async move { reg.cleanup_orphans("m1", &outcome).await }
    });
    let mut targets = Vec::new();
    for _ in 0..2 {
        let msg = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("kill frame timeout")
            .expect("channel alive");
        match msg {
            OutboundMsg::Frame(Frame::InstanceKill(k)) => {
                targets.push(k.chat_id.clone());
                let ack = InstanceAck::Kill(InstanceKillAck {
                    command_id: k.command_id.clone(),
                    chat_id: k.chat_id.clone(),
                    ok: true,
                });
                assert!(reg.on_ack("m1", &k.command_id, ack).await);
            }
            other => panic!("expected InstanceKill, got {other:?}"),
        }
    }
    let killed = cleanup.await.unwrap();
    assert!(
        killed.contains(&"s1".to_string()),
        "意外存活应 kill（§7.5）"
    );
    assert!(
        killed.contains(&"s2".to_string()),
        "pending_close 应补发 kill（§7.6）"
    );
    assert!(!killed.contains(&"s3".to_string()), "正常存活不 kill");
    assert_eq!(targets.len(), 2);
    // s2 补发完成 → 清 pending_close（§7.6）。
    assert!(chats.pending_close_chats().await.is_empty());
    let _ = (doc, tmp);
}

/// hello fencing 后旧连接滞后断开不得触碰新连接状态（§4.5 幂等替换）：
/// 旧连接退出路径的 `on_disconnect` 必须识别陈旧断开（conn 句柄比对），
/// 否则新连接被误置 OFFLINE + 清 conn，指令下发全部失败。
#[tokio::test]
async fn fencing_stale_disconnect_keeps_new_connection_serving() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_secs(10), chats);
    let (tx1, _rx1) = mpsc::channel(8);
    reg.on_hello(
        "m1",
        "tok-1",
        InstanceConn { tx: tx1.clone() },
        &hello("m1"),
    )
    .await;
    // 新 hello（fencing 旧连接，§4.5）。
    let (tx2, mut rx2) = mpsc::channel(8);
    let outcome = reg
        .on_hello(
            "m1",
            "tok-1",
            InstanceConn { tx: tx2.clone() },
            &hello("m1"),
        )
        .await;
    assert!(outcome.fenced_previous);
    // 旧连接退出（gateway 断链路径）——陈旧断开：不得置 Offline。
    assert!(!reg.on_disconnect("m1", &InstanceConn { tx: tx1 }).await);
    assert_eq!(reg.state("m1").await, Some(InstanceState::Online));
    // 新连接仍可服务：spawn 下发成功（fake instance 回 ack）。
    let spawned = tokio::spawn({
        let reg = reg.clone();
        async move {
            reg.send_spawn(
                "m1",
                InstanceSpawn {
                    command_id: "c1".into(),
                    chat_id: "s1".into(),
                    cmd: vec!["peri".into()],
                    cwd: "/".into(),
                    env: None,
                },
            )
            .await
        }
    });
    match rx2.recv().await {
        Some(OutboundMsg::Frame(Frame::InstanceSpawn(k))) => {
            assert!(
                reg.on_ack(
                    "m1",
                    &k.command_id,
                    InstanceAck::Spawn(InstanceSpawnAck {
                        command_id: k.command_id.clone(),
                        chat_id: "s1".into(),
                        ok: true,
                        error: None,
                    }),
                )
                .await
            );
        }
        other => panic!("expected spawn frame on new connection, got {other:?}"),
    }
    assert!(matches!(spawned.await.unwrap(), Ok(SpawnOutcome::Acked(_))));
    let _ = _drop;
}

/// 心跳超时判定 OFFLINE 后，心跳恢复 → ONLINE 即可服务（§7.1 心跳恢复
/// 路径）：`sweep_offline` 不得清空连接句柄（连接可能仍存活；清空后无恢复
/// 路径，机器不会重连——服务瘫痪）。
#[tokio::test]
async fn heartbeat_recovery_after_offline_sweep_serves_commands() {
    let (registry, _drop) = test_registry();
    let chats = ChatRegistry::new(registry);
    let reg = InstanceRegistry::new(Duration::from_secs(30), Duration::from_secs(10), chats);
    let (tx, mut rx) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx: tx.clone() }, &hello("m1"))
        .await;
    // 30s 无心跳 → OFFLINE（连接句柄保留）。
    let t0 = Instant::now();
    let offline = reg.sweep_offline(t0 + Duration::from_secs(31)).await;
    assert_eq!(offline, vec!["m1".to_string()]);
    assert_eq!(reg.state("m1").await, Some(InstanceState::Offline));
    // OFFLINE 期间指令拒绝（§7.1）。
    assert!(matches!(
        reg.send_spawn(
            "m1",
            InstanceSpawn {
                command_id: "c0".into(),
                chat_id: "s0".into(),
                cmd: vec!["peri".into()],
                cwd: "/".into(),
                env: None,
            },
        )
        .await,
        Err(InstanceError::Offline)
    ));
    // 心跳恢复 → ONLINE，无需重连即可服务（§7.1 图：OFFLINE ──► ONLINE）。
    reg.on_heartbeat(
        "m1",
        &InstanceHeartbeat {
            load: 0,
            alive_sessions: vec![],
        },
    )
    .await
    .unwrap();
    assert_eq!(reg.state("m1").await, Some(InstanceState::Online));
    let spawned = tokio::spawn({
        let reg = reg.clone();
        async move {
            reg.send_spawn(
                "m1",
                InstanceSpawn {
                    command_id: "c1".into(),
                    chat_id: "s1".into(),
                    cmd: vec!["peri".into()],
                    cwd: "/".into(),
                    env: None,
                },
            )
            .await
        }
    });
    match rx.recv().await {
        Some(OutboundMsg::Frame(Frame::InstanceSpawn(k))) => {
            assert!(
                reg.on_ack(
                    "m1",
                    &k.command_id,
                    InstanceAck::Spawn(InstanceSpawnAck {
                        command_id: k.command_id.clone(),
                        chat_id: "s1".into(),
                        ok: true,
                        error: None,
                    }),
                )
                .await
            );
        }
        other => panic!("expected spawn frame after heartbeat recovery, got {other:?}"),
    }
    assert!(matches!(spawned.await.unwrap(), Ok(SpawnOutcome::Acked(_))));
    let _ = _drop;
}

/// 心跳驱动的 alive_sessions 对账（§8.3 步骤 5）：M1 hello 无存活清单字段
/// （§4.5 表），对账输入唯一来源是 `instance/heartbeat`——alive 变化且非空
/// 时触发 reconcile + kill（意外存活 §7.5 / pending_close §7.6），且不阻塞
/// 心跳调用（后台任务）。
#[tokio::test]
async fn heartbeat_alive_sessions_reconciliation_kills() {
    let tmp = tempfile::tempdir().unwrap();
    let persist_cfg = crate::persist::PersistConfig {
        data_dir: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let store = Arc::new(crate::persist::Store::open(&persist_cfg).unwrap());
    store.recover().await;
    let sink = Arc::new(crate::control::StoreSink::new(store.clone()).await.unwrap());
    let doc = Arc::new(crate::state::doc_manager::DocManager::new(
        crate::state::doc_manager::BatchConfig::default(),
        sink,
    ));
    let chats = ChatRegistry::new(doc.registry());
    let reg = InstanceRegistry::new(
        Duration::from_secs(30),
        Duration::from_secs(2),
        chats.clone(),
    );
    let (tx, mut rx) = mpsc::channel(8);
    reg.on_hello("m1", "tok-1", InstanceConn { tx }, &hello("m1"))
        .await;

    // s1：终态（意外存活，§7.5）；s2：pending_close（§7.6 补发）。
    chats.register("s1", "m1", None, "/", None).await.unwrap();
    chats.transition("s1", ChatState::Closed).await.unwrap();
    chats.register("s2", "m1", None, "/", None).await.unwrap();
    chats.request_close_offline("s2").await.unwrap();

    // 首次心跳带存活清单（变化）→ 触发对账 + kill（后台任务）。
    reg.on_heartbeat(
        "m1",
        &InstanceHeartbeat {
            load: 0,
            alive_sessions: vec!["s1".to_string(), "s2".to_string()],
        },
    )
    .await
    .unwrap();
    let mut targets = Vec::new();
    for _ in 0..2 {
        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("kill frame timeout")
            .expect("channel alive");
        match msg {
            OutboundMsg::Frame(Frame::InstanceKill(k)) => {
                targets.push(k.chat_id.clone());
                let ack = InstanceAck::Kill(InstanceKillAck {
                    command_id: k.command_id.clone(),
                    chat_id: k.chat_id.clone(),
                    ok: true,
                });
                assert!(reg.on_ack("m1", &k.command_id, ack).await);
            }
            other => panic!("expected InstanceKill, got {other:?}"),
        }
    }
    assert!(
        targets.contains(&"s1".to_string()),
        "意外存活应 kill（§7.5）"
    );
    assert!(
        targets.contains(&"s2".to_string()),
        "pending_close 应补发 kill（§7.6）"
    );
    // kill 完成 → 会话 Closed（pending_close 集合清除，§7.6）。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(chats.pending_close_chats().await.is_empty());
    // 相同 alive 再上报 → 不重复触发（无新 kill 帧）。
    reg.on_heartbeat(
        "m1",
        &InstanceHeartbeat {
            load: 0,
            alive_sessions: vec!["s1".to_string(), "s2".to_string()],
        },
    )
    .await
    .unwrap();
    let extra = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(extra.is_err(), "alive 未变化不应重复对账 kill");
    let _ = (doc, tmp);
}
