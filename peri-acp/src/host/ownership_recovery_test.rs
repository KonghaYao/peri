//! A08 / A05 / A06：真实 Host 所有权收尾及 SQLite 恢复组合回归。

use super::*;
use crate::host::{
    connection::ConnectionFacts, history_window, ownership, prompt,
    prompt_dispatch::admission_gate, requests::session_lifecycle, AcpServerConfig,
    ContextOwnership,
};
use crate::transport::{mpsc::mpsc_transport_pair, AcpTransport};
use serde_json::json;
use std::time::Duration;

async fn config(
    dir: &tempfile::TempDir,
    ctx: &SessionContext,
    store: Arc<CompactStore>,
) -> Arc<AcpServerConfig> {
    let provider = LlmProvider::OpenAi {
        api_key: String::new(),
        base_url: String::new(),
        model: "fixture".into(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: 200_000,
        retry_observer: None,
    };
    let cfg = crate::host::requests::tests::build_server_config(
        PeriConfig::default(),
        provider,
        dir,
        store,
    )
    .await;
    cfg.session_manager
        .ensure_session(&ctx.session_id, &ctx.cwd);
    Arc::new(cfg)
}

async fn run_host_prompt(
    sid: String,
    sessions: SharedSessions,
    cfg: Arc<AcpServerConfig>,
    transport: Arc<dyn AcpTransport>,
) -> Result<serde_json::Value, crate::transport::types::AcpError> {
    prompt::run_prompt(
        json!({"sessionId": sid, "prompt": [{"type": "text", "text": "continue"}]}),
        &sessions,
        &cfg,
        &transport,
        Arc::new(parking_lot::Mutex::new(AgentPool::new())),
        None,
        false,
        None,
    )
    .await
}

async fn wait_entered(gate: &admission_gate::Gate) {
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
}

async fn restore(
    cfg: &AcpServerConfig,
    sessions: &SharedSessions,
    sid: &str,
    transport: &Arc<dyn AcpTransport>,
) {
    session_lifecycle::handle_load_shared(
        &json!({"sessionId": sid}),
        cfg,
        sessions,
        transport,
        ConnectionFacts::unnegotiated(),
    )
    .await
    .unwrap();
    assert_eq!(
        sessions.lock().await[sid].context_ownership,
        ContextOwnership::Resident
    );
}

async fn assert_active_store_matches(
    ctx: &SessionContext,
    store: &CompactStore,
    sessions: &SharedSessions,
) {
    let active = store
        .inner
        .load_active_snapshot(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    let states = sessions.lock().await;
    let state = &states[&ctx.session_id];
    assert_eq!(state.context_ownership, ContextOwnership::Resident);
    assert_eq!(
        state
            .history_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        active
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
    );
    assert!(!state.history_payloads.is_empty());
}

async fn open_ticket(cfg: &AcpServerConfig, sessions: &SharedSessions, sid: &str) {
    let (_client, server) = mpsc_transport_pair();
    let connection = history_window::WindowConnection::new(
        "ownership-test",
        Arc::new(history_window::TransportWindowSink::new(Arc::new(server))),
    );
    history_window::handle(
        cfg,
        sessions,
        &peri_acp_types::PeriCaps::all_enabled(),
        sid,
        peri_acp_types::history_window::HistoryWindowRequest::decode(
            &json!({"kind": "open", "direction": "before", "limit": 10}),
        )
        .unwrap(),
        &connection,
    )
    .await
    .unwrap();
    assert!(sessions.lock().await[sid].history_window.has_ticket());
}

async fn assert_reload_required(sessions: &SharedSessions, sid: &str) {
    let states = sessions.lock().await;
    let state = &states[sid];
    assert_eq!(state.context_ownership, ContextOwnership::ReloadRequired);
    assert!(state.history_payloads.is_empty());
    assert!(state.cancel_token.is_none());
    assert!(!state.history_window.has_ticket());
}

#[tokio::test]
#[serial]
async fn a08_abort_after_take_with_lock_contention_recovers_on_next_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    let cfg = config(&dir, &ctx, store.clone()).await;
    let (_client, server) = mpsc_transport_pair();
    let transport: Arc<dyn AcpTransport> = Arc::new(server);
    let gate = admission_gate::install(&format!("{}:handed-off", ctx.session_id));
    let task = tokio::spawn(run_host_prompt(
        ctx.session_id.clone(),
        sessions.clone(),
        cfg.clone(),
        transport.clone(),
    ));
    wait_entered(&gate).await;
    assert_eq!(
        sessions.lock().await[&ctx.session_id].context_ownership,
        ContextOwnership::HandedOff
    );
    // 正常在途 attempt 不得被冷恢复，也不得凭空发布正文。
    let error = prompt::ensure_resident_context(
        &sessions,
        &ctx.session_id,
        &cfg.controller,
        cfg.session_resources.as_ref(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32010);
    // 持 sessions 锁直到 abort 完成，确定性迫使 Drop 的 try_lock 失败。
    let states = sessions.lock().await;
    task.abort();
    assert!(tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap_err()
        .is_cancelled());
    assert_eq!(
        states[&ctx.session_id].context_ownership,
        ContextOwnership::HandedOff
    );
    assert!(!states[&ctx.session_id]
        .history_window
        .handed_off_alive
        .as_ref()
        .unwrap()
        .load(Ordering::Acquire));
    drop(states);
    // 再次真正进入 run_prompt；在第二次 take 后停住，证明未永久 -32010。
    let gate = admission_gate::install(&format!("{}:handed-off", ctx.session_id));
    let task = tokio::spawn(run_host_prompt(
        ctx.session_id.clone(),
        sessions.clone(),
        cfg.clone(),
        transport.clone(),
    ));
    wait_entered(&gate).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_reload_required(&sessions, &ctx.session_id).await;
    restore(&cfg, &sessions, &ctx.session_id, &transport).await;
    assert_active_store_matches(&ctx, &store, &sessions).await;
}

#[tokio::test]
#[serial]
async fn a08_dropped_future_recovers_via_restore_without_cleanup_task() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    let cfg = config(&dir, &ctx, store.clone()).await;
    let (_client, server) = mpsc_transport_pair();
    let transport: Arc<dyn AcpTransport> = Arc::new(server);
    let gate = admission_gate::install(&format!("{}:handed-off", ctx.session_id));
    let mut future = Box::pin(run_host_prompt(
        ctx.session_id.clone(),
        sessions.clone(),
        cfg.clone(),
        transport.clone(),
    ));
    tokio::select! {
        result = &mut future => panic!("关卡前不应完成: {result:?}"),
        _ = wait_entered(&gate) => {}
    }
    let states = sessions.lock().await;
    drop(future);
    assert_eq!(
        states[&ctx.session_id].context_ownership,
        ContextOwnership::HandedOff
    );
    drop(states);
    restore(&cfg, &sessions, &ctx.session_id, &transport).await;
    assert_active_store_matches(&ctx, &store, &sessions).await;
}

#[tokio::test]
#[serial]
async fn a08_run_session_start_failure_requires_reload_and_restores() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    let cfg = config(&dir, &ctx, store.clone()).await;
    let (_client, server) = mpsc_transport_pair();
    let transport: Arc<dyn AcpTransport> = Arc::new(server);
    let gate = admission_gate::install(&format!("{}:registered", ctx.session_id));
    let task = tokio::spawn(run_host_prompt(
        ctx.session_id.clone(),
        sessions.clone(),
        cfg.clone(),
        transport.clone(),
    ));
    wait_entered(&gate).await;
    // 在 register 与 run_session 之间删除真实 Runtime 登记，不伪造执行器结果。
    cfg.controller
        .destroy_session(&ctx.session_id, Duration::from_secs(1))
        .await
        .unwrap();
    gate.release.notify_one();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(error.message.contains("run_session failed"), "{error:?}");
    assert_reload_required(&sessions, &ctx.session_id).await;
    restore(&cfg, &sessions, &ctx.session_id, &transport).await;
    assert_active_store_matches(&ctx, &store, &sessions).await;
}

#[tokio::test]
#[serial]
async fn a08_finished_or_superseded_guard_does_not_invalidate_current_context() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    for outcome in 0..3 {
        let (payloads, guard) = {
            let mut states = sessions.lock().await;
            ownership::begin_handoff(
                states.get_mut(&ctx.session_id).unwrap(),
                &sessions,
                &ctx.session_id,
            )
        };
        let mut result = synthetic_result(false);
        result.persisted_payloads = payloads;
        if outcome == 1 {
            result.ok = false;
            result.stop_reason = PromptStopReason::Cancelled;
        } else if outcome == 2 {
            result.ok = false;
            result.failure = Some(peri_acp_types::session::ExecutionFailure::internal(
                "模型失败",
            ));
        }
        let response = finish_prompt_turn(
            &sessions,
            &ctx.session_id,
            false,
            result,
            Some(store.clone()),
        )
        .await;
        assert_eq!(response.is_err(), outcome == 2);
        if outcome == 1 {
            assert_eq!(response.unwrap()["stopReason"], "cancelled");
        }
        drop(guard);
        assert_active_store_matches(&ctx, &store, &sessions).await;
    }
    let (payloads, old_guard) = {
        let mut states = sessions.lock().await;
        ownership::begin_handoff(
            states.get_mut(&ctx.session_id).unwrap(),
            &sessions,
            &ctx.session_id,
        )
    };
    let mut result = synthetic_result(false);
    result.persisted_payloads = payloads;
    finish_prompt_turn(
        &sessions,
        &ctx.session_id,
        false,
        result,
        Some(store.clone()),
    )
    .await
    .unwrap();
    let (_, current_guard) = {
        let mut states = sessions.lock().await;
        ownership::begin_handoff(
            states.get_mut(&ctx.session_id).unwrap(),
            &sessions,
            &ctx.session_id,
        )
    };
    drop(old_guard);
    assert_eq!(
        sessions.lock().await[&ctx.session_id].context_ownership,
        ContextOwnership::HandedOff
    );
    drop(current_guard);
    assert_reload_required(&sessions, &ctx.session_id).await;
}

#[tokio::test]
#[serial]
async fn a05_inconsistent_result_revokes_ticket_then_restores_sqlite_active_set() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    let cfg = config(&dir, &ctx, store.clone()).await;
    open_ticket(&cfg, &sessions, &ctx.session_id).await;
    let mut result = synthetic_result(false);
    result.persistence_inconsistent = true;
    result.persisted_payloads = vec![PersistedPayload::Message(BaseMessage::human("不可信热态"))];
    finish_prompt_turn(
        &sessions,
        &ctx.session_id,
        false,
        result,
        Some(store.clone()),
    )
    .await
    .unwrap();
    assert_reload_required(&sessions, &ctx.session_id).await;
    let (_client, server) = mpsc_transport_pair();
    let transport: Arc<dyn AcpTransport> = Arc::new(server);
    restore(&cfg, &sessions, &ctx.session_id, &transport).await;
    assert_active_store_matches(&ctx, &store, &sessions).await;
}

#[tokio::test]
#[serial]
async fn a06_full_commit_then_flush_failure_preserves_archive_and_restores_only_active() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_compact_context(&dir, Arc::new(SummaryModel)).await;
    let cfg = config(&dir, &ctx, store.clone()).await;
    open_ticket(&cfg, &sessions, &ctx.session_id).await;
    store
        .fail_append_after_compact
        .store(true, Ordering::SeqCst);
    let mut turn = make_compact_turn(&ctx, &sessions, true).await;
    let guard = {
        let mut states = sessions.lock().await;
        let (payloads, guard) = ownership::begin_handoff(
            states.get_mut(&ctx.session_id).unwrap(),
            &sessions,
            &ctx.session_id,
        );
        turn.history_payloads = payloads;
        guard
    };
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 1);
    assert!(
        store.failed_appends.load(Ordering::SeqCst) > 0,
        "必须实际触发同轮后续追加失败"
    );
    assert!(result.persistence_inconsistent);
    // flush 失败时结果不声明可采纳的 compact 快照；提交事实由 SQLite 断言。
    assert!(!result.history_replaced_by_compaction);
    assert!(result.failure.is_some());
    assert!(finish_prompt_turn(
        &sessions,
        &ctx.session_id,
        false,
        result,
        Some(store.clone())
    )
    .await
    .is_err());
    drop(guard);
    assert_reload_required(&sessions, &ctx.session_id).await;
    let snapshot = store
        .inner
        .load_session_snapshot(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    let archived = snapshot
        .payloads
        .iter()
        .find(|payload| {
            payload
                .as_message()
                .is_some_and(|message| message.content() == OLD)
        })
        .unwrap();
    assert!(snapshot.flags[&archived.id()].excluded);
    assert!(snapshot.payloads.iter().any(|payload| payload
        .as_message()
        .is_some_and(|message| message.content().contains(SUMMARY))));
    store
        .fail_append_after_compact
        .store(false, Ordering::SeqCst);
    let (_client, server) = mpsc_transport_pair();
    let transport: Arc<dyn AcpTransport> = Arc::new(server);
    restore(&cfg, &sessions, &ctx.session_id, &transport).await;
    assert_active_store_matches(&ctx, &store, &sessions).await;
    let text = sessions.lock().await[&ctx.session_id]
        .effective_messages()
        .iter()
        .map(|message| message.content())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains(SUMMARY));
    assert!(!text.contains(OLD));
    assert_next_turn_sees_summary(ctx, &sessions).await;
}

// SQLite 写锁阻止提交；取消真实 append future，让 MutationGate::WriteScope::drop
// 留下 sticky Unknown。drain 的阻塞证明写入已取得 gate，而非仅在等待调度。
async fn make_sticky_unknown(dir: &tempfile::TempDir, store: &CompactStore, id: &ThreadId) {
    use sqlx::Connection;
    let options =
        sqlx::sqlite::SqliteConnectOptions::new().filename(dir.path().join("compact-history.db"));
    let mut blocker = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut blocker)
        .await
        .unwrap();
    let payloads = [PersistedPayload::Message(BaseMessage::human(
        "被取消的真实写入",
    ))];
    let mut writing = Box::pin(store.append_history(id, &payloads));
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut writing => panic!("写锁释放前不得完成: {result:?}"),
            _ = async {
                loop {
                    if tokio::time::timeout(Duration::from_millis(20), store.drain_persistence(id))
                        .await.is_err()
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            } => {},
        }
    })
    .await
    .unwrap();
    drop(writing);
    sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();
    assert!(store
        .drain_persistence(id)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    assert!(store
        .append_history(id, &payloads)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
    // active snapshot 仍可读取；这不能解除写入不确定性。
    store.inner.load_active_snapshot(id).await.unwrap();
    assert!(store
        .drain_persistence(id)
        .await
        .unwrap_err()
        .is_persistence_uncertain());
}

struct RecoveryModel(Arc<AtomicUsize>);

#[async_trait]
impl Model for RecoveryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_streaming: true,
            ..Default::default()
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        cancellation: AgentCancellationToken,
    ) -> ModelResult<ModelStream> {
        let call = self.0.fetch_add(1, Ordering::SeqCst);
        let (message, stop) = if call == 0 {
            (
                ModelMessage::assistant(
                    vec![],
                    vec![peri_model::ToolCall::new(
                        "recovery-call",
                        "RecoveryProbe",
                        peri_model::JsonObject::from_value(json!({})).unwrap(),
                    )],
                ),
                StopReason::ToolUse,
            )
        } else {
            (
                ModelMessage::assistant_text("恢复后正常续聊"),
                StopReason::EndTurn,
            )
        };
        let response = ModelResponse::new(message, stop, None, None)?;
        Ok(ModelStream::with_parent_cancellation(
            stream::iter(vec![Ok(ModelStreamEvent::Completed(response))]),
            cancellation,
        ))
    }
}

struct RecoveryTool(Arc<AtomicUsize>);

#[async_trait]
impl peri_agent::tools::BaseTool for RecoveryTool {
    fn name(&self) -> &str {
        "RecoveryProbe"
    }
    fn description(&self) -> &str {
        "记录恢复测试中的真实工具执行次数"
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {}})
    }
    fn is_direct(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: peri_acp_types::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok("恢复工具已执行".into())
    }
}

async fn sticky_unknown_prompt_case(recover: bool) {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let calls = Arc::new(AtomicUsize::new(0));
    let tools = Arc::new(AtomicUsize::new(0));
    let model: Arc<dyn Model> = Arc::new(RecoveryModel(calls.clone()));
    let (ctx, store, sessions) = make_compact_context(&dir, model.clone()).await;
    let cfg = config(&dir, &ctx, store.clone()).await;
    cfg.shared_tools.write().insert(
        "RecoveryProbe".into(),
        Arc::new(RecoveryTool(tools.clone())),
    );
    // 无 historyWindowV1 协商，且 PeriConfig 使用默认 beta 关闭配置。
    cfg.session_manager
        .set_pending_caps(peri_acp_types::PeriCaps::default());
    cfg.session_manager.consume_pending_caps(&ctx.session_id);
    assert!(
        !cfg.session_manager
            .get_caps(&ctx.session_id)
            .history_window_v1
    );
    assert!(!peri_config::betas::from_merged(&cfg.peri_config.read())
        .is_enabled(peri_config::betas::HISTORY_WINDOW_V1));
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let fingerprint = crate::session::agent_pool::fingerprint(&cfg.provider.read());
    pool.lock()
        .subagent_llm_cache
        .insert(fingerprint.clone(), model.clone());
    pool.lock()
        .store_llm(crate::session::agent_pool::CachedLlmInstances {
            auxiliary_model: model,
            auto_classifier_model: Arc::new(tokio::sync::Mutex::new(Box::new(RecoveryModel(
                calls.clone(),
            )))),
            fingerprint,
        });
    let (_client, server) = mpsc_transport_pair();
    let transport: Arc<dyn AcpTransport> = Arc::new(server);
    let id = ctx.thread_id.as_ref().unwrap();
    make_sticky_unknown(&dir, &store, id).await;
    // 第一遍覆盖 Resident 未及时收到失效结果；第二遍覆盖审查指出的 ReloadRequired。
    for _ in 0..2 {
        let error = prompt::run_prompt(
            json!({"sessionId": ctx.session_id, "prompt": [{"type": "text", "text": "continue"}]}),
            &sessions,
            &cfg,
            &transport,
            pool.clone(),
            None,
            false,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32010, "{error:?}");
        assert!(
            error.message.contains("explicit persistence recovery"),
            "{error:?}"
        );
        assert_reload_required(&sessions, &ctx.session_id).await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "Unknown 时 provider 调用必须为零"
        );
        assert_eq!(
            tools.load(Ordering::SeqCst),
            0,
            "Unknown 时工具执行必须为零"
        );
        assert!(store
            .drain_persistence(id)
            .await
            .unwrap_err()
            .is_persistence_uncertain());
    }
    if recover {
        assert_eq!(
            store.recover_session_persistence(id).await.unwrap(),
            PersistenceRecovery::Recovered
        );
        store.drain_persistence(id).await.unwrap();
        let response = prompt::run_prompt(
            json!({"sessionId": ctx.session_id, "prompt": [{"type": "text", "text": "continue"}]}),
            &sessions,
            &cfg,
            &transport,
            pool,
            None,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(response["stopReason"], "end_turn");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(tools.load(Ordering::SeqCst), 1);
        assert_active_store_matches(&ctx, &store, &sessions).await;
        store.drain_persistence(id).await.unwrap();
    }
}

#[tokio::test]
#[serial]
async fn sticky_unknown_rejects_prompt_before_provider_and_tools_without_beta() {
    sticky_unknown_prompt_case(false).await;
}

#[tokio::test]
#[serial]
async fn sticky_unknown_explicit_recovery_allows_normal_prompt_without_beta() {
    sticky_unknown_prompt_case(true).await;
}
