use super::*;
use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, CloseSettlement, ForkSnapshot,
    FrozenSnapshotBytes, FrozenState, NewSession, NewSessionDraft, PersistenceRecovery,
    RewindBoundary, RewindOutcome, SessionAvailability, SessionInitialization, SessionMetaPatch,
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
        (s.history_payloads[0].id(), s.history_payloads[1].id())
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
    let summary = state.history_payloads[2].clone();
    state.history_payloads = vec![summary];
    (h1, a1, state.history_payloads[0].id())
}

fn history_ids(sessions: &HashMap<String, SessionState>, sid: &str) -> Vec<String> {
    sessions
        .get(sid)
        .unwrap()
        .history_payloads
        .iter()
        .map(|payload| payload.id().as_uuid().to_string())
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
    load_frozen_state(id: &ThreadId) -> FrozenState;
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

// ── 子模块：本文件按职责拆分出的用例模块（正文逐行原样搬移，语义零改动）────────
//
// 共享夹具（`archive_prefix_in_storage` / `RewindFaultStore` / `history_ids` 等）留在本文件，
// 子模块经 `use super::*` 复用；拆分依据与对应关系见 `.recon/S13-size-acp-tests.md`。

#[path = "requests_rewind_effective_view_test.rs"]
mod effective_view_tests;

#[path = "requests_rewind_cursor_expiry_test.rs"]
mod cursor_expiry_tests;

#[path = "requests_rewind_failure_paths_test.rs"]
mod failure_paths_tests;

#[path = "requests_rewind_routing_test.rs"]
mod routing_tests;

#[path = "requests_rewind_inherited_region_test.rs"]
mod inherited_region_tests;

// ── session/cancel-bg-task 路由测试（issue 2026-08-05）───────────────────
