use super::*;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::RewindCandidate;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── 读取路径探针：候选查询不得再走全量历史 ────────────────────────────────────

struct CandidateReadSpy {
    inner: Arc<dyn SessionResources>,
    history_reads: AtomicUsize,
}

/// 只替换两条读取：`load_session_history` 记一次调用，`load_rewind_candidates` 委派真实
/// 门面；其余方法逐项转发（注入点唯一，故用宏而不是手写三十个转发方法）。
macro_rules! forward_session_resources_except_candidate_reads {
    (
        $name:ident
        $( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*
    ) => {
        #[async_trait]
        impl SessionResources for $name {
            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                self.inner.$method($( $argument ),*).await
            })*

            async fn load_session_history(
                &self,
                id: &ThreadId,
            ) -> SessionResourceResult<Vec<PersistedPayload>> {
                self.history_reads.fetch_add(1, Ordering::SeqCst);
                self.inner.load_session_history(id).await
            }

            async fn load_rewind_candidates(
                &self,
                id: &ThreadId,
                limit: usize,
            ) -> SessionResourceResult<Vec<RewindCandidate>> {
                self.inner.load_rewind_candidates(id, limit).await
            }
        }
    };
}

forward_session_resources_except_candidate_reads! {
    CandidateReadSpy
    inspect_availability(session: Option<&ThreadId>) -> SessionAvailability;
    resolve_workspace(cwd: &std::path::Path) -> ResolvedWorkspace;
    validate_session(id: &ThreadId, workspace: &ResolvedWorkspace) -> ();
    finish_close(id: &ThreadId) -> ();
    close_settlement(id: &ThreadId) -> CloseSettlement;
    create_session(input: &NewSession) -> ();
    abandon_initialization(id: &ThreadId) -> ();
    begin_initialization(draft: &NewSessionDraft) -> Arc<dyn SessionInitialization>;
    discard_incomplete_initialization(id: &ThreadId) -> ();
    adopt_legacy_session(id: &ThreadId, saved_cwd: &str, workspace: &ResolvedWorkspace, frozen: &FrozenSnapshotBytes) -> ();
    load_session_snapshot(id: &ThreadId) -> SessionSnapshot;
    load_frozen_state(id: &ThreadId) -> FrozenState;
    load_session_binding(id: &ThreadId) -> BindingState;
    validate_bound_workspace(id: &ThreadId, check: BindingRecheck) -> ResolvedWorkspace;
    load_session_meta(id: &ThreadId) -> ThreadMeta;
    list_sessions(query: &ScopedThreadQuery) -> ScopedThreadPage;
    list_children(parent: &ThreadId) -> Vec<ThreadMeta>;
    list_session_tree(root: &ThreadId) -> Vec<ThreadMeta>;
    append_history(id: &ThreadId, payloads: &[PersistedPayload]) -> ();
    append_reminder_if_absent(id: &ThreadId, message_id: MessageId, reminder: &TrustedSystemReminder) -> bool;
    mark_session_closing(id: &ThreadId) -> ();
    is_session_closing(id: &ThreadId) -> bool;
    save_fork(fork: &ForkSnapshot) -> ();
    save_child(child: &ChildSnapshot) -> ();
    claim_child_resume(child: &ThreadId, root: &ThreadId) -> Box<dyn ChildResumeClaim>;
    apply_compaction(id: &ThreadId, change: &CompactionChange) -> ();
    apply_message_projections(id: &ThreadId, updates: &[(MessageId, MessageFlags)]) -> ();
    rewind_history(id: &ThreadId, boundary: RewindBoundary, clear_excluded: &[MessageId]) -> RewindOutcome;
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}

/// session/rewind-candidates 路由到 dispatch：返回 user-only 候选。
#[tokio::test]
async fn test_rewind_candidates_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );

    let result = handle_request(
        "session/rewind-candidates",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    let value = result.unwrap();
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "只返回 user 消息");
}

/// session/rewind-preview 路由到 dispatch：返回 file_changes 数组（无工具调用 → 空）。
/// 目标取 history[2]（Human 消息）——与生产口径一致：rewind-candidates 只返回
/// user 消息，AI 消息永远不可能成为回滚目标。
#[tokio::test]
async fn test_rewind_preview_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );
    let target_id = sessions.get(&sid).unwrap().effective_messages()[2]
        .id()
        .as_uuid()
        .to_string();

    let result = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    let value = result.unwrap();
    let changes = value["file_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 0, "历史无工具调用 → 空预算");
}

/// session/rewind-preview：目标消息不存在时返回 not found 错误（生产 rewind_preview
/// 按 id 定位，history 之外的 id 一律拒绝）。「仅 AI 消息」场景由候选层保证不可达
/// （rewind-candidates 只返回 user 消息），UI 不可能选中 AI 消息作为目标。
#[tokio::test]
async fn test_rewind_preview_missing_target_returns_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );

    let result = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": "00000000-0000-0000-0000-000000000000" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "目标不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("未找到目标消息"),
        "错误消息应提及未找到目标，实际: {}",
        err.message,
    );
}

/// session/rewind 路由到 dispatch：执行回退（无 Write/Edit 时仅截断）。
#[tokio::test]
async fn test_rewind_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );
    let target_id = sessions.get(&sid).unwrap().effective_messages()[0]
        .id()
        .as_uuid()
        .to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let preview_fingerprint = preview["preview_fingerprint"].as_str().unwrap();

    let result = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target_id,
            "preview_fingerprint": preview_fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert_eq!(result.unwrap()["status"], "executed");

    // P1：rewind 后热态有效集合必须截断——它是后续候选/预算查询的
    // 数据源，不写回会导致第二次回退 not found。
    let s = sessions.get(&sid).unwrap();
    assert_eq!(
        s.effective_messages().len(),
        0,
        "回退到第一条后 history 应为空"
    );
    assert_eq!(
        s.history_payloads.len(),
        0,
        "rewind 必须同步裁剪 canonical history_payloads"
    );
}

/// 候选 handler 只调用有界读取：spy 的 `load_session_history` 必须保持 0 次。
#[tokio::test]
async fn test_rewind_candidates_never_reads_full_history() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );

    let spy = Arc::new(CandidateReadSpy {
        inner: cfg.session_resources.clone(),
        history_reads: AtomicUsize::new(0),
    });
    cfg.controller = Arc::new(peri_controller::Controller::new(spy.clone()));

    let value = handle_request(
        "session/rewind-candidates",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(value["messages"].as_array().unwrap().len(), 2);
    assert_eq!(
        spy.history_reads.load(Ordering::SeqCst),
        0,
        "候选查询不得回退到 load_session_history"
    );
}

/// 尾部纯 reminder + 超长用户文本：handler 仍返回最多 64 条有效候选，
/// preview 截断 200 字符、最新在前。
#[tokio::test]
async fn test_rewind_candidates_skips_trailing_reminders_and_truncates_preview() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );

    let mut extra: Vec<PersistedPayload> = (0..70)
        .map(|index| {
            let text = if index == 69 {
                format!("extra-{index}\n{}", "x".repeat(500))
            } else {
                format!("extra-{index}")
            };
            PersistedPayload::Message(BaseMessage::human(text))
        })
        .collect();
    extra.extend((0..10).map(|index| {
        PersistedPayload::Message(BaseMessage::human(format!(
            "<system-reminder>notice-{index}</system-reminder>"
        )))
    }));
    cfg.session_resources
        .append_history(&sid, &extra)
        .await
        .unwrap();

    let value = handle_request(
        "session/rewind-candidates",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 64, "尾部注入不占有效候选名额");
    let first = messages[0]["preview"].as_str().unwrap();
    assert!(first.starts_with("extra-69"), "最新在前: {first}");
    assert_eq!(first.chars().count(), 200, "preview 截断 200 字符");
    assert!(
        messages[1]["preview"].as_str().unwrap() == "extra-68",
        "纯注入被丢弃且不改变后续顺序"
    );
}
