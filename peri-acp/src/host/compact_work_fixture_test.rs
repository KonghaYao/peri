use super::*;

pub(super) async fn assert_blocked_preserves_summary(
    mut ctx: SessionContext,
    sessions: &SharedSessions,
) {
    use peri_acp_types::session_resources::work::{WorkQuery, WorkStage};
    let resources = ctx.session_resources.clone().unwrap();
    let query = WorkQuery {
        session_id: ctx.session_id.clone(),
        limit: 100,
    };
    let before = resources.load_session_work(&query).await.unwrap();
    assert!(before.blocked);
    assert!(before
        .state
        .works
        .values()
        .any(|work| work.stage == WorkStage::Blocked && work.reason_request.is_some()));
    let payloads = resources
        .load_session_history(ctx.thread_id.as_ref().unwrap())
        .await
        .unwrap();
    assert!(payloads.iter().any(|payload| payload
        .as_message()
        .is_some_and(|message| message.content().contains(SUMMARY))));
    ctx.session_access = None;
    execution_fixture::bind_execution(&mut ctx, None);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    ctx.primary_llm_factory = Some(Arc::new(move || {
        Arc::new(CapturePromptModel {
            requests: observed.clone(),
        })
    }));
    ctx.cancel = AgentCancellationToken::new();
    let turn = make_recovery_turn(&ctx, sessions, false).await;
    let result = run_session_loop(ctx, turn).await;
    assert!(!result.ok);
    assert!(
        requests.lock().unwrap().is_empty(),
        "unresolved Reason must not be retried"
    );
    let after = resources.load_session_work(&query).await.unwrap();
    assert!(after.blocked);
    for (work_id, original) in before
        .state
        .works
        .iter()
        .filter(|(_, work)| work.stage == WorkStage::Blocked)
    {
        assert_eq!(
            after.state.works[work_id].reason_request,
            original.reason_request
        );
    }
}

/// [回归测试] Full 提交后 cancel 的结果仍更新热 host，下一轮恢复摘要与 flags。
#[tokio::test]
#[serial]
async fn test_full_compact_cancel_preserves_summary_and_blocks_retry() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let model: Arc<dyn Model> = Arc::new(CancelGateModel {
        entered: Mutex::new(Some(entered_tx)),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let running_ctx = ctx.clone();
    let task = tokio::spawn(async move { run_session_loop(running_ctx, turn).await });
    entered_rx.await.unwrap();
    assert_eq!(
        store.compact_commits.load(Ordering::SeqCst),
        1,
        "取消必须发生在真实 Full 提交之后"
    );
    ctx.cancel.cancel();
    let result = task.await.unwrap();
    assert!(!result.ok);
    assert!(result.history_replaced_by_compaction);
    assert!(!result.persistence_inconsistent);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap();
    assert_eq!(wire["stopReason"], "cancelled");
    assert!(sessions.lock().await[&ctx.session_id]
        .cancel_token
        .is_none());
    assert_blocked_preserves_summary(ctx, &sessions).await;
}

/// [回归测试] Full 提交后的 fatal LLM error 保持 wire error，同时采纳 canonical progress。
#[tokio::test]
#[serial]
async fn test_full_compact_llm_error_preserves_summary_and_blocks_retry() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_recovery_context(&dir, Arc::new(FatalModel), false).await;
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 1);
    assert!(!result.ok);
    assert!(result.history_replaced_by_compaction);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(
        wire.code,
        crate::host::prompt::ACP_TURN_EXECUTION_FAILED_CODE
    );
    assert_eq!(wire.data.unwrap()["kind"], "llm_http");
    assert_blocked_preserves_summary(ctx, &sessions).await;
}

/// [回归测试] Full 提交后的 forwarder 失败不丢 canonical 摘要。
#[tokio::test]
#[serial]
async fn test_full_compact_forwarder_error_preserves_next_turn_summary() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    let mut turn = make_recovery_turn(&ctx, &sessions, true).await;
    turn.forwarder_launcher = make_aborting_forwarder_launcher();
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 1);
    assert!(!result.ok);
    assert!(result.history_replaced_by_compaction);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert_next_turn_sees_summary(ctx, &sessions).await;
}

/// [回归测试] Full 事务后追加失败，不能按 turn ID 删除摘要；新 SQLite owner 可恢复。
#[tokio::test]
#[serial]
async fn test_full_compact_work_commit_unknown_cold_resolve_preserves_summary() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let (mut ctx, store, sessions) = make_recovery_context(&dir, model, true).await;
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 1);
    assert!(!result.ok);
    let original = assert_unknown_command(&ctx, &store).await;
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert_eq!(
        store.deletes.load(Ordering::SeqCst),
        0,
        "不得删除已提交 lifecycle 的消息"
    );
    // 释放原持久化句柄，再打开全新的 SQLite store（桥与门面重新配对）。
    ctx.session_resources = None;
    drop(store);
    let (recovered, recovered_facade) =
        peri_resources::sessions::open_store_and_facade_for_tests(dir.path().join("recovery.db"))
            .await
            .unwrap();
    let resolution = recovered_facade
        .resolve_work_mutation(&original)
        .await
        .unwrap();
    assert!(matches!(
        resolution,
        peri_acp_types::session_resources::work::WorkResolution::Applied { .. }
    ));
    let thread_id = ctx.thread_id.as_ref().unwrap();
    let payloads = recovered.load_payloads(thread_id).await.unwrap();
    let flags = recovered.load_message_flags(thread_id).await.unwrap();
    let old = payloads
        .iter()
        .find(|p| p.as_message().is_some_and(|m| m.content() == OLD))
        .unwrap();
    assert!(flags[&old.id()].excluded);
    assert_eq!(
        payloads
            .iter()
            .filter(|p| p
                .as_message()
                .is_some_and(|m| m.content().contains(SUMMARY)))
            .count(),
        1
    );
    ctx.session_resources = Some(Arc::new(recovered_facade));
    let cold_sessions = make_host_sessions(&ctx, payloads);
    assert_next_turn_sees_summary(ctx, &cold_sessions).await;
}

/// [回归测试] 未发生 Full 的 writer 错误同样移除热状态，不能假称 ID 回滚成功。
#[tokio::test]
#[serial]
async fn test_work_journal_unknown_without_compact_preserves_durable_history() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    store.fail_claim.store(true, Ordering::SeqCst);
    let turn = make_recovery_turn(&ctx, &sessions, false).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(!result.ok);
    let original = assert_unknown_command(&ctx, &store).await;
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 0);
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .load_payloads(ctx.thread_id.as_ref().unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
    let fresh =
        peri_resources::sessions::SessionResourcesImpl::open(dir.path().join("recovery.db"))
            .await
            .unwrap();
    assert_eq!(
        fresh.resolve_work_mutation(&original).await.unwrap(),
        peri_acp_types::session_resources::work::WorkResolution::NotApplied
    );
    assert!(
        fresh.apply_work_mutation(&original).await.is_err(),
        "NotApplied seals the original command"
    );
}

/// [回归测试] 未执行的真实 PromptHandle 不提供可验证 snapshot，host 必须要求冷加载。
#[tokio::test]
#[serial]
async fn test_missing_prompt_result_evicts_host_without_adopting_empty_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_recovery_context(&dir, Arc::new(FatalModel), false).await;
    let turn = make_recovery_turn(&ctx, &sessions, false).await;
    let handle = crate::host::prompt_handle::PromptHandle::new(ctx.clone(), turn);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, handle.take_result())
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert!(!sessions.lock().await.contains_key(&ctx.session_id));
    assert_eq!(
        store
            .load_payloads(ctx.thread_id.as_ref().unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
}

/// [回归测试] 装配失败仍回传原 canonical snapshot，可以保留热状态；区别于缺失结果。
#[tokio::test]
#[serial]
async fn test_stage_initialization_failure_preserves_verified_previous_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let (ctx, store, sessions) = make_recovery_context(&dir, Arc::new(FatalModel), false).await;
    let mut turn = make_recovery_turn(&ctx, &sessions, false).await;
    let previous_ids = turn
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect::<Vec<_>>();
    turn.stage_build = Arc::new(|_| {
        Err(
            peri_agent::session::exec::stage_builder::StageBuildError::ToolCatalog(
                peri_agent::session::tool_catalog::CatalogRefreshError::AliasConflict,
            ),
        )
    });
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(!result.ok);
    assert!(!result.persistence_inconsistent);
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    let sessions = sessions.lock().await;
    let state = sessions
        .get(&ctx.session_id)
        .expect("装配前未修改持久化，热状态仍有效");
    assert_eq!(
        state
            .history_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        previous_ids
    );
    assert!(state.cancel_token.is_none());
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
}

async fn assert_unconfirmed_commit_requires_cold_recovery(cancel_after_commit: bool) {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: requests.clone(),
    });
    let (mut ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    *store.after_commit.lock().unwrap() = Some(if cancel_after_commit {
        AfterCommitAction::Cancel(ctx.cancel.clone())
    } else {
        AfterCommitAction::Error
    });
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert_eq!(
        store.compact_commits.load(Ordering::SeqCst),
        1,
        "故障必须发生在真实COMMIT之后: {:?}",
        result.failure
    );
    assert!(
        requests.lock().unwrap().is_empty(),
        "未知持久化状态不能继续Reason读取旧快照"
    );
    assert!(!result.ok);
    assert!(
        result.persistence_inconsistent,
        "普通writer barrier成功不能抹去未确认commit"
    );
    assert_eq!(
        result.failure.as_ref().unwrap().kind,
        peri_acp_types::session::ExecutionFailureKind::Internal
    );
    assert_eq!(
        result.stop_reason,
        PromptStopReason::EndTurn,
        "未知持久化沿用Internal收尾，不能伪装普通cancel"
    );
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert!(wire.message.contains("reload"));
    assert!(!sessions.lock().await.contains_key(&ctx.session_id));
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
    // Reopen the persisted history through a fresh store facade.
    ctx.session_resources = None;
    drop(store);
    let (recovered, recovered_facade) =
        peri_resources::sessions::open_store_and_facade_for_tests(dir.path().join("recovery.db"))
            .await
            .unwrap();
    let thread_id = ctx.thread_id.as_ref().unwrap();
    let payloads = recovered.load_payloads(thread_id).await.unwrap();
    let flags = recovered.load_message_flags(thread_id).await.unwrap();
    let old = payloads
        .iter()
        .find(|p| p.as_message().is_some_and(|m| m.content() == OLD))
        .unwrap();
    assert!(flags[&old.id()].excluded);
    assert_eq!(
        payloads
            .iter()
            .filter(|p| p
                .as_message()
                .is_some_and(|m| m.content().contains(SUMMARY)))
            .count(),
        1
    );
    ctx.session_resources = Some(Arc::new(recovered_facade));
    let cold_sessions = make_host_sessions(&ctx, payloads);
    assert_next_turn_sees_summary(ctx, &cold_sessions).await;
}

/// [回归测试] SQLite已COMMIT但尚未apply内存时取消，不能把旧snapshot当成功flush结果。
#[tokio::test]
#[serial]
async fn test_full_compact_cancel_after_durable_commit_requires_cold_recovery() {
    assert_unconfirmed_commit_requires_cold_recovery(true).await;
}

/// [回归测试] store已COMMIT后返回Err，不能降级继续Reason或采纳旧snapshot。
#[tokio::test]
#[serial]
async fn test_full_compact_error_after_durable_commit_requires_cold_recovery() {
    assert_unconfirmed_commit_requires_cold_recovery(false).await;
}

/// [回归测试] Full内部barrier消费写错误后，Phase8的成功barrier仍不得证明快照安全。
#[tokio::test]
#[serial]
async fn test_full_compact_preclaim_journal_unknown_blocks_before_model() {
    let dir = tempfile::tempdir().unwrap();
    let _home = HomeGuard::set(dir.path());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let model: Arc<dyn Model> = Arc::new(CapturePromptModel {
        requests: requests.clone(),
    });
    let (ctx, store, sessions) = make_recovery_context(&dir, model, false).await;
    store.fail_claim.store(true, Ordering::SeqCst);
    let turn = make_recovery_turn(&ctx, &sessions, true).await;
    let result = run_session_loop(ctx.clone(), turn).await;
    assert!(!result.ok);
    let original = assert_unknown_command(&ctx, &store).await;
    assert_eq!(store.compact_commits.load(Ordering::SeqCst), 0);
    assert!(requests.lock().unwrap().is_empty());
    let wire = finish_prompt_turn(&sessions, &ctx.session_id, false, result)
        .await
        .unwrap_err();
    assert_eq!(wire.data.unwrap()["kind"], "internal");
    assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .load_payloads(ctx.thread_id.as_ref().unwrap())
            .await
            .unwrap()
            .len(),
        2
    );
    let fresh =
        peri_resources::sessions::SessionResourcesImpl::open(dir.path().join("recovery.db"))
            .await
            .unwrap();
    assert_eq!(
        fresh.resolve_work_mutation(&original).await.unwrap(),
        peri_acp_types::session_resources::work::WorkResolution::NotApplied
    );
}

pub(super) async fn assert_unknown_command(
    ctx: &SessionContext,
    store: &RecoveryStore,
) -> peri_acp_types::session_resources::work::WorkCommand {
    use peri_acp_types::session_resources::work::{WorkQuery, WorkResolution};
    let original = store
        .uncertain
        .lock()
        .unwrap()
        .clone()
        .expect("real journal must retain original command");
    let snapshot = store
        .load_session_work(&WorkQuery {
            session_id: ctx.session_id.clone(),
            limit: 100,
        })
        .await
        .unwrap();
    assert!(snapshot.blocked);
    assert!(snapshot.pending_commands.contains(&original));
    let entry = store
        .load_work_command(&peri_acp_types::session_resources::work::WorkCommandQuery {
            session_id: original.session_id.clone(),
            mutation_id: original.mutation_id.clone(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.command, original);
    assert!(entry.pending);
    assert_eq!(
        store.resolve_work_mutation(&original).await.unwrap(),
        WorkResolution::Unknown
    );
    let recovered = crate::host::work_recovery::resolve_pending(
        store,
        &WorkQuery {
            session_id: ctx.session_id.clone(),
            limit: 100,
        },
    )
    .await
    .unwrap();
    assert!(recovered.blocked);
    assert!(recovered.pending_commands.contains(&original));
    original
}
