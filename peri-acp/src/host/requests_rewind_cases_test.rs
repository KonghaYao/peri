use super::*;
use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement, ForkSnapshot,
    FrozenSnapshotBytes, NewSession, NewSessionDraft, PersistenceRecovery, RewindBoundary,
    RewindOutcome, SessionAvailability, SessionInitialization, SessionMetaPatch,
    SessionResourceError, SessionResourceResult, SessionResources, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, InheritedContext, MessageFlags, PersistedPayload};
use peri_acp_types::system_reminder::TrustedSystemReminder;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery};

// ── R2：rewind 以存储为事实源 ─────────────────────────────────────────────────

fn enable_rewind_cap(cfg: &AcpServerConfig, sid: &str) {
    cfg.session_manager.caps_registry().insert(
        sid.to_string(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );
}

/// 把已注册会话改造成「存储里有归档区、热态只有有效集合」的形态（Full Compact 的效果）：
/// `history[0]`/`history[1]` 在存储里标为 `excluded`，`history[2]` 仍是有效的尾条目；
/// Host 快照只留有效的那一条（[`SessionState::history_payloads`] 的有效集合语义）。
///
/// 返回 `(被归档的第一条, 被归档的第二条, 仍然有效的尾条目)` 的 id。
async fn archive_prefix_in_storage(
    sessions: &mut HashMap<String, SessionState>,
    cfg: &AcpServerConfig,
    sid: &str,
) -> (
    peri_acp_types::messages::MessageId,
    peri_acp_types::messages::MessageId,
    peri_acp_types::messages::MessageId,
) {
    let (h1, a1) = {
        let s = sessions.get(sid).unwrap();
        (s.history[0].id(), s.history[1].id())
    };
    let excluded = peri_acp_types::store::MessageFlags {
        excluded: true,
        ..Default::default()
    };
    cfg.session_resources
        .apply_message_projections(
            &peri_acp_types::thread::ThreadId::from(sid.to_string()),
            &[(h1, excluded.clone()), (a1, excluded)],
        )
        .await
        .unwrap();
    let state = sessions.get_mut(sid).unwrap();
    let summary = state.history[2].clone();
    state.history = vec![summary.clone()];
    state.history_payloads = vec![PersistedPayload::Message(summary)];
    (h1, a1, state.history[0].id())
}

fn history_ids(sessions: &HashMap<String, SessionState>, sid: &str) -> Vec<String> {
    sessions
        .get(sid)
        .unwrap()
        .history
        .iter()
        .map(|message| message.id().as_uuid().to_string())
        .collect()
}

/// 热态有效集合（`history_payloads`）的 id 序列。
fn payload_ids(sessions: &HashMap<String, SessionState>, sid: &str) -> Vec<MessageId> {
    sessions
        .get(sid)
        .unwrap()
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect()
}

/// 同时开 `rewind` 与 `agent_event`：前者放行请求，后者让 `RewindCompleted` 真的走通知
/// 通道（cap 缺失时事件被静默丢弃，断言会假绿）。
fn enable_rewind_and_event_caps(cfg: &AcpServerConfig, sid: &str) {
    cfg.session_manager.caps_registry().insert(
        sid.to_string(),
        PeriCaps {
            rewind: true,
            agent_event: true,
            ..PeriCaps::default()
        },
    );
}

/// mock transport 上收到的 `peri/agent_event` 事件中，指定 `type` 的载荷（`value`）。
fn agent_events(mock: &MockTransport, expected_type: &str) -> Vec<Value> {
    let mut events = Vec::new();
    for (method, params) in mock.notifications() {
        if method != "peri/agent_event" {
            continue;
        }
        let Some(event_json) = params.get("event_json").and_then(Value::as_str) else {
            continue;
        };
        let parsed: Value = serde_json::from_str(event_json).expect("event_json 必须是 JSON");
        if parsed.get("type").and_then(Value::as_str) == Some(expected_type) {
            events.push(parsed.get("value").cloned().unwrap_or(Value::Null));
        }
    }
    events
}

/// 只在 durable 提交上做文章的故障注入门面（读取照常成功——否则测的就不是「提交」，
/// 而是「读不到历史」）。
enum RewindFault {
    /// 提交一律失败（-32603 路径）。
    CommitFails,
    /// 提交时边界行已被并发删除：先经真实门面删掉它，再走真实提交。
    ///
    /// 真实存储的未知边界语义是「什么都不做 + [`RewindOutcome::BoundaryMissing`]」，
    /// 因此这条注入测得的是 F1：存储没动，ACP 也不许发布「已截断 + 已清零」的热态。
    BoundaryVanishesAtCommit(MessageId),
}

struct RewindFaultStore {
    inner: Arc<dyn SessionResources>,
    fault: RewindFault,
}

/// 只替换 `rewind_history` 一个方法，其余逐项转发真实门面（注入点唯一，故用宏而不是
/// 每个故障注入各写一遍 30 个转发方法）。
///
/// 宏体只做转发与委派，注入逻辑放在调用方的固有方法里（见下方的 `rewind_fault`）：宏体
/// 内的 `self` 与形参不参与调用方的名字解析（macro hygiene），注入体若写进宏参数就引用
/// 不到它们。
macro_rules! forward_session_resources_except_rewind {
    (
        $name:ident => $hook:ident
        $( $method:ident ( $( $argument:ident : $kind:ty ),* ) -> $output:ty; )*
    ) => {
        #[async_trait]
        impl SessionResources for $name {
            $(async fn $method(&self, $( $argument: $kind ),*) -> SessionResourceResult<$output> {
                self.inner.$method($( $argument ),*).await
            })*

            async fn rewind_history(
                &self,
                id: &ThreadId,
                boundary: RewindBoundary,
                clear_excluded: &[MessageId],
            ) -> SessionResourceResult<RewindOutcome> {
                self.$hook(id, boundary, clear_excluded).await
            }
        }
    };
}

impl RewindFaultStore {
    /// 注入点：按 [`RewindFault`] 给出这一次 durable 提交的应答。
    async fn rewind_fault(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
        clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        match self.fault {
            RewindFault::CommitFails => Err(SessionResourceError::conflict(
                "injected: durable rewind commit failed",
            )),
            // 并发删除：边界行在批次之前消失，真实门面按未知边界语义返回 BoundaryMissing。
            RewindFault::BoundaryVanishesAtCommit(boundary_id) => {
                self.inner
                    .remove_history_entries(id, &[boundary_id])
                    .await
                    .expect("injected concurrent deletion");
                self.inner
                    .rewind_history(id, boundary, clear_excluded)
                    .await
            }
        }
    }
}

forward_session_resources_except_rewind! {
    RewindFaultStore => rewind_fault
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
    load_session_history(id: &ThreadId) -> Vec<PersistedPayload>;
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
    remove_history_entries(id: &ThreadId, ids: &[MessageId]) -> ();
    update_session_meta(id: &ThreadId, patch: &SessionMetaPatch) -> ();
    delete_session_tree(id: &ThreadId) -> ();
    recover_session_persistence(id: &ThreadId) -> PersistenceRecovery;
    drain_persistence(id: &ThreadId) -> ();
}

/// 归档目标（只存在于存储）仍出现在候选里、可预览、可执行。
///
/// 收缩后的热态只持有有效集合：目标是否合法不得再以 Host 快照为判据。
#[tokio::test]
async fn test_rewind_reaches_target_archived_by_full_compact() {
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
    let (h1, _a1, _h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
    let target = h1.as_uuid().to_string();

    let candidates = handle_request(
        "session/rewind-candidates",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let ids: Vec<String> = candidates["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.contains(&target),
        "被 Full 归档的 user 消息必须仍是候选，实际: {ids:?}"
    );

    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap();

    let result = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert_eq!(result.unwrap()["status"], "executed");
    // 目标本身及之后的 own 历史（含归档正文）都在存储里消失，热态同步清空。
    let stored = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap();
    assert!(stored.is_empty(), "存储里不应再有 own 历史");
    let state = sessions.get(&sid).unwrap();
    assert!(state.history.is_empty(), "热态展示投影必须清空");
    assert!(state.history_payloads.is_empty(), "热态有效集合必须清空");
}

/// 回退到仍然有效的尾条目：保留前缀里被排除的原文重新生效（摘要被删，旧排除失去依据），
/// 热态与冷恢复看到同一份有效视图。
#[tokio::test]
async fn test_rewind_publishes_the_cold_recovery_view() {
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
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
    let target = h2.as_uuid().to_string();

    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // 冷恢复（load_active_snapshot 就是恢复路径读的那一份）：h2 已删，h1/a1 重新有效。
    let active = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap();
    let cold_ids: Vec<peri_acp_types::messages::MessageId> =
        active.payloads.iter().map(PersistedPayload::id).collect();
    assert_eq!(
        cold_ids,
        vec![h1, a1],
        "被删摘要的排除失去依据 → 保留前缀的原文必须重新有效"
    );

    let state = sessions.get(&sid).unwrap();
    let hot_ids: Vec<peri_acp_types::messages::MessageId> = state
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(hot_ids, cold_ids, "热态有效集合必须等于冷恢复视图");
    assert_eq!(
        state
            .history
            .iter()
            .map(peri_acp_types::messages::BaseMessage::id)
            .collect::<Vec<_>>(),
        vec![h1, a1]
    );
    assert!(!hot_ids.contains(&h2), "被移除的目标不得留在任何容器里");
}

/// 回归（S6c / 验收 A19）：rewind 之后**同一会话继续 prompt** 与**冷恢复**必须把同一份
/// 有效视图交给模型，且都不含已退出有效上下文的原文或被移除的摘要。
///
/// 断言贴在模型边界而不是数据边界：链路是 `SessionState::history_payloads`
/// →（`host/prompt.rs` 的 `TurnInput.history_payloads`）→ 执行体
/// `MessageTranscript::with_own_payloads`（`peri-agent` `v2_execute.rs` 用的同一个构造器）
/// → `visible_model_messages()`。热态若多带一份被删摘要、或漏掉重新有效的原文，
/// 这条断言就会红；它同时覆盖「延续同一会话」与「重启后冷恢复」两条路径。
#[tokio::test]
async fn test_rewind_then_prompt_matches_a_cold_restore_at_the_model_boundary() {
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
    let (h1, a1, summary) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);

    // 回退到摘要本身：摘要被移除 → 它覆盖的原文重新有效（REQ-T04 的边界语义）。
    let target = summary.as_uuid().to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();
    handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // 热态：同一会话继续（下一条 prompt 拿到的就是这一份）。
    let hot = sessions.get(&sid).unwrap().history_payloads.clone();
    // 冷恢复：重启后从存储读同一份有效视图。
    let cold = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert_eq!(
        hot.iter().map(PersistedPayload::id).collect::<Vec<_>>(),
        vec![h1, a1],
        "热态 = 重新有效的原文，且不含被移除的摘要"
    );

    let model_view = |payloads: Vec<PersistedPayload>| {
        peri_agent::session::MessageTranscript::new()
            .with_own_payloads(payloads)
            .visible_model_messages()
            .expect("模型视图必须可投影")
    };
    let hot_view = model_view(hot);
    let cold_view = model_view(cold);
    // `BaseMessage` 没有 `PartialEq`：按（身份，正文）逐条比较模型真正看到的东西。
    let projection = |view: &[peri_acp_types::messages::BaseMessage]| {
        view.iter()
            .map(|message| (message.id(), message.content()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        projection(&hot_view),
        projection(&cold_view),
        "热态与冷恢复必须给模型同一份历史（无复活正文、无重复摘要）"
    );
    assert_eq!(
        hot_view
            .iter()
            .map(|message| message.id())
            .collect::<Vec<_>>(),
        vec![h1, a1],
        "模型请求里的历史正是重新生效的那两条"
    );
    assert!(
        !hot_view.iter().any(|message| message.id() == summary),
        "被移除的摘要不得回到模型请求"
    );
}

/// 回归（S6c / 验收 A19「旧 cursor 失效」）：rewind 提交成功后，已发出的历史分页游标
/// **在进入存储之前**被拒（结构版本），且与「锚点被删」得到同一个可区分错误。
///
/// 本用例刻意让锚点在 rewind 后仍然存在（回退目标是一条更晚的消息），因此失败原因
/// 只能是结构版本；同时用「新游标可用」证明存储侧一切正常。
#[tokio::test]
async fn test_rewind_expires_previously_issued_history_cursors() {
    use crate::host::history_page::HistoryPageRequest;
    use peri_acp_types::history_page::HistoryCursorEnvelope;

    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let shared: crate::host::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let sid = {
        let mut sessions = shared.lock().await;
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await
    };
    let (_h1, _a1, anchor) = {
        let mut sessions = shared.lock().await;
        archive_prefix_in_storage(&mut sessions, &cfg, &sid).await
    };
    enable_rewind_cap(&cfg, &sid);
    // 更晚的一条自有消息：回退到它，锚点（摘要）在提交后仍然存活。
    let tail = peri_acp_types::messages::BaseMessage::human("第三轮用户问题");
    cfg.session_resources
        .append_history(&sid, &[PersistedPayload::Message(tail.clone())])
        .await
        .unwrap();

    let pagination = PeriCaps {
        history_pagination: true,
        ..PeriCaps::default()
    };
    let old_cursor = HistoryCursorEnvelope::new(
        0,
        "snapshot-before-rewind",
        crate::dispatch::initial_history_cursor(cfg.controller.as_ref(), &sid)
            .await
            .unwrap()
            .next_cursor
            .expect("归档区存在时必须给出更早页"),
    );
    assert_eq!(old_cursor.cursor.anchor, anchor, "首游标锚在有效区最旧一条");
    let request = HistoryPageRequest {
        session_id: sid.clone(),
        cursor: old_cursor.clone(),
        limit: 50,
    };
    let page = crate::host::history_page::handle_page(&cfg, &shared, &pagination, &request)
        .await
        .expect("rewind 之前该游标必须可用");
    assert!(
        !page["items"].as_array().unwrap().is_empty(),
        "归档区可翻页"
    );

    let target = tail.id().as_uuid().to_string();
    let preview = {
        let mut sessions = shared.lock().await;
        handle_request(
            "session/rewind-preview",
            &json!({ "sessionId": sid, "target_message_id": target }),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap()
    };
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();
    {
        let mut sessions = shared.lock().await;
        handle_request(
            "session/rewind",
            &json!({
                "sessionId": sid,
                "target_message_id": target,
                "preview_fingerprint": fingerprint,
            }),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
    }

    let cold = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert!(
        cold.iter().any(|payload| payload.id() == anchor),
        "锚点仍在冷恢复视图里：失败原因只能是结构版本"
    );

    let error = crate::host::history_page::handle_page(&cfg, &shared, &pagination, &request)
        .await
        .expect_err("rewind 之后旧游标必须失效");
    assert_eq!(error.code, -32012, "与「锚点被删」同一个可区分错误");

    // 新游标（同一份历史、新版本）继续可用：存储侧没有被这次拒绝影响。
    let version = shared.lock().await.get(&sid).unwrap().structure_version;
    assert_eq!(version, 1, "一次成功的 rewind 只推进一次结构版本");
    let fresh = crate::dispatch::initial_history_cursor(cfg.controller.as_ref(), &sid)
        .await
        .unwrap()
        .next_cursor
        .expect("摘要仍活着，更早方向仍有页");
    let fresh = HistoryCursorEnvelope::new(version, "snapshot-after-rewind", fresh);
    crate::host::history_page::handle_page(
        &cfg,
        &shared,
        &pagination,
        &HistoryPageRequest {
            session_id: sid.clone(),
            cursor: fresh,
            limit: 50,
        },
    )
    .await
    .expect("rewind 之后的新游标必须照常可读");
}

/// 保留前缀里仍成立的排除必须原样保留：摘要活着时，被它覆盖的正文不因 rewind 复活。

///
/// 与上一个用例互为反面——清零集合为空的分支，热态与 wire 都只能是有效视图
/// （目标之后的原文被移除，但目标之前的归档区不受影响）。
#[tokio::test]
async fn test_rewind_keeps_still_covered_bodies_archived() {
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
    // 追加一轮：目标落在摘要（history[2]）之后，回退不会删掉摘要本身。
    let late = peri_acp_types::messages::BaseMessage::ai("late answer");
    cfg.session_resources
        .append_history(
            &peri_acp_types::thread::ThreadId::from(sid.clone()),
            &[PersistedPayload::Message(late.clone())],
        )
        .await
        .unwrap();
    sessions.get_mut(&sid).unwrap().history.push(late.clone());
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
    let target = late.id().as_uuid().to_string();

    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    let response = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let stored: Vec<peri_acp_types::messages::MessageId> = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(stored, vec![h1, a1, h2], "摘要仍在 → 归档区原样保留");

    let active = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap();
    let cold_ids: Vec<peri_acp_types::messages::MessageId> =
        active.payloads.iter().map(PersistedPayload::id).collect();
    assert_eq!(cold_ids, vec![h2], "被覆盖的正文不得复活");

    let state = sessions.get(&sid).unwrap();
    assert_eq!(
        state
            .history_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        cold_ids
    );
    assert_eq!(
        response["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![h2.as_uuid().to_string()],
        "wire 里的 history 必须是有效视图，不是含归档正文的完整前缀"
    );
}

/// F1 回归：提交时边界行已不在存储里（并发删除）⇒ 存储一行未动，热态与结构版本都不得前进。
///
/// 旧行为是「存储返回 Ok(什么都没做)，ACP 照样发布已截断 + 已清零的热态」——冷恢复与
/// 运行中的上下文就此分叉。现在必须报可区分错误（`-32013` + `data.reason`）、不发
/// `RewindCompleted`（TUI 会以它为事实源重建 transcript）。
#[tokio::test]
async fn test_rewind_missing_boundary_changes_nothing_and_reports_it() {
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
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_and_event_caps(&cfg, &sid);
    let before_history = history_ids(&sessions, &sid);
    let before_payloads = payload_ids(&sessions, &sid);
    let target = h2.as_uuid().to_string();

    // 注入：提交那一刻边界行已被并发删除（真实门面的未知边界语义 = 无变更 + BoundaryMissing）。
    cfg.controller = Arc::new(peri_controller::Controller::new(Arc::new(
        RewindFaultStore {
            inner: Arc::clone(&cfg.session_resources),
            fault: RewindFault::BoundaryVanishesAtCommit(h2),
        },
    )));

    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.code, -32013,
        "未生效的提交必须与普通失败可区分: {error:?}"
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str),
        Some("rewind_boundary_missing"),
        "分类必须稳定: {error:?}"
    );
    assert!(
        error.message.contains("存储未改动"),
        "报文要说清副作用: {error:?}"
    );
    assert_eq!(
        history_ids(&sessions, &sid),
        before_history,
        "热态展示投影不得改变"
    );
    assert_eq!(
        payload_ids(&sessions, &sid),
        before_payloads,
        "热态有效集合不得改变"
    );
    assert_eq!(
        sessions.get(&sid).unwrap().structure_version,
        0,
        "未生效的提交不得推进结构版本（内存不得领先于已提交事实）"
    );
    assert!(
        agent_events(&mock, "rewind_completed").is_empty(),
        "提交没生效就不得发重建信号: {:?}",
        mock.notifications()
    );
    // 存储：除并发删掉的那一行，一行未动——尤其是保留前缀的 `excluded` 没有被清零
    //（清零与删除同批，批不生效就一条都不能落）。
    let snapshot = cfg
        .controller
        .sessions()
        .load_session_snapshot(&ThreadId::from(sid.clone()))
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![h1, a1]
    );
    for id in [h1, a1] {
        assert!(
            snapshot.flags.get(&id).is_some_and(|flags| flags.excluded),
            "保留前缀的排除不得被清零: {id:?}"
        );
    }
}

/// F4 回归：`RewindCompleted` 的载荷必须是**有效视图**（与热态、wire `history` 同源），
/// 不是含「仍被存活摘要覆盖的正文」的 canonical 前缀。
///
/// TUI 以该事件为事实源重建 transcript 与候选弹窗；载荷退回 canonical 前缀时，用户会看到
/// 模型与存储都不认为有效的条目（这正是 F4 逃逸的路径）。
#[tokio::test]
async fn test_rewind_completed_event_carries_the_effective_view() {
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
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    // 目标落在摘要之后：摘要活着 → canonical 前缀含被覆盖的正文，有效视图不含。
    let late = peri_acp_types::messages::BaseMessage::ai("late answer");
    cfg.session_resources
        .append_history(
            &peri_acp_types::thread::ThreadId::from(sid.clone()),
            &[PersistedPayload::Message(late.clone())],
        )
        .await
        .unwrap();
    sessions.get_mut(&sid).unwrap().history.push(late.clone());
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_and_event_caps(&cfg, &sid);
    let target = late.id().as_uuid().to_string();

    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    let response = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let events = agent_events(&mock, "rewind_completed");
    assert_eq!(events.len(), 1, "应恰好发一条重建信号: {events:?}");
    let messages_json = events[0]
        .get("messages_json")
        .and_then(Value::as_str)
        .expect("messages_json 缺失");
    let messages: Vec<peri_acp_types::messages::BaseMessage> =
        serde_json::from_str(messages_json).unwrap();
    let event_ids: Vec<MessageId> = messages
        .iter()
        .map(peri_acp_types::messages::BaseMessage::id)
        .collect();
    assert_eq!(
        event_ids,
        vec![h2],
        "事件载荷必须是有效视图：仍被存活摘要覆盖的正文（{h1:?}/{a1:?}）不得出现"
    );
    // 三处同源：事件载荷 = 热态有效集合 = wire `history`。
    assert_eq!(payload_ids(&sessions, &sid), vec![h2]);
    assert_eq!(
        response["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![h2.as_uuid().to_string()]
    );
}

/// durable 提交失败：内存保持提交前状态，错误按原样上抛（-32603）。
#[tokio::test]
async fn test_rewind_commit_failure_leaves_memory_untouched() {
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
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
    let before = history_ids(&sessions, &sid);
    let before_payloads: Vec<peri_acp_types::messages::MessageId> = sessions
        .get(&sid)
        .unwrap()
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();

    // 注入：提交失败（读取照常成功）。
    cfg.controller = Arc::new(peri_controller::Controller::new(Arc::new(
        RewindFaultStore {
            inner: Arc::clone(&cfg.session_resources),
            fault: RewindFault::CommitFails,
        },
    )));
    let target = h2.as_uuid().to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, -32603);
    assert!(
        error.message.contains("durable rewind commit failed"),
        "原始错误原因必须保留: {error:?}"
    );
    assert_eq!(history_ids(&sessions, &sid), before, "热态展示投影不得改变");
    let after_payloads: Vec<peri_acp_types::messages::MessageId> = sessions
        .get(&sid)
        .unwrap()
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(after_payloads, before_payloads, "热态有效集合不得改变");
    assert_eq!(
        sessions.get(&sid).unwrap().structure_version,
        0,
        "失败的提交不得推进结构版本（内存不得领先于已提交事实）"
    );
    // 存储同样是提交前状态：完整历史仍是三条，排除标记仍在。
    let stored: Vec<peri_acp_types::messages::MessageId> = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(stored, vec![h1, a1, h2]);
}

/// 边界语义未变：ACP 用户 rewind 用 RemoveFrom（目标本身也被移除），transcript 的
/// KeepThrough 保留目标——两者仍由同一份边界规则区分。
#[tokio::test]
async fn test_rewind_remove_from_drops_the_target_itself() {
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
    enable_rewind_cap(&cfg, &sid);
    let (h1, a1, h2) = {
        let s = sessions.get(&sid).unwrap();
        (s.history[0].id(), s.history[1].id(), s.history[2].id())
    };
    let ids = vec![h1, a1, h2];
    assert_eq!(
        peri_acp_types::store::history::rewind_keep_len(
            &ids,
            peri_acp_types::session_resources::RewindBoundary::RemoveFrom(h2),
        )
        .unwrap(),
        2,
        "RemoveFrom 移除目标本身"
    );
    assert_eq!(
        peri_acp_types::store::history::rewind_keep_len(
            &ids,
            peri_acp_types::session_resources::RewindBoundary::KeepThrough(h2),
        )
        .unwrap(),
        3,
        "KeepThrough 保留目标本身"
    );

    let target = h2.as_uuid().to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();
    handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let stored: Vec<peri_acp_types::messages::MessageId> = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(stored, vec![h1, a1], "目标本身必须被移除");
    assert_eq!(
        history_ids(&sessions, &sid),
        vec![h1.as_uuid().to_string(), a1.as_uuid().to_string()]
    );
}

#[tokio::test]
async fn test_rewind_methods_require_explicit_capability() {
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
    cfg.session_manager
        .caps_registry()
        .insert(sid.clone(), PeriCaps::default());
    let target = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();
    let original: Vec<_> = sessions
        .get(&sid)
        .unwrap()
        .history
        .iter()
        .map(|message| (message.id().as_uuid().to_string(), message.content()))
        .collect();

    for (method, params) in [
        ("session/rewind-candidates", json!({"sessionId": sid})),
        (
            "session/rewind-preview",
            json!({"sessionId": sid, "target_message_id": target}),
        ),
        (
            "session/rewind",
            json!({"sessionId": sid, "target_message_id": target}),
        ),
    ] {
        let error = handle_request(method, &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32601);
        assert_eq!(error.message, "peri.rewind capability not negotiated");
    }
    let current: Vec<_> = sessions
        .get(&sid)
        .unwrap()
        .history
        .iter()
        .map(|message| (message.id().as_uuid().to_string(), message.content()))
        .collect();
    assert_eq!(current, original);
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
    let target_id = sessions.get(&sid).unwrap().history[2]
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
    let target_id = sessions.get(&sid).unwrap().history[0]
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

    // P1：rewind 后 SessionState.history 必须截断——它是后续候选/预算查询的
    // 数据源，不写回会导致第二次回退 not found。
    let s = sessions.get(&sid).unwrap();
    assert_eq!(s.history.len(), 0, "回退到第一条后 history 应为空");
    assert_eq!(
        s.history_payloads.len(),
        0,
        "rewind 必须同步裁剪 canonical history_payloads"
    );
}

#[tokio::test]
async fn test_rewind_rejects_stale_preview_without_mutating_history() {
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
    let target_id = sessions.get(&sid).unwrap().history[0]
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
    let preview_fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    // 事实源是存储：新消息落进存储后，旧预览的 fingerprint 不再成立。
    cfg.session_resources
        .append_history(
            &peri_acp_types::thread::ThreadId::from(sid.clone()),
            &[PersistedPayload::Message(
                peri_acp_types::messages::BaseMessage::ai("late answer"),
            )],
        )
        .await
        .unwrap();
    let before = sessions.get(&sid).unwrap().history.len();
    let stored_before = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .len();
    let error = handle_request(
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
    .await
    .unwrap_err();

    assert!(error.message.contains("preview is stale"));
    assert_eq!(sessions.get(&sid).unwrap().history.len(), before);
    assert_eq!(
        cfg.controller
            .sessions()
            .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
            .await
            .unwrap()
            .len(),
        stored_before,
        "被拒的过期执行不得改动存储"
    );
}

// ── 继承区在请求级 rewind 里的口径 ─────────────────────────────────────────────

/// 带继承区的会话夹具：parent 的两条历史作为 child 的继承区，child 自有区按 Full 之后的
/// 形状排布（排除段 + **仍然存活的摘要** + 回退目标）。
///
/// 继承区条目不属于本会话，永不参与排除。夹具刻意让继承区以未排除的非 System 条目开头：
/// 清零依据若误用完整逻辑上下文，规则会在这里认出「位置 0 上的存活摘要」，把自有区里仍被
/// 存活摘要覆盖的原文一并清零——用户会看到模型与存储都不认为有效的重复覆盖。
struct InheritedRewindFixture {
    sid: String,
    inherited: Vec<MessageId>,
    own: Vec<MessageId>,
}

async fn register_child_with_inherited_history(
    sessions: &mut HashMap<String, SessionState>,
    cfg: &AcpServerConfig,
    cwd: &str,
) -> InheritedRewindFixture {
    use peri_acp_types::messages::BaseMessage;

    let workspace = cfg
        .session_resources
        .resolve_workspace(std::path::Path::new(cwd))
        .await
        .unwrap();
    let frozen = cfg
        .session_manager
        .build_frozen_data(workspace.cwd.to_str().unwrap());
    let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();

    // parent：child 的 frozen 必须逐字节等于 root 已保存快照，因此复用同一份编码结果。
    let parent = "rewind-inherited-parent".to_string();
    cfg.session_resources
        .create_session(&bound_input(&parent, &workspace, encoded.clone()))
        .await
        .unwrap();
    let inherited_payloads = vec![
        PersistedPayload::Message(BaseMessage::human("继承区问题")),
        PersistedPayload::Message(BaseMessage::ai("继承区回答")),
    ];
    cfg.session_resources
        .append_history(&parent, &inherited_payloads)
        .await
        .unwrap();

    // child：继承 parent 到快照点为止的上下文，自有历史另算。
    let sid = "rewind-inherited-child".to_string();
    let mut target = bound_input(&sid, &workspace, encoded);
    target.meta.parent_thread_id = Some(parent.clone());
    target.meta.snapshot_at_message_id = Some(inherited_payloads[1].id());
    cfg.session_resources
        .save_child(&ChildSnapshot {
            target,
            parent_id: parent.clone(),
            root_id: parent,
            inherited: InheritedContext {
                payloads: inherited_payloads.clone(),
                flags: HashMap::new(),
            },
        })
        .await
        .unwrap();

    let own_payloads = vec![
        PersistedPayload::Message(BaseMessage::human("自有原文一")),
        PersistedPayload::Message(BaseMessage::ai("自有原文二")),
        PersistedPayload::Message(BaseMessage::human("存活摘要")),
        PersistedPayload::Message(BaseMessage::ai("回退目标")),
    ];
    cfg.session_resources
        .append_history(&sid, &own_payloads)
        .await
        .unwrap();
    let excluded = MessageFlags {
        excluded: true,
        ..Default::default()
    };
    cfg.session_resources
        .apply_message_projections(
            &ThreadId::from(sid.clone()),
            &[
                (own_payloads[0].id(), excluded.clone()),
                (own_payloads[1].id(), excluded),
            ],
        )
        .await
        .unwrap();

    // 热态：与冷恢复同形的有效视图（排除段的正文不在其中）。
    let effective: Vec<PersistedPayload> = inherited_payloads
        .iter()
        .cloned()
        .chain([own_payloads[2].clone(), own_payloads[3].clone()])
        .collect();
    let history: Vec<BaseMessage> = effective
        .iter()
        .filter_map(|payload| payload.as_message().cloned())
        .collect();
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: sid.clone(),
            cwd: workspace.cwd.to_str().unwrap().to_owned(),
            environment: None,
            closing: false,
            history,
            history_payloads: effective,
            structure_version: 0,
            cancel_token: None,
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_notifications: None,
            title: None,
            tags: Vec::new(),
        },
    );
    InheritedRewindFixture {
        sid,
        inherited: inherited_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect(),
        own: own_payloads.iter().map(PersistedPayload::id).collect(),
    }
}

/// 请求级：会话带非空继承区时，回退目标必须在自有区，清零依据只取自有区。
///
/// 存储的边界提交按 id 找目标，继承条目根本不在本会话的行集合里——在那里放行会留下
/// 「内存已截断、存储没动」的分叉，因此必须先拒绝。
#[tokio::test]
async fn test_rewind_with_an_inherited_region_only_clears_own_archived_prefix() {
    use peri_acp_types::messages::BaseMessage;

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
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let fixture =
        register_child_with_inherited_history(&mut sessions, &cfg, tmp.path().to_str().unwrap())
            .await;
    enable_rewind_and_event_caps(&cfg, &fixture.sid);
    let before_history = history_ids(&sessions, &fixture.sid);
    let before_payloads = payload_ids(&sessions, &fixture.sid);

    // 继承条目不是本会话的回退目标：存储按 id 在**本会话的行集合**里找边界，放行会留下
    // 「内存已截断、存储没动」的分叉，因此执行必须先拒绝，且热态一行不动。
    let inherited_target = fixture.inherited[0].as_uuid().to_string();
    let inherited_preview = handle_request(
        "session/rewind-preview",
        &json!({
            "sessionId": fixture.sid,
            "target_message_id": inherited_target,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": fixture.sid,
            "target_message_id": inherited_target,
            "preview_fingerprint": inherited_preview["preview_fingerprint"],
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602, "继承条目不得当作回退目标: {error:?}");
    assert_eq!(history_ids(&sessions, &fixture.sid), before_history);
    assert_eq!(payload_ids(&sessions, &fixture.sid), before_payloads);

    let preview = handle_request(
        "session/rewind-preview",
        &json!({
            "sessionId": fixture.sid,
            "target_message_id": fixture.own[3].as_uuid().to_string(),
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    let response = handle_request(
        "session/rewind",
        &json!({
            "sessionId": fixture.sid,
            "target_message_id": fixture.own[3].as_uuid().to_string(),
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // 有效视图：继承区 + 存活摘要。被摘要覆盖的原文不得复活（它们仍在归档区），目标被移除。
    let expected = vec![fixture.inherited[0], fixture.inherited[1], fixture.own[2]];
    assert_eq!(
        payload_ids(&sessions, &fixture.sid),
        expected,
        "热态必须是「继承区 + 存活摘要」"
    );
    assert_eq!(
        response["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|id| id.as_uuid().to_string())
            .collect::<Vec<_>>(),
        "wire 与热态同源"
    );
    let events = agent_events(&mock, "rewind_completed");
    assert_eq!(events.len(), 1);
    let event_messages: Vec<BaseMessage> =
        serde_json::from_str(events[0]["messages_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        event_messages
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        expected,
        "事件载荷 = 发布的有效视图（含继承区、不含被覆盖的正文）"
    );

    // 存储：排除段仍在（摘要存活 → 清零依据不成立），目标及其之后的行已删除。
    let snapshot = cfg
        .controller
        .sessions()
        .load_session_snapshot(&ThreadId::from(fixture.sid.clone()))
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![fixture.own[0], fixture.own[1], fixture.own[2]],
        "自有区只保留到回退目标之前"
    );
    for id in [fixture.own[0], fixture.own[1]] {
        assert!(
            snapshot.flags.get(&id).is_some_and(|flags| flags.excluded),
            "存活摘要覆盖的原文不得被清零: {id:?}"
        );
    }
    assert_eq!(
        snapshot
            .inherited
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        fixture.inherited,
        "继承区不因本会话的 rewind 改变"
    );
}

// ── session/cancel-bg-task 路由测试（issue 2026-08-05）───────────────────
