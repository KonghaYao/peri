//! CommandCoordinator 单测（设计稿 §16 测试 9–14 的子集；全链路 create 成功
//! 路径在 hub_test 端到端）。
//!
//! 环境注：F4 `DocManager::try_reserve` 的 in_flight 计数在 writer 消费成功
//! 路径不递减（F4 已知缺口，见输出遗留问题）——「队列满」测试利用该行为
//! （提交 64 次后永久 RATE_LIMITED），其余测试每个 session 提交量 < 64。
//!
//! 2026-08-07 主管修复 P1-1（writer 消费后名额释放）：「队列满」测试改为
//! 先占满名额（try_reserve ×64 不提交）再验证第 65 次 RATE_LIMITED。

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use acp_hub_proto::action::{
    ActionEnvelope, CreateChatPayload, PromptChatPayload,
};
use acp_hub_proto::frame::Frame;
use acp_hub_proto::instance::InstanceForwardAck;

use crate::auth::{ConnectionCtx, TokenRole};
use crate::channel::OutboundMsg;
use crate::channel::{CommandCoordinator, DEFAULT_ACP_CMD, SubmitAck};
use crate::channel::RelayEventHandler;
use crate::control::StoreSink;
use crate::control::{InstanceAck, InstanceConn, InstanceRegistry};
use crate::control::ChatRegistry;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager};

/// 测试用固定 UUID session id（coordinator 要求 UUID 形态，§4.3）。
const S1: &str = "00000000-0000-0000-0000-000000000001";
const S2: &str = "00000000-0000-0000-0000-000000000002";
const S3: &str = "00000000-0000-0000-0000-000000000003";
const S4: &str = "00000000-0000-0000-0000-000000000004";
const S5: &str = "00000000-0000-0000-0000-000000000005";

struct Env {
    _tmp: tempfile::TempDir,
    store: Arc<Store>,
    doc: Arc<DocManager>,
    instance: Arc<InstanceRegistry>,
    chats: ChatRegistry,
    relay: Arc<RelayEventHandler>,
    coordinator: Arc<CommandCoordinator>,
    /// instance 连接发送侧（测试读下行帧）。
    instance_rx: mpsc::Receiver<OutboundMsg>,
    /// instance 连接发送句柄（on_disconnect 句柄比对用）。
    instance_tx: mpsc::Sender<OutboundMsg>,
}

fn ctx(name: &str) -> ConnectionCtx {
    ConnectionCtx {
        token_id: format!("tok-{name}"),
        role: TokenRole::Full,
        name: name.to_string(),
        peer: "127.0.0.1:1234".parse().unwrap(),
        hostname: None,
        established_at: chrono::Utc::now(),
    }
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
        Duration::from_millis(200),
        chats.clone(),
    ));
    let relay = Arc::new(RelayEventHandler::new(
        doc.clone(),
        chats.clone(),
        instance.clone(),
        doc.registry(),
    ));
    let coordinator = Arc::new(CommandCoordinator::with_l3_timeout(
        store.clone(),
        doc.clone(),
        instance.clone(),
        chats.clone(),
        relay.clone(),
        &BatchConfig::default(),
        DEFAULT_ACP_CMD.iter().map(|s| s.to_string()).collect(),
        Duration::from_millis(500),
        Duration::from_millis(500),
        Duration::from_millis(500),
        Duration::from_millis(500),
    ));
    // instance 上线（hello）。
    let (instance_tx, instance_rx) = mpsc::channel(64);
    instance
        .on_hello(
            "local",
            "tok-m",
            InstanceConn { tx: instance_tx.clone() },
            &acp_hub_proto::instance::InstanceHello {
                token: "tok".into(),
                hostname: "local".into(),
                caps: serde_json::json!({}),
                buffered: None,
                buffer_lost: None,
                stream_epochs: None,
                nonce: "AAAA".into(),
            },
        )
        .await;
    Env {
        _tmp: tmp,
        store,
        doc,
        instance,
        chats,
        relay,
        coordinator,
        instance_rx,
        instance_tx,
    }
}

/// 注册 + binding 一个可 prompt 的 session（完整生命周期：Store 目录 →
/// Doc 打开 → Registry 登记 → binding，§6.2）。
async fn bound_session(env: &Env, sid: &str, acp: &str) {
    let uuid = uuid::Uuid::parse_str(sid).expect("uuid session id");
    env.store.create_chat(uuid).unwrap();
    env.doc.open_chat(sid, "local", None).await.unwrap();
    env.chats.register(sid, "local", None).await.unwrap();
    env.chats.bind(sid, acp).await.unwrap();
}

fn prompt_action(cid: &str, sid: &str) -> ActionEnvelope {
    ActionEnvelope::Prompt {
        command_id: cid.into(),
        payload: PromptChatPayload {
            chat_id: sid.into(),
            message: format!("msg-{cid}"),
        },
    }
}

#[tokio::test]
async fn dedup_duplicate_ack() {
    let env = env().await;
    bound_session(&env, S1, "acp-1").await;
    let (tx, rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();

    let first = env.coordinator.submit(&ctx("c"), prompt_action(&cid, S1), tx.clone()).await;
    assert!(matches!(first, SubmitAck::Accepted { .. }), "{first:?}");
    // 二次提交同 commandId → duplicate（不重复调用 Agent）。
    let second = env.coordinator.submit(&ctx("c"), prompt_action(&cid, S1), tx.clone()).await;
    match second {
        SubmitAck::Duplicate(ack) => {
            assert_eq!(ack.command_id, cid);
            assert!(ack.turn_id.is_some(), "duplicate 必带原 turnId");
        }
        other => panic!("expected duplicate, got {other:?}"),
    }
    drop(tx);
    drop(rx);
}

#[tokio::test]
async fn queue_full_rate_limited() {
    let env = env().await;
    bound_session(&env, S2, "acp-2").await;
    let (tx, _rx) = mpsc::channel(16);
    // 占满队列名额（§7.4 规则 1：try_reserve 上限 64，不提交则不释放）。
    for _ in 0..64 {
        assert!(env.doc.try_reserve(S2).await, "应能占满 64 名额");
    }
    // 第 65 次提交 → try_reserve 失败 → RATE_LIMITED。
    let cid65 = uuid::Uuid::new_v4().to_string();
    let r = env.coordinator.submit(&ctx("c"), prompt_action(&cid65, S2), tx.clone()).await;
    match r {
        SubmitAck::Failed(e) => assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::RateLimited),
        other => panic!("expected rate limited, got {other:?}"),
    }
}

#[tokio::test]
async fn serial_execution_order() {
    let mut env = env().await;
    bound_session(&env, S3, "acp-3").await;
    let (tx, _rx) = mpsc::channel(16);
    // 串行提交 6 条 prompt（< 64 规避 F4 in_flight 缺口）。
    let mut cids = Vec::new();
    for _ in 0..6 {
        let cid = uuid::Uuid::new_v4().to_string();
        cids.push(cid.clone());
        let r = env.coordinator.submit(&ctx("c"), prompt_action(&cid, S3), tx.clone()).await;
        assert!(matches!(r, SubmitAck::Accepted { .. }));
    }
    // instance 侧收到的 forward 顺序 = 提交顺序（§7.4 规则 1 串行）。
    // 每条 forward 帧回 ack（L1+L2 确认）否则 forward_rpc 阻塞 200ms 超时。
    let mut seen = Vec::new();
    for _ in 0..6 {
        match tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv()).await {
            Ok(Some(OutboundMsg::Frame(Frame::InstanceForward(f)))) => {
                seen.push(f.frame["id"].as_str().unwrap().to_string());
                env.instance
                    .on_ack(
                        "local",
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
    }
    // 6 条 prompt 的 rpc id 单调（hub-N）。
    assert_eq!(seen.len(), 6);
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "rpc ids should be monotonic (serial execution)");
}

#[tokio::test]
async fn spawn_failure_agent_unavailable() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::Create {
        command_id: cid.clone(),
        payload: CreateChatPayload {
            instance_id: Some("local".into()),
            cwd: None,
            title: Some("t".into()),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    // instance 收 spawn → 回 spawn_ack{ok:false}。
    let spawn_frame = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("instance should receive spawn")
        .expect("rx alive");
    let (spawn_cid, chat_id) = match spawn_frame {
        OutboundMsg::Frame(Frame::InstanceSpawn(s)) => (s.command_id, s.chat_id),
        other => panic!("expected spawn, got {other:?}"),
    };
    assert_eq!(spawn_cid, cid);
    env.instance
        .on_ack(
            "local",
            &cid,
            crate::control::InstanceAck::Spawn(acp_hub_proto::instance::InstanceSpawnAck {
                command_id: cid.clone(),
                chat_id: chat_id.clone(),
                ok: false,
                error: Some("spawn failed".into()),
            }),
        )
        .await;
    // client 收 action_error AGENT_UNAVAILABLE（retryable）。
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionError(e)))) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::AgentUnavailable);
            assert!(e.retryable);
        }
        other => panic!("expected action_error, got {other:?}"),
    }
    // 半创建清理：无幽灵视图（session 已移除；轮询等待异步清理完成）。
    let sid_uuid = chat_id.parse().unwrap();
    let mut removed = false;
    for _ in 0..40 {
        if env.store.chat(sid_uuid).is_none() {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(removed, "半创建 session 应被清理（§6.2）");
}

#[tokio::test]
async fn create_timeout_cleanup_with_kill() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::Create {
        command_id: cid.clone(),
        payload: CreateChatPayload::default(),
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }));
    // instance 收 spawn（不回 ack）→ spawn 超时（500ms）→ 清理 + 补发 kill。
    let _spawn = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("spawn received");
    let mut saw_kill = false;
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv()).await {
            Ok(Some(OutboundMsg::Frame(Frame::InstanceKill(k)))) => {
                saw_kill = true;
                // 回 kill_ack（幂等清理路径）。
                env.instance
                    .on_ack(
                        "local",
                        &k.command_id,
                        crate::control::InstanceAck::Kill(
                            acp_hub_proto::instance::InstanceKillAck {
                                command_id: k.command_id.clone(),
                                chat_id: k.chat_id.clone(),
                                ok: true,
                            },
                        ),
                    )
                    .await;
                break;
            }
            Ok(Some(_)) => continue,
            other => panic!("expected kill, got {other:?}"),
        }
    }
    assert!(saw_kill, "cleanup kill should be sent (§6.2)");
    // client 收 action_error AGENT_UNAVAILABLE(retryable)。
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionError(e)))) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::AgentUnavailable);
            assert!(e.retryable);
        }
        other => panic!("expected action_error, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_duplicate_and_unknown() {
    let mut env = env().await;
    bound_session(&env, S4, "acp-4").await;
    let (tx, mut rx) = mpsc::channel(16);
    // 先建立 active turn（§6.5 服务端单写：RegisterUserEntry 注册 accepting
    // turn——聚合器终态守卫要求 permission 归属的活动 turn，§6.3）。
    let turn = env
        .doc
        .submit_command(
            S4,
            crate::state::doc_manager::DocCommand::RegisterUserEntry {
                turn_id: "t1".into(),
                entry_id: "t1:user".into(),
                text: "hi".into(),
                author_user_id: None,
                created_at: chrono::Utc::now().to_rfc3339(),
            },
        )
        .await;
    assert!(matches!(
        turn,
        crate::state::doc_manager::SubmitResult::Applied(_)
    ));
    // 先投影一个 permission_request（epoch=0，聚合器接受）。信封 chat_id
    // = 进程归属（hub id）；帧内 sessionId = acp id（binding 校验键）。
    let ev = acp_hub_proto::instance::InstanceEvent {
        chat_id: S4.into(),
        epoch: 0,
        seq: 1,
        frame: serde_json::json!({
            "type": "permission_request",
            "sessionId": "acp-4",
            "payload": {"permissionId": "p1", "turnId": "t1", "title": "run", "options": ["allow"]}
        }),
    };
    let r = env.relay.on_instance_event("local", &ev).await;
    assert!(matches!(r, crate::channel::ConsumeResult::Delivered { applied: true, .. }));
    // 等待聚合器落盘（控制类事件挂 oneshot，已落盘）。
    let resolve = |cid: &str| ActionEnvelope::ResolvePermission {
        command_id: cid.into(),
        payload: acp_hub_proto::action::ResolvePermissionPayload {
            chat_id: S4.into(),
            permission_id: "p1".into(),
            decision: acp_hub_proto::action::PermissionDecision::Allow,
        },
    };
    // 第一次 resolve：CAS Migrated → forward（instance 在线收 InstanceForward）。
    let cid1 = uuid::Uuid::new_v4().to_string();
    let r1 = env.coordinator.submit(&ctx("c"), resolve(&cid1), tx.clone()).await;
    assert!(matches!(r1, SubmitAck::Accepted { .. }), "r1 = {r1:?}");
    let fwd = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("forward received")
        .expect("rx alive");
    let rpc_id = match &fwd {
        OutboundMsg::Frame(Frame::InstanceForward(f)) => {
            // 回 forward_ack（L1+L2 确认；否则 forward_rpc 200ms 超时）。
            env.instance
                .on_ack(
                    "local",
                    &f.command_id,
                    InstanceAck::Forward(InstanceForwardAck {
                        command_id: f.command_id.clone(),
                        chat_id: f.chat_id.clone(),
                        ok: true,
                        error: None,
                    }),
                )
                .await;
            f.frame["id"].as_str().unwrap().to_string()
        }
        other => panic!("expected forward frame, got {other:?}"),
    };
    // L3：instance 回 JSON-RPC response（pending_rpc 匹配 → delivery_confirmed）。
    let resp = acp_hub_proto::instance::InstanceEvent {
        chat_id: S4.into(),
        epoch: 0,
        seq: 2,
        frame: serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": {}
        }),
    };
    let r = env.relay.on_instance_event("local", &resp).await;
    assert!(matches!(
        r,
        crate::channel::ConsumeResult::RpcConfirmed { .. }
    ));
    // 第一次 committed ack（L3 后）。
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionAck(ack)))) => {
            assert_eq!(ack.status, acp_hub_proto::ack::AckStatus::Committed);
        }
        other => panic!("expected committed ack, got {other:?}"),
    }

    // 第二次（新 commandId）答同一 permission：CAS Duplicate → duplicate ack。
    let cid2 = uuid::Uuid::new_v4().to_string();
    let r2 = env.coordinator.submit(&ctx("c"), resolve(&cid2), tx.clone()).await;
    assert!(matches!(r2, SubmitAck::Accepted { .. }));
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionAck(ack)))) => {
            assert_eq!(ack.status, acp_hub_proto::ack::AckStatus::Duplicate);
        }
        other => panic!("expected duplicate ack, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_session_rejected() {
    let env = env().await;
    let (tx, _rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(&ctx("c"), prompt_action(&cid, "ffffffff-ffff-ffff-ffff-ffffffffffff"), tx)
        .await;
    match r {
        SubmitAck::Failed(e) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::ChatNotFound)
        }
        other => panic!("expected session not found, got {other:?}"),
    }
}

#[tokio::test]
async fn close_offline_pending_close() {
    let env = env().await;
    bound_session(&env, S5, "acp-5").await;
    // 机器断开 → OFFLINE。
    env.instance
        .on_disconnect("local", &InstanceConn { tx: env.instance_tx.clone() })
        .await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::Close {
        command_id: cid.clone(),
        payload: acp_hub_proto::action::CloseChatPayload {
            chat_id: S5.into(),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }));
    // §7.6：offline close → MACHINE_OFFLINE(retryable) + pending_close 标记。
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionError(e)))) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::InstanceOffline);
            assert!(e.retryable);
        }
        other => panic!("expected action_error, got {other:?}"),
    }
    assert!(env.chats.pending_close_chats().await.contains(&S5.to_string()));
}
