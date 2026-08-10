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

use acp_hub_proto::ack::{ActionError, ErrorCode};
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
    /// 落盘 sink（session doc 镜像快照断言用）。
    sink: Arc<StoreSink>,
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
        sink,
        instance_rx,
        instance_tx,
    }
}

/// 注册 + binding 一个可 prompt 的 session（完整生命周期：Store 目录 →
/// Doc 打开 → Registry 登记 → binding，§6.2）。
async fn bound_session(env: &Env, sid: &str, acp: &str) {
    let uuid = uuid::Uuid::parse_str(sid).expect("uuid session id");
    env.store.create_chat(uuid).unwrap();
    env.doc.open_chat(sid, "local", None, None, None).await.unwrap();
    env.chats.register(sid, "local", None, "/", None).await.unwrap();
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
            acp_session_id: None,
            workspace_id: None,
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

// ---------------------------------------------------------------------------
// session/list 响应解析（§6.3）：纯函数单测
// ---------------------------------------------------------------------------

#[test]
fn parse_session_list_response_accepts_camel_case_and_defaults_status() {
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "hub-1",
        "result": {
            "sessions": [
                { "sessionId": "sess-1", "title": "会话A", "updatedAt": "2026-08-09T00:00:00Z" },
                { "sessionId": "sess-2", "title": "会话B", "cwd": "/tmp" }
            ]
        }
    });
    let entries = crate::channel::command_coordinator::parse_session_list_response(&resp);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].session_id, "sess-1");
    assert_eq!(entries[0].title, "会话A");
    // peri SessionInfo 无 status 字段（agent-client-protocol-schema 1.4）→ 空串缺省。
    assert_eq!(entries[0].status, "");
    assert_eq!(entries[0].updated_at, "2026-08-09T00:00:00Z");
    assert_eq!(entries[1].updated_at, "");
}

#[test]
fn parse_session_list_response_handles_snake_case_and_drops_invalid() {
    let resp = serde_json::json!({
        "result": {
            "sessions": [
                { "session_id": "s1", "status": "completed" },
                { "sessionId": "" }
            ]
        }
    });
    let entries = crate::channel::command_coordinator::parse_session_list_response(&resp);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, "s1");
    assert_eq!(entries[0].status, "completed");
}

#[test]
fn parse_session_list_response_error_and_missing_result_are_empty() {
    assert!(
        crate::channel::command_coordinator::parse_session_list_response(
            &serde_json::json!({ "error": { "code": -32603, "message": "x" } })
        )
        .is_empty()
    );
    assert!(
        crate::channel::command_coordinator::parse_session_list_response(
            &serde_json::json!({ "result": {} })
        )
        .is_empty()
    );
    assert!(
        crate::channel::command_coordinator::parse_session_list_response(
            &serde_json::json!({ "result": { "sessions": null } })
        )
        .is_empty()
    );
}

// ── workspace 管理命令（独立于 chat 的上层概念）────────────────────────

#[tokio::test]
async fn workspace_create_committed_and_registry_projected() {
    let env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    let action = ActionEnvelope::WorkspaceCreate {
        command_id: cid.clone(),
        payload: acp_hub_proto::action::WorkspaceCreatePayload {
            name: "my-ws".into(),
            cwd: path.clone(),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    // client 收 committed Ack（管理面：accepted → committed 直通）。
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionAck(a)))) => {
            assert_eq!(a.command_id, cid);
            assert_eq!(a.status, acp_hub_proto::ack::AckStatus::Committed);
        }
        other => panic!("expected committed ack, got {other:?}"),
    }
    // 投影到 Registry Doc（create 的读取面）。
    let list = env.chats.registry().list_workspaces().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "my-ws");
    assert_eq!(list[0].cwd, path);
}

#[tokio::test]
async fn workspace_create_invalid_cwd_error() {
    let env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::WorkspaceCreate {
        command_id: cid.clone(),
        payload: acp_hub_proto::action::WorkspaceCreatePayload {
            name: "w".into(),
            cwd: "/no/such/dir-xyz-123".into(),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionError(e)))) => {
            assert_eq!(e.command_id, cid);
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::InvalidState);
            assert!(!e.retryable);
        }
        other => panic!("expected action_error, got {other:?}"),
    }
    // 未投影。
    assert!(env.chats.registry().list_workspaces().await.unwrap().is_empty());
}

#[tokio::test]
async fn workspace_remove_committed_and_removed() {
    let env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let dir = tempfile::tempdir().unwrap();
    // 先建。
    let create_cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(
            &ctx("c"),
            ActionEnvelope::WorkspaceCreate {
                command_id: create_cid.clone(),
                payload: acp_hub_proto::action::WorkspaceCreatePayload {
                    name: "w".into(),
                    cwd: dir.path().to_str().unwrap().into(),
                },
            },
            tx.clone(),
        )
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    // 消费 committed ack。
    let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("create committed ack");
    // 从 Registry Doc 读回 id。
    let list = env.chats.registry().list_workspaces().await.unwrap();
    assert_eq!(list.len(), 1);
    let ws_id = list[0].id.clone();

    // 删。
    let remove_cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(
            &ctx("c"),
            ActionEnvelope::WorkspaceRemove {
                command_id: remove_cid.clone(),
                payload: acp_hub_proto::action::WorkspaceRemovePayload {
                    workspace_id: ws_id.clone(),
                },
            },
            tx.clone(),
        )
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionAck(a)))) => {
            assert_eq!(a.command_id, remove_cid);
            assert_eq!(a.status, acp_hub_proto::ack::AckStatus::Committed);
        }
        other => panic!("expected committed ack, got {other:?}"),
    }
    assert!(env.chats.registry().list_workspaces().await.unwrap().is_empty());

    // 删不存在的 → InvalidState。
    let cid2 = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(
            &ctx("c"),
            ActionEnvelope::WorkspaceRemove {
                command_id: cid2.clone(),
                payload: acp_hub_proto::action::WorkspaceRemovePayload {
                    workspace_id: ws_id.clone(),
                },
            },
            tx.clone(),
        )
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionError(e)))) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::InvalidState);
        }
        other => panic!("expected action_error, got {other:?}"),
    }
}

#[tokio::test]
async fn create_with_workspace_id_inherits_cwd() {
    let mut env = env().await;
    let (tx, _rx) = mpsc::channel(16);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_string();
    // 先建 workspace。
    let ws_cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(
            &ctx("c"),
            ActionEnvelope::WorkspaceCreate {
                command_id: ws_cid,
                payload: acp_hub_proto::action::WorkspaceCreatePayload {
                    name: "ws".into(),
                    cwd: path.clone(),
                },
            },
            tx.clone(),
        )
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    let list = env.chats.registry().list_workspaces().await.unwrap();
    assert_eq!(list.len(), 1);
    let ws_id = list[0].id.clone();

    // workspace 下新建对话（不直传 cwd）→ spawn cwd = workspace.cwd。
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::Create {
        command_id: cid.clone(),
        payload: CreateChatPayload {
            instance_id: Some("local".into()),
            cwd: None,
            title: Some("t".into()),
            acp_session_id: None,
            workspace_id: Some(ws_id),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    // spawn 帧携带 workspace 的 cwd（§6.2：ACP 进程工作目录继承）。
    let spawn_frame = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("instance should receive spawn")
        .expect("rx alive");
    match spawn_frame {
        OutboundMsg::Frame(Frame::InstanceSpawn(s)) => {
            assert_eq!(s.cwd, path);
        }
        other => panic!("expected spawn, got {other:?}"),
    }
}

#[tokio::test]
async fn create_with_unknown_workspace_fails_invalid_state() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::Create {
        command_id: cid.clone(),
        payload: CreateChatPayload {
            instance_id: Some("local".into()),
            cwd: None,
            title: Some("t".into()),
            acp_session_id: None,
            workspace_id: Some(uuid::Uuid::new_v4().to_string()),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    match r {
        SubmitAck::Failed(e) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::InvalidState);
            assert!(!e.retryable, "workspace 缺失不是瞬时故障");
        }
        other => panic!("expected failed, got {other:?}"),
    }
    // instance 不应收到任何帧（前置失败）。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(env.instance_rx.try_recv().is_err(), "instance 不应收到 spawn");
    // client 不应收到 ack/error（提交即失败，无异步终态）。
    assert!(rx.try_recv().is_err(), "client 不应收到帧");
}

// ── session/list 按需查询（§6.3：agent 侧真实数据源）────────────────────

/// 完整链路：bound chat → submit session/list → instance 侧收到 RPC 帧 →
/// 回 forward ack + JSON-RPC 响应 → client 收 session_list 帧（条目带 cwd）。
#[tokio::test]
async fn session_list_queries_agent_and_returns_frame() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    bound_session(&env, S1, "acp-1").await;
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::SessionList {
        command_id: cid.clone(),
        payload: acp_hub_proto::action::SessionListPayload {
            chat_id: S1.into(),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");

    // instance 侧收到 session/list RPC 帧（cwd 来自 chat record）。
    let fwd = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("instance should receive session/list RPC")
        .expect("rx alive");
    let rpc_id = match &fwd {
        OutboundMsg::Frame(Frame::InstanceForward(f)) => {
            assert_eq!(f.frame["method"], serde_json::json!("session/list"));
            assert_eq!(f.frame["params"]["cwd"], serde_json::json!("/"));
            // 转发目标必须是 hub chat id（instance 进程表键），不是 bound
            // 的 acp session id（否则 instance 找不到进程 → stdin_write_failed）。
            assert_eq!(f.chat_id, S1, "forward 目标须为 hub chat id");
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

    // L3：instance 回 JSON-RPC 响应（pending_rpc 匹配 → session_list 帧）。
    let resp = acp_hub_proto::instance::InstanceEvent {
        chat_id: S1.into(),
        epoch: 0,
        seq: 1,
        frame: serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": { "sessions": [
                { "sessionId": "acp-1", "title": "已打开会话", "status": "active",
                  "updatedAt": "2026-08-10T00:00:00Z" },
                { "sessionId": "sess-b", "title": "", "status": "",
                  "updatedAt": "" },
            ]},
        }),
    };
    let r = env.relay.on_instance_event("local", &resp).await;
    assert!(matches!(r, crate::channel::ConsumeResult::RpcConfirmed { .. }), "{r:?}");

    // client 收 session_list 帧：command_id 回显 + 条目带 cwd（查询面）。
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::SessionList(s)))) => {
            assert_eq!(s.command_id, cid);
            assert_eq!(s.chat_id, S1);
            assert_eq!(s.sessions.len(), 2);
            // 已绑定会话（acp-1 → S1）：标注 bound_chat_id（§8.5 激活）。
            assert_eq!(s.sessions[0].session_id, "acp-1");
            assert_eq!(s.sessions[0].bound_chat_id.as_deref(), Some(S1));
            assert_eq!(s.sessions[0].cwd, "/", "条目应标注查询面 cwd");
            // 未绑定会话：bound_chat_id = None。
            assert_eq!(s.sessions[1].bound_chat_id, None);
            assert_eq!(s.sessions[1].cwd, "/");
        }
        other => panic!("expected session_list frame, got {other:?}"),
    }
}

/// 未知 chat → CHAT_NOT_FOUND（同步失败，无 instance 帧）。
#[tokio::test]
async fn session_list_unknown_chat_fails_chat_not_found() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::SessionList {
        command_id: cid.clone(),
        payload: acp_hub_proto::action::SessionListPayload {
            chat_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".into(),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    match r {
        SubmitAck::Failed(e) => {
            assert_eq!(e.command_id, cid);
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::ChatNotFound);
        }
        other => panic!("expected failed, got {other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(env.instance_rx.try_recv().is_err(), "instance 不应收到帧");
    assert!(rx.try_recv().is_err(), "client 不应收到帧");
}

/// 终态 chat（ACP 进程已退出）→ INVALID_STATE（无查询面）。
#[tokio::test]
async fn session_list_terminal_chat_fails_invalid_state() {
    let env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    bound_session(&env, S1, "acp-1").await;
    // 置终态：进程退出事件。
    let exit = acp_hub_proto::instance::InstanceProcessExit {
        chat_id: S1.into(),
        code: 0,
    };
    env.relay.on_process_exit("local", &exit).await;
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::SessionList {
        command_id: cid.clone(),
        payload: acp_hub_proto::action::SessionListPayload {
            chat_id: S1.into(),
        },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    match r {
        SubmitAck::Failed(e) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::InvalidState);
            assert!(!e.retryable, "终态 chat 不是瞬时故障");
        }
        other => panic!("expected failed, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "client 不应收到帧");
}

// ---------------------------------------------------------------------------
// chat/load 会话切换（§8.5）：当前对话内 load，不新建 chat/进程
// ---------------------------------------------------------------------------

fn load_action(cid: &str, chat_id: &str, acp_session_id: &str) -> ActionEnvelope {
    ActionEnvelope::Load {
        command_id: cid.into(),
        payload: acp_hub_proto::action::LoadChatPayload {
            chat_id: chat_id.into(),
            acp_session_id: acp_session_id.into(),
        },
    }
}

/// 成功路径：submit Accepted → instance 收 session/load RPC（转发目标 =
/// hub chat id，不是 acp session id）→ L3 响应 → committed ack + chat
/// 当前会话切换（bindings 新旧会话均指向本 chat，§8.5 进程内切换）。
#[tokio::test]
async fn load_chat_switches_session_in_place() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    bound_session(&env, S1, "acp-1").await;
    // 目标会话曾在同一进程内加载过再切回。回归：`bind` 的幂等早退不会
    // 更新当前 session，load 预绑定必须使用 `switch_session`。
    env.chats.switch_session(S1, "acp-2").await.unwrap();
    env.chats.switch_session(S1, "acp-1").await.unwrap();
    let cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(&ctx("c"), load_action(&cid, S1, "acp-2"), tx.clone())
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");

    // instance 侧收到 session/load RPC 帧（cwd 来自 chat record）。
    let fwd = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("instance should receive session/load RPC")
        .expect("rx alive");
    let rpc_id = match &fwd {
        OutboundMsg::Frame(Frame::InstanceForward(f)) => {
            assert_eq!(f.frame["method"], serde_json::json!("session/load"));
            assert_eq!(f.frame["params"]["cwd"], serde_json::json!("/"));
            assert_eq!(f.frame["params"]["sessionId"], serde_json::json!("acp-2"));
            // 转发目标必须是 hub chat id（instance 进程表键），不是 acp
            // session id——与 session/list 同款（§6.2 spawn 时按 hub id 注册）。
            assert_eq!(f.chat_id, S1, "forward 目标须为 hub chat id");
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

    // 预绑定先于响应生效（§8.5 修复）：ACP spec 强制 replay before
    // response——回放帧先于 load 响应到达，binding 须先建立否则被 relay
    // 以 binding_missing 丢弃（create 路径同款预绑定）。此处 L3 响应
    // 尚未回投，binding 已命中。
    assert_eq!(env.chats.resolve("acp-2").await.as_deref(), Some(S1));

    // 第一条 load 完成前拒绝同 chat 的第二条 load，避免两组 replay 通知
    // 写入同一个 Yjs Doc。
    let concurrent_cid = uuid::Uuid::new_v4().to_string();
    let concurrent = env
        .coordinator
        .submit(
            &ctx("c"),
            load_action(&concurrent_cid, S1, "acp-3"),
            tx.clone(),
        )
        .await;
    assert!(matches!(
        concurrent,
        SubmitAck::Failed(ActionError {
            code: ErrorCode::RateLimited,
            ..
        })
    ));

    // L3：instance 回 session/load 响应。
    let resp = acp_hub_proto::instance::InstanceEvent {
        chat_id: S1.into(),
        epoch: 0,
        seq: 1,
        frame: serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": { "sessionId": "acp-2" },
        }),
    };
    let r = env.relay.on_instance_event("local", &resp).await;
    assert!(matches!(r, crate::channel::ConsumeResult::RpcConfirmed { .. }), "{r:?}");

    // client 收 committed ack（带 chat_id）。
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionAck(ack)))) => {
            assert_eq!(ack.command_id, cid);
            assert_eq!(ack.status, acp_hub_proto::ack::AckStatus::Committed);
            assert_eq!(ack.chat_id.as_deref(), Some(S1));
        }
        other => panic!("expected committed ack, got {other:?}"),
    }
    // chat 当前会话已切换（进程内）：entry.session_id = acp-2；新旧会话
    // binding 均指向本 chat（relay 逐帧校验仍通过）。
    assert_eq!(env.chats.session_id(S1).await.as_deref(), Some("acp-2"));
    assert_eq!(env.chats.resolve("acp-1").await.as_deref(), Some(S1));
    assert_eq!(env.chats.resolve("acp-2").await.as_deref(), Some(S1));
}

/// 未知 chat → CHAT_NOT_FOUND（同步失败，无 instance 帧）。
#[tokio::test]
async fn load_chat_unknown_chat_fails_chat_not_found() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(&ctx("c"), load_action(&cid, "ffffffff-ffff-ffff-ffff-ffffffffffff", "acp-x"), tx)
        .await;
    match r {
        SubmitAck::Failed(e) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::ChatNotFound);
        }
        other => panic!("expected failed, got {other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(env.instance_rx.try_recv().is_err(), "instance 不应收到帧");
    assert!(rx.try_recv().is_err(), "client 不应收到帧");
}

/// 终态 chat（进程已退出）→ INVALID_STATE：load 是进程内操作，无进程
/// 则无会话可切（§8.5）。
#[tokio::test]
async fn load_chat_terminal_chat_fails_invalid_state() {
    let env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    bound_session(&env, S1, "acp-1").await;
    let exit = acp_hub_proto::instance::InstanceProcessExit {
        chat_id: S1.into(),
        code: 0,
    };
    env.relay.on_process_exit("local", &exit).await;
    let cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(&ctx("c"), load_action(&cid, S1, "acp-2"), tx.clone())
        .await;
    match r {
        SubmitAck::Failed(e) => {
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::InvalidState);
            assert!(!e.retryable);
        }
        other => panic!("expected failed, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "client 不应收到帧");
}

/// L3 错误响应（如目标会话不存在）→ action_error（可重试；会话切换
/// 无副作用残留——回放窗口已开，但 load 拒绝时 agent 侧未切换，下次
/// load 重开窗口覆盖）。
#[tokio::test]
async fn load_chat_rejected_response_is_retryable_error() {
    let mut env = env().await;
    let (tx, mut rx) = mpsc::channel(16);
    bound_session(&env, S1, "acp-1").await;
    let cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(&ctx("c"), load_action(&cid, S1, "ghost"), tx.clone())
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    let fwd = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("instance should receive session/load RPC")
        .expect("rx alive");
    let rpc_id = match &fwd {
        OutboundMsg::Frame(Frame::InstanceForward(f)) => {
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
    // L3：JSON-RPC error（会话不存在）。
    let resp = acp_hub_proto::instance::InstanceEvent {
        chat_id: S1.into(),
        epoch: 0,
        seq: 1,
        frame: serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "error": { "code": -32602, "message": "session not found" },
        }),
    };
    let r = env.relay.on_instance_event("local", &resp).await;
    assert!(matches!(r, crate::channel::ConsumeResult::RpcConfirmed { .. }), "{r:?}");
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionError(e)))) => {
            assert_eq!(e.command_id, cid);
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::AgentUnavailable);
            assert!(e.retryable);
        }
        other => panic!("expected action_error, got {other:?}"),
    }
    // chat 当前会话未被改动（切换失败不落地）。
    assert_eq!(env.chats.session_id(S1).await.as_deref(), Some("acp-1"));
}

// ---------------------------------------------------------------------------
// prompt L3 响应 → turn 终态（§7.2 宿主驱动 turn 模型）：真实 peri 不发
// turn_complete 通知，唯一终态信号是 prompt 的 L3 响应 result.stopReason
// （acp-channel.ts 同源）：failed/error → Failed、cancelled → Cancelled、
// 缺省 → Completed。同时断言活动 turn 表项清理（clear_active_turn——终态
// 后表项不得滞留阻塞后续 load「有活动 turn」校验）。
// ---------------------------------------------------------------------------

/// 驱动一轮完整 prompt L3 链路：提交 → forward_ack（L1+L2）→ L3 response
/// （`stop_reason` 缺省 = result 无 stopReason）→ committed ack。返回 session
/// doc 镜像中 `active_turn_status`；断言活动 turn 表项已清理。
async fn drive_prompt_l3(
    env: &mut Env,
    sid: &str,
    acp: &str,
    stop_reason: Option<&str>,
) -> String {
    bound_session(env, sid, acp).await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(&ctx("c"), prompt_action(&cid, sid), tx.clone())
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    let fwd = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("forward received")
        .expect("rx alive");
    let rpc_id = match &fwd {
        OutboundMsg::Frame(Frame::InstanceForward(f)) => {
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
    // L3：JSON-RPC response（result.stopReason）。
    let mut result = serde_json::Map::new();
    if let Some(sr) = stop_reason {
        result.insert("stopReason".into(), serde_json::Value::String(sr.into()));
    }
    let resp = acp_hub_proto::instance::InstanceEvent {
        chat_id: sid.into(),
        epoch: 0,
        seq: 2,
        frame: serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "result": result,
        }),
    };
    let r = env.relay.on_instance_event("local", &resp).await;
    assert!(matches!(r, crate::channel::ConsumeResult::RpcConfirmed { .. }), "{r:?}");
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionAck(ack)))) => {
            assert_eq!(ack.status, acp_hub_proto::ack::AckStatus::Committed);
        }
        other => panic!("expected committed ack, got {other:?}"),
    }
    // 活动 turn 表项清理（§7.2：终态后不得滞留阻塞 load）。
    assert!(
        env.chats.active_turn(sid).await.is_none(),
        "终态后活动 turn 表项必须清理"
    );
    // session doc 镜像：active_turn_status。
    let (snapshot, _) = env
        .sink
        .snapshot(&acp_hub_proto::conn::DocId::session(sid))
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
    sm.get(&txn, "active_turn_status")
        .and_then(|v| v.cast::<String>().ok())
        .unwrap_or_default()
}

/// stopReason → turn 终态三分支映射（§7.2）：failed → Failed、cancelled →
/// Cancelled、缺省 → Completed。
#[tokio::test]
async fn prompt_l3_stop_reason_maps_turn_terminal() {
    let mut env = env().await;
    let failed = drive_prompt_l3(&mut env, S1, "acp-1", Some("failed")).await;
    assert_eq!(failed, "failed", "stopReason=failed → turn 终态 Failed");
    let cancelled = drive_prompt_l3(&mut env, S2, "acp-2", Some("cancelled")).await;
    assert_eq!(cancelled, "cancelled", "stopReason=cancelled → turn 终态 Cancelled");
    let completed = drive_prompt_l3(&mut env, S3, "acp-3", None).await;
    assert_eq!(completed, "completed", "无 stopReason → turn 终态 Completed");
}

/// L3 error（agent 拒绝 prompt）→ error ack（AgentUnavailable，可重试）+
/// 活动 turn 表项清理（§7.2：L3 error 也是 turn 的终结——表项滞留会阻塞
/// 后续 load「有活动 turn」校验）。
#[tokio::test]
async fn prompt_l3_error_clears_active_turn() {
    let mut env = env().await;
    bound_session(&env, S4, "acp-4").await;
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let r = env
        .coordinator
        .submit(&ctx("c"), prompt_action(&cid, S4), tx.clone())
        .await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    let fwd = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("forward received")
        .expect("rx alive");
    let rpc_id = match &fwd {
        OutboundMsg::Frame(Frame::InstanceForward(f)) => {
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
    // executor 在 forward_ack 后继续执行（RegisterUserEntry → set_active_turn）；
    // 轮询等待活动 turn 登记（避免 executor 调度时序竞态）。
    let registered = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if env.chats.active_turn(S4).await.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(registered.is_ok(), "prompt 执行中登记活动 turn");
    // L3 error。
    let resp = acp_hub_proto::instance::InstanceEvent {
        chat_id: S4.into(),
        epoch: 0,
        seq: 2,
        frame: serde_json::json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "error": { "code": -32603, "message": "agent rejected" },
        }),
    };
    let r = env.relay.on_instance_event("local", &resp).await;
    assert!(matches!(r, crate::channel::ConsumeResult::RpcConfirmed { .. }), "{r:?}");
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionError(e)))) => {
            assert_eq!(e.command_id, cid);
            assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::AgentUnavailable);
            assert!(!e.retryable, "L3 error 是终态失败（fail_terminal，不可重试）");
        }
        other => panic!("expected action_error, got {other:?}"),
    }
    assert!(
        env.chats.active_turn(S4).await.is_none(),
        "L3 error → 活动 turn 表项清理"
    );
}

/// cancel（notification：无 id 帧，发送成功即 L3 等价确认）→ 注入 Cancelled
/// 终态 + 活动 turn 表项清理（§7.2；表项滞留会阻塞后续 load）。
#[tokio::test]
async fn cancel_notification_injects_cancelled_and_clears_active_turn() {
    let mut env = env().await;
    bound_session(&env, S5, "acp-5").await;
    // 投影活动 turn（session doc active_turn 建立；user_message 事件路径）。
    let r = env
        .relay
        .on_instance_event(
            "local",
            &acp_hub_proto::instance::InstanceEvent {
                chat_id: S5.into(),
                epoch: 0,
                seq: 1,
                frame: serde_json::json!({
                    "type": "user_message_chunk",
                    "sessionId": "acp-5",
                    "payload": {"turnId": "t1", "entryId": "t1:user", "text": "hi"}
                }),
            },
        )
        .await;
    assert!(matches!(r, crate::channel::ConsumeResult::Delivered { applied: true, .. }));
    env.chats.set_active_turn(S5, "t1").await;
    // cancel（notification 路径）。
    let (tx, mut rx) = mpsc::channel(16);
    let cid = uuid::Uuid::new_v4().to_string();
    let action = ActionEnvelope::Cancel {
        command_id: cid.clone(),
        payload: acp_hub_proto::action::CancelChatPayload { chat_id: S5.into() },
    };
    let r = env.coordinator.submit(&ctx("c"), action, tx.clone()).await;
    assert!(matches!(r, SubmitAck::Accepted { .. }), "{r:?}");
    // forward notification 帧 + ack（L1+L2；notification 无 L3 响应）。
    let fwd = tokio::time::timeout(Duration::from_secs(2), env.instance_rx.recv())
        .await
        .expect("forward received")
        .expect("rx alive");
    match &fwd {
        OutboundMsg::Frame(Frame::InstanceForward(f)) => {
            assert_eq!(f.frame["id"], serde_json::Value::Null, "cancel 为 notification（无 id）");
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
    match tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        Ok(Some(OutboundMsg::Frame(Frame::ActionAck(ack)))) => {
            assert_eq!(ack.status, acp_hub_proto::ack::AckStatus::Committed);
        }
        other => panic!("expected committed ack, got {other:?}"),
    }
    // 终态注入：session doc active_turn_status = cancelled + 表项清理。
    assert!(
        env.chats.active_turn(S5).await.is_none(),
        "cancel 终态后活动 turn 表项清理"
    );
    let (snapshot, _) = env
        .sink
        .snapshot(&acp_hub_proto::conn::DocId::session(S5))
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
        "cancelled",
        "cancel 发送成功 → turn 终态 Cancelled（§7.2）"
    );
}
