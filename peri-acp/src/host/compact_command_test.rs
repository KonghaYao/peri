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
            "成功命令必须确认摘要提交: ok={} stop={:?} failure={:?} messages={:?}",
            result.ok,
            result.stop_reason,
            result.failure,
            result
                .messages
                .iter()
                .map(|message| message.content())
                .collect::<Vec<_>>()
        );
    }
    let response = finish_prompt_turn(
        &sessions,
        &ctx.session_id,
        false,
        result,
        ctx.session_resources.clone(),
    )
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
    finish_prompt_turn(
        &sessions,
        &ctx.session_id,
        false,
        result,
        ctx.session_resources.clone(),
    )
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
    // 热态只持有有效集合（excluded 原文留在 canonical 存储）：用 active 快照
    // 重建 Host，和 session/resume 的冷恢复一致。
    let active = reader
        .load_active_snapshot(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    let restored = make_host_sessions(&ctx, active.payloads);
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

/// [S1 原生复现] Full 提交后 Host 只采纳有效集合，对象级快照下降。
///
/// 真实临时 SQLite + 受控摘要模型；断言不依赖 RSS，也不承诺固定降幅。
#[tokio::test]
#[serial]
async fn test_finish_prompt_turn_adopts_only_active_set_and_reports_memory_shrink() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (mut ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    enable_compact_command(&mut ctx, Arc::new(SummaryModel));
    let thread_id = ctx.thread_id.clone().unwrap();

    // 扩大归档正文，使“摘要取代旧正文”的字节变化可观测；不改变 compact 语义。
    let extra: Vec<PersistedPayload> = (0..32)
        .map(|index| {
            PersistedPayload::Message(BaseMessage::human(format!(
                "OLD-EXTRA-{index:02}-{}",
                "z".repeat(512)
            )))
        })
        .collect();
    store.append_history(&thread_id, &extra).await.unwrap();
    {
        let mut sessions = sessions.lock().await;
        sessions
            .get_mut(&ctx.session_id)
            .unwrap()
            .history_payloads
            .extend(extra);
    }

    let before = {
        let sessions = sessions.lock().await;
        sessions[&ctx.session_id].history_memory_snapshot()
    };
    let response = run_manual_compact(ctx.clone(), sessions.clone()).await;
    assert_eq!(response["stopReason"], "end_turn");
    let after = {
        let sessions = sessions.lock().await;
        sessions[&ctx.session_id].history_memory_snapshot()
    };
    println!("Host adopt T1=[{before:?}] T2=[{after:?}]");

    // Host 旧正文副本被有效集合替换：条目、正文与容器槽位都下降。
    assert!(after.entries < before.entries);
    assert!(after.bytes.body < before.bytes.body);
    assert!(after.container_bytes < before.container_bytes);

    // Host 热态与存储的 active 快照逐 id 一致。
    let active = store.inner.load_active_snapshot(&thread_id).await.unwrap();
    assert_eq!(after.entries, active.payloads.len());
    let sessions = sessions.lock().await;
    let host = &sessions[&ctx.session_id];
    assert_eq!(
        host.context_ownership,
        crate::host::ContextOwnership::Resident
    );
    assert_eq!(
        host.history_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        active
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
    let host_text = host
        .effective_messages()
        .iter()
        .map(|message| message.content())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(host_text.contains(SUMMARY), "Host 必须持有已提交摘要");
    assert!(!host_text.contains(OLD), "Host 不得继续持有被排除正文");
    drop(sessions);

    // 权威存储仍保留归档正文：释放的是热态副本。
    let stored = store.load_payloads(&thread_id).await.unwrap();
    assert!(stored
        .iter()
        .filter_map(PersistedPayload::as_message)
        .any(|message| message.content() == OLD));
}

/// Full compact 的 checkpoint/tail 只能来自 durable commit 之后的存储事实。
#[tokio::test]
#[serial]
async fn test_manual_compact_publishes_history_window_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (mut ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    enable_compact_command(&mut ctx, Arc::new(SummaryModel));

    run_manual_compact(ctx.clone(), sessions.clone()).await;

    let thread_id = ctx
        .thread_id
        .clone()
        .expect("compact fixture binds a thread");
    let snapshot = store.inner.load_session_snapshot(&thread_id).await.unwrap();
    let summary = snapshot
        .payloads
        .iter()
        .find(|payload| {
            payload
                .as_message()
                .is_some_and(|message| message.content().contains(SUMMARY))
        })
        .expect("durable compact must append the summary payload");
    let expected =
        peri_acp_types::history_window::HistoryEntryId::new(&thread_id, summary.id()).encode();

    let states = sessions.lock().await;
    let state = states.get(&ctx.session_id).expect("session remains live");
    assert_eq!(
        state.history_window.checkpoint_id.as_deref(),
        Some(expected.as_str()),
        "checkpointId must be the durable summary entry identity"
    );
    assert_eq!(
        state.history_window.summary_entry_id.as_deref(),
        Some(expected.as_str())
    );
    assert!(
        state.history_window.committed_tail.is_some(),
        "compact must publish a committed tail"
    );
}
