//! SessionChannel 单测（设计稿 §16 测试 20–21 的纯逻辑部分：首帧纪律、
//! ready 前缓冲、订阅状态；gateway ws 集成在 gateway_test）。

use std::sync::Arc;

use tokio::sync::mpsc;

use acp_hub_proto::action::PromptSessionPayload;
use acp_hub_proto::conn::DocId;
use acp_hub_proto::ysync::{YsyncSubscribe, YsyncUnsubscribe};
use acp_hub_proto::frame::Frame;

use crate::auth::{ConnectionCtx, TokenRole};
use crate::channel::CommandCoordinator;
use crate::channel::RelayEventHandler;
use crate::channel::{ChannelDeps, DispatchOutcome, SessionChannel};
use crate::control::StoreSink;
use crate::control::MachineRegistry;
use crate::control::SessionRegistry;
use crate::persist::{PersistConfig, Store};
use crate::state::doc_manager::{BatchConfig, DocManager};


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

/// 真实装配（复用 coordinator env 的依赖，但不 spawn 网络）。
struct Env {
    machine: Arc<MachineRegistry>,
    sessions: SessionRegistry,
    coordinator: Arc<CommandCoordinator>,
    conns: Arc<crate::channel::ConnectionRegistry>,
    broadcast: Arc<crate::channel::Broadcaster>,
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
    let sessions = SessionRegistry::new(registry);
    let machine = Arc::new(MachineRegistry::new(
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(1),
        sessions.clone(),
    ));
    let relay = Arc::new(RelayEventHandler::new(
        doc.clone(),
        sessions.clone(),
        machine.clone(),
        doc.registry(),
    ));
    let coordinator = Arc::new(CommandCoordinator::with_l3_timeout(
        store,
        doc.clone(),
        machine.clone(),
        sessions.clone(),
        relay.clone(),
        &BatchConfig::default(),
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(1),
    ));
    let conns = Arc::new(crate::channel::ConnectionRegistry::new(200));
    let broadcast = Arc::new(crate::channel::Broadcaster::new(1_000_000, 2_000_000));
    Env {
        machine,
        sessions,
        coordinator,
        conns,
        broadcast,
    }
}

#[tokio::test]
async fn first_frame_must_be_subscribe_or_action() {
    let env = env().await;
    let deps = ChannelDeps {
        coordinator: env.coordinator.clone(),
        broadcast: env.broadcast.clone(),
        machine: env.machine.clone(),
        sessions: Arc::new(env.sessions.clone()),
        conns: env.conns.clone(),
    };
    let mut ch = SessionChannel::new(ctx("c"));
    let (tx, _rx) = mpsc::channel(8);
    // 首帧 pong → 断开（1011）。
    let o = ch.dispatch(Frame::Pong(acp_hub_proto::conn::Pong {}), &deps, tx.clone()).await;
    assert!(matches!(o, DispatchOutcome::Disconnect(1011)));
    // 新连接：首帧 auth 类（S→C 帧）→ 断开。
    let mut ch2 = SessionChannel::new(ctx("c"));
    let o = ch2.dispatch(Frame::KeepAlive(acp_hub_proto::conn::KeepAlive {}), &deps, tx).await;
    assert!(matches!(o, DispatchOutcome::Disconnect(1011)));
}

#[tokio::test]
async fn action_buffered_before_ready_and_flushed() {
    let env = env().await;
    let deps = ChannelDeps {
        coordinator: env.coordinator.clone(),
        broadcast: env.broadcast.clone(),
        machine: env.machine.clone(),
        sessions: Arc::new(env.sessions.clone()),
        conns: env.conns.clone(),
    };
    let mut ch = SessionChannel::new(ctx("c"));
    let (tx, _rx) = mpsc::channel(8);
    let action = Frame::Action(acp_hub_proto::action::ActionEnvelope::Prompt {
        command_id: uuid::Uuid::new_v4().to_string(),
        payload: PromptSessionPayload {
            session_id: "s1".into(),
            message: "hi".into(),
        },
    });
    // ready 前：缓冲不处理。
    let o = ch.dispatch(action.clone(), &deps, tx.clone()).await;
    assert!(matches!(o, DispatchOutcome::None));
    assert_eq!(ch.pending_len(), 1);
    assert!(!ch.is_ready());
    // mark_ready → flush 缓冲。
    let flushed = ch.mark_ready();
    assert_eq!(flushed.len(), 1);
    assert!(ch.is_ready());
}

#[tokio::test]
async fn buffer_overflow_rate_limited() {
    let env = env().await;
    let deps = ChannelDeps {
        coordinator: env.coordinator.clone(),
        broadcast: env.broadcast.clone(),
        machine: env.machine.clone(),
        sessions: Arc::new(env.sessions.clone()),
        conns: env.conns.clone(),
    };
    let mut ch = SessionChannel::new(ctx("c"));
    let (tx, _rx) = mpsc::channel(8);
    // 65 个 action（缓冲上限 64，§4.6）。
    for i in 0..64 {
        let action = Frame::Action(acp_hub_proto::action::ActionEnvelope::Prompt {
            command_id: uuid::Uuid::new_v4().to_string(),
            payload: PromptSessionPayload {
                session_id: "s1".into(),
                message: format!("m{i}"),
            },
        });
        ch.dispatch(action, &deps, tx.clone()).await;
    }
    let overflow = Frame::Action(acp_hub_proto::action::ActionEnvelope::Prompt {
        command_id: uuid::Uuid::new_v4().to_string(),
        payload: PromptSessionPayload {
            session_id: "s1".into(),
            message: "overflow".into(),
        },
    });
    let o = ch.dispatch(overflow, &deps, tx.clone()).await;
    match o {
        DispatchOutcome::Send(msgs) => {
            assert_eq!(msgs.len(), 1);
            match &msgs[0] {
                crate::channel::OutboundMsg::Frame(Frame::ActionError(e)) => {
                    assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::RateLimited)
                }
                other => panic!("expected action_error, got {other:?}"),
            }
        }
        other => panic!("expected send, got {other:?}"),
    }
}

#[tokio::test]
async fn subscribe_first_and_second() {
    let env = env().await;
    let deps = ChannelDeps {
        coordinator: env.coordinator.clone(),
        broadcast: env.broadcast.clone(),
        machine: env.machine.clone(),
        sessions: Arc::new(env.sessions.clone()),
        conns: env.conns.clone(),
    };
    let mut ch = SessionChannel::new(ctx("c"));
    let (tx, _rx) = mpsc::channel(8);
    let sub1 = Frame::YsyncSubscribe(YsyncSubscribe {
        docs: vec![DocId::chat("s1")],
    });
    match ch.dispatch(sub1, &deps, tx.clone()).await {
        DispatchOutcome::Subscribe { first, docs } => {
            assert!(first);
            assert_eq!(docs.len(), 1);
        }
        other => panic!("expected subscribe, got {other:?}"),
    }
    // 二次订阅：非 first。
    let sub2 = Frame::YsyncSubscribe(YsyncSubscribe {
        docs: vec![DocId::chat("s2")],
    });
    match ch.dispatch(sub2, &deps, tx.clone()).await {
        DispatchOutcome::Subscribe { first, .. } => assert!(!first),
        other => panic!("expected subscribe, got {other:?}"),
    }
    // 退订。
    let unsub = Frame::YsyncUnsubscribe(YsyncUnsubscribe {
        docs: vec![DocId::chat("s1")],
    });
    match ch.dispatch(unsub, &deps, tx).await {
        DispatchOutcome::Unsubscribe { docs } => assert_eq!(docs.len(), 1),
        other => panic!("expected unsubscribe, got {other:?}"),
    }
}

#[tokio::test]
async fn upstream_ysync_update_rejected() {
    let env = env().await;
    let deps = ChannelDeps {
        coordinator: env.coordinator.clone(),
        broadcast: env.broadcast.clone(),
        machine: env.machine.clone(),
        sessions: Arc::new(env.sessions.clone()),
        conns: env.conns.clone(),
    };
    let mut ch = SessionChannel::new(ctx("c"));
    let (tx, _rx) = mpsc::channel(8);
    // 先合法订阅（首帧纪律），再上行 update → UNSUPPORTED_FRAME（§5.6）。
    ch.dispatch(
        Frame::YsyncSubscribe(YsyncSubscribe {
            docs: vec![DocId::REGISTRY],
        }),
        &deps,
        tx.clone(),
    )
    .await;
    let o = ch
        .dispatch(
            Frame::YsyncUpdate(acp_hub_proto::ysync::YsyncUpdate {
                doc: DocId::REGISTRY,
                update: "AAAA".into(),
                projection_version: None,
            }),
            &deps,
            tx.clone(),
        )
        .await;
    match o {
        DispatchOutcome::Send(msgs) => match &msgs[0] {
            crate::channel::OutboundMsg::Frame(Frame::ActionError(e)) => {
                assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::UnsupportedFrame)
            }
            other => panic!("expected error, got {other:?}"),
        },
        other => panic!("expected send, got {other:?}"),
    }
}

#[tokio::test]
async fn readonly_cannot_send_action() {
    let env = env().await;
    let deps = ChannelDeps {
        coordinator: env.coordinator.clone(),
        broadcast: env.broadcast.clone(),
        machine: env.machine.clone(),
        sessions: Arc::new(env.sessions.clone()),
        conns: env.conns.clone(),
    };
    // read-only token（§9.2.2：M1 即强制，仅读）。
    let mut ro_ctx = ctx("ro");
    ro_ctx.role = TokenRole::ReadOnly;
    let mut ch = SessionChannel::new(ro_ctx);
    let (tx, _rx) = mpsc::channel(8);
    // 先订阅（首帧纪律）→ ready。
    ch.dispatch(
        Frame::YsyncSubscribe(YsyncSubscribe {
            docs: vec![DocId::REGISTRY],
        }),
        &deps,
        tx.clone(),
    )
    .await;
    ch.mark_ready();
    let action = Frame::Action(acp_hub_proto::action::ActionEnvelope::Prompt {
        command_id: uuid::Uuid::new_v4().to_string(),
        payload: PromptSessionPayload {
            session_id: "s1".into(),
            message: "x".into(),
        },
    });
    let o = ch.dispatch(action, &deps, tx).await;
    match o {
        DispatchOutcome::Send(msgs) => match &msgs[0] {
            crate::channel::OutboundMsg::Frame(Frame::ActionError(e)) => {
                assert_eq!(e.code, acp_hub_proto::ack::ErrorCode::UnsupportedFrame)
            }
            other => panic!("expected error, got {other:?}"),
        },
        other => panic!("expected send, got {other:?}"),
    }
}
