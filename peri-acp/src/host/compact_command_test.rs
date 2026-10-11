//! 手动 compact 经 executor、Host 收尾及真实 ACP 通知的跨轮回归。
use super::*;
use crate::session::event_sink::TransportEventSink;
use crate::transport::{mpsc::mpsc_transport_pair, types::IncomingMessage, AcpTransport};
use peri_acp_types::PeriCaps;

fn enable_compact_command(ctx: &mut SessionContext, model: Arc<dyn Model>) {
    let registry = Arc::new(crate::session::command::CommandRegistry::new());
    crate::session::command::register_builtins(&registry);
    ctx.command_lookup = Arc::new(move |text| registry.resolve(text));
    ctx.fresh_auxiliary_model = Some(Arc::new(move || model.clone()));
}

async fn run_manual_compact(ctx: SessionContext, sessions: SharedSessions) -> serde_json::Value {
    let (client, server) = mpsc_transport_pair();
    let caps = Arc::new(dashmap::DashMap::new());
    caps.insert(
        ctx.session_id.clone(),
        PeriCaps {
            agent_event_done: true,
            ..Default::default()
        },
    );
    let mut turn = make_compact_turn(&ctx, &sessions, false).await;
    turn.content = MessageContent::text("/compact");
    turn.event_sink = Arc::new(TransportEventSink::new(Arc::new(server), caps));
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(!result.persistence_inconsistent);
    assert!(result.failure.is_none());
    if result.stop_reason == PromptStopReason::EndTurn {
        assert!(
            result.history_replaced_by_compaction,
            "成功命令必须确认摘要提交"
        );
    }
    let response = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap();
    // 使用真实 EventSink + MPSC wire，避免只在记录型 sink 上验证映射。
    loop {
        let notification = tokio::time::timeout(std::time::Duration::from_secs(5), client.recv())
            .await
            .unwrap()
            .unwrap();
        if let IncomingMessage::Notification { method, params } = notification {
            if method == "peri/agent_event_done" {
                assert_eq!(params["sessionId"], ctx.session_id);
                assert_eq!(
                    params["stopReason"], response["stopReason"],
                    "done 通知与 PromptResponse 必须表达同一终态"
                );
                break;
            }
        }
    }
    response
}

/// [回归测试] Host 保存完整历史后，第二次命令不得拿它与 visible IDs 错配。
#[tokio::test]
#[serial]
async fn test_manual_compact_twice_through_host_preserves_canonical_history() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (mut ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    enable_compact_command(&mut ctx, Arc::new(SummaryModel));
    let original = store
        .load_payloads(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            run_manual_compact(ctx.clone(), sessions.clone()).await["stopReason"],
            "end_turn"
        );
    }
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 2);
    let snapshot = store
        .inner
        .load_session_snapshot(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(snapshot.payloads.len(), original.len() + 2);
    for payload in original {
        assert!(snapshot.flags[&payload.id()].excluded);
        assert_eq!(
            snapshot
                .payloads
                .iter()
                .filter(|item| item.id() == payload.id())
                .count(),
            1
        );
    }
    assert_next_turn_sees_summary(ctx, &sessions).await;
}

/// [回归测试] 自动 Full 与手动 Full 共用 canonical 历史，下一轮 Reason 不复活旧原文。
#[tokio::test]
#[serial]
async fn test_auto_full_then_manual_compact_through_host() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (mut ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    enable_compact_command(&mut ctx, Arc::new(SummaryModel));
    let turn = make_compact_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(result.history_replaced_by_compaction);
    finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap();
    run_manual_compact(ctx.clone(), sessions.clone()).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 2);
    assert_next_turn_sees_summary(ctx, &sessions).await;
}

/// [回归测试] 新连接冷读 canonical payload 和 flags 后，手动 compact 仍然可用。
#[tokio::test]
#[serial]
async fn test_cold_snapshot_then_manual_compact_through_host() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (mut ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    enable_compact_command(&mut ctx, Arc::new(SummaryModel));
    run_manual_compact(ctx.clone(), sessions.clone()).await;
    drop(sessions);
    let reader = peri_resources::sessions::SessionResourcesImpl::open_existing_read_only(
        dir.path().join("compact-history.db"),
    )
    .await
    .unwrap();
    let snapshot = reader
        .load_session_snapshot(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    assert!(snapshot.flags.values().any(|flags| flags.excluded));
    let restored = make_host_sessions(&ctx, snapshot.payloads);
    run_manual_compact(ctx.clone(), restored.clone()).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 2);
    assert_next_turn_sees_summary(ctx, &restored).await;
}

/// [回归测试] 手动摘要模型正在生成时取消，wire 必须 cancelled，且未提交原文保留。
#[tokio::test]
#[serial]
async fn test_manual_compact_model_cancel_has_consistent_wire_and_preserves_history() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (mut ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    let (entered, ready) = tokio::sync::oneshot::channel();
    enable_compact_command(
        &mut ctx,
        Arc::new(CancelGateModel {
            entered: Mutex::new(Some(entered)),
        }),
    );
    let before = store
        .load_payloads(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    let task = tokio::spawn(run_manual_compact(ctx.clone(), sessions.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), ready)
        .await
        .unwrap()
        .unwrap();
    ctx.cancel.cancel();
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response["stopReason"], "cancelled");
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 0);
    let after = store
        .load_payloads(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        after.iter().map(PersistedPayload::id).collect::<Vec<_>>(),
        before.iter().map(PersistedPayload::id).collect::<Vec<_>>()
    );
    assert!(sessions.lock().await[&ctx.session_id]
        .cancel_token
        .is_none());
}
