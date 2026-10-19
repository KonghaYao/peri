//! 门面契约的不变量测试：三个独立事实、效果确定性、定向更新语义。

use super::*;
use crate::thread::ThreadId;
use crate::workspace::WorkspaceError;

fn thread_id() -> ThreadId {
    ThreadId::from("0197f3f0-0000-7000-8000-000000000001".to_string())
}

#[test]
fn access_mode_capabilities_and_execution_are_independent_facts() {
    // 只读授权不蕴含「只能读历史」：数据能力仍可为 Complete，只是本次不允许写。
    let read_only_remote = SessionAvailability {
        access: AccessMode::ReadOnly,
        capabilities: DataCapabilities::Complete,
        execution: Some(ExecutionAvailability::ReadOnlyStore),
    };
    assert_eq!(read_only_remote.access, AccessMode::ReadOnly);
    assert_eq!(read_only_remote.capabilities, DataCapabilities::Complete);
    assert_eq!(
        read_only_remote.execution,
        Some(ExecutionAvailability::ReadOnlyStore)
    );

    // 数据能力 Complete 不蕴含可执行：未决持久化必须先收敛。
    let writable_but_pending = SessionAvailability {
        access: AccessMode::ReadWrite,
        capabilities: DataCapabilities::Complete,
        execution: Some(ExecutionAvailability::PersistencePending),
    };
    assert_ne!(
        writable_but_pending.execution,
        Some(ExecutionAvailability::Available)
    );

    // 历史只读后端仍可声明读权限；未指定会话时不编造执行事实。
    let history_read_only = SessionAvailability {
        access: AccessMode::ReadWrite,
        capabilities: DataCapabilities::HistoryReadOnly,
        execution: None,
    };
    assert_eq!(history_read_only.execution, None);
}

#[test]
fn persistence_uncertain_is_the_only_unknown_effect() {
    let id = thread_id();
    let uncertain = SessionResourceError::persistence_uncertain(Some(id.clone()));
    assert_eq!(uncertain.effect(), MutationOutcome::Unknown);
    assert!(uncertain.is_persistence_uncertain());
    assert!(matches!(
        uncertain.kind(),
        SessionResourceErrorKind::PersistenceUncertain { thread_id: Some(inner) } if *inner == id
    ));

    // 已保存但准入失败：数据确实生效，不能报告成「确定未创建」。
    let not_admitted = SessionResourceError::saved_but_not_admitted(id.clone());
    assert_eq!(not_admitted.effect(), MutationOutcome::Applied);

    // 其余原因默认「已确认未生效」：不从 Timeout/Unavailable 推导别的含义。
    for kind in [
        SessionResourceErrorKind::NotFound,
        SessionResourceErrorKind::Unsupported,
        SessionResourceErrorKind::ReadOnlyStore,
        SessionResourceErrorKind::Timeout,
        SessionResourceErrorKind::Unavailable {
            detail: "network".into(),
        },
        SessionResourceErrorKind::Corrupt {
            detail: "bad row".into(),
        },
    ] {
        assert_eq!(
            SessionResourceError::new(kind).effect(),
            MutationOutcome::NotApplied
        );
    }
}

#[test]
fn error_display_keeps_effect_but_leaks_no_identity() {
    let id = thread_id();
    let uncertain = SessionResourceError::persistence_uncertain(Some(id.clone()));
    let rendered = uncertain.to_string();
    assert!(rendered.contains("reload"), "{rendered}");
    assert!(
        !rendered.contains(id.as_str()),
        "错误文本不携带会话标识：{rendered}"
    );

    let not_admitted = SessionResourceError::saved_but_not_admitted(id.clone());
    assert!(!not_admitted.to_string().contains(id.as_str()));
}

#[test]
fn workspace_failure_keeps_local_semantics_and_source() {
    let error: SessionResourceError = WorkspaceError::NeedsRelink.into();

    assert_eq!(error.effect(), MutationOutcome::NotApplied);
    assert!(matches!(
        error.workspace_error(),
        Some(WorkspaceError::NeedsRelink)
    ));
    assert!(std::error::Error::source(&error).is_some());
    assert!(error.to_string().contains("session directory changed"));
}

#[test]
fn meta_patch_distinguishes_untouched_from_cleared() {
    let untouched = SessionMetaPatch::default();
    let cleared = SessionMetaPatch {
        title: Some(None),
        ..Default::default()
    };

    assert!(untouched.title.is_none(), "None 表示不更新");
    assert_eq!(cleared.title, Some(None), "Some(None) 表示清除");

    let set = SessionMetaPatch {
        title: Some(Some("new title".into())),
        status: Some(AgentStatus::Done),
        cancel_policy: Some(CancelPolicy::Independent),
        config: Some(None),
    };
    assert_eq!(set.title, Some(Some("new title".into())));
    assert_eq!(set.status, Some(AgentStatus::Done));
    assert_eq!(set.cancel_policy, Some(CancelPolicy::Independent));
    assert_eq!(set.config, Some(None));
}

#[test]
fn frozen_snapshot_bytes_stay_opaque() {
    let envelope = r#"{"version":1,"data":{"entries":[]}}"#;

    let bytes = FrozenSnapshotBytes::new(envelope);

    assert_eq!(bytes.as_str(), envelope, "存储不得重编码 envelope");
    assert_eq!(bytes.clone().into_string(), envelope);
}

#[test]
fn rewind_boundary_reports_its_target() {
    let id = MessageId::new();
    assert_eq!(RewindBoundary::KeepThrough(id).message_id(), id);
    assert_eq!(RewindBoundary::RemoveFrom(id).message_id(), id);
}
/// 没有任何一条段读取实现的后端：值是空的，只用来验证 trait 的默认实现。
struct SegmentlessResources;

#[async_trait]
impl SessionResources for SegmentlessResources {
    async fn inspect_availability(
        &self,
        _session: Option<&ThreadId>,
    ) -> SessionResourceResult<SessionAvailability> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn resolve_workspace(&self, _cwd: &Path) -> SessionResourceResult<ResolvedWorkspace> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn validate_session(
        &self,
        _id: &ThreadId,
        _workspace: &ResolvedWorkspace,
    ) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn finish_close(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn close_settlement(&self, _id: &ThreadId) -> SessionResourceResult<CloseSettlement> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn create_session(&self, _input: &NewSession) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn abandon_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn begin_initialization(
        &self,
        _draft: &NewSessionDraft,
    ) -> SessionResourceResult<Arc<dyn SessionInitialization>> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn discard_incomplete_initialization(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn adopt_legacy_session(
        &self,
        _id: &ThreadId,
        _saved_cwd: &str,
        _workspace: &ResolvedWorkspace,
        _frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn load_session_snapshot(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<SessionSnapshot> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn load_session_binding(&self, _id: &ThreadId) -> SessionResourceResult<BindingState> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn validate_bound_workspace(
        &self,
        _id: &ThreadId,
        _check: BindingRecheck,
    ) -> SessionResourceResult<ResolvedWorkspace> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn load_session_history(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn load_session_meta(&self, _id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn list_sessions(
        &self,
        _query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn list_children(&self, _parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn list_session_tree(&self, _root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn append_history(
        &self,
        _id: &ThreadId,
        _payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn append_reminder_if_absent(
        &self,
        _id: &ThreadId,
        _message_id: MessageId,
        _reminder: &TrustedSystemReminder,
    ) -> SessionResourceResult<bool> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn mark_session_closing(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn is_session_closing(&self, _id: &ThreadId) -> SessionResourceResult<bool> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn save_fork(&self, _fork: &ForkSnapshot) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn save_child(&self, _child: &ChildSnapshot) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn claim_child_resume(
        &self,
        _child: &ThreadId,
        _root: &ThreadId,
    ) -> SessionResourceResult<Box<dyn ChildResumeClaim>> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn apply_compaction(
        &self,
        _id: &ThreadId,
        _change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn apply_message_projections(
        &self,
        _id: &ThreadId,
        _updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn rewind_history(
        &self,
        _id: &ThreadId,
        _boundary: RewindBoundary,
        _clear_excluded: &[MessageId],
    ) -> SessionResourceResult<RewindOutcome> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn remove_history_entries(
        &self,
        _id: &ThreadId,
        _ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn update_session_meta(
        &self,
        _id: &ThreadId,
        _patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn delete_session_tree(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn recover_session_persistence(
        &self,
        _id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        unimplemented!("a backend without this read must not reach it")
    }
    async fn drain_persistence(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        unimplemented!("a backend without this read must not reach it")
    }
}

/// 没有实现这条读取的后端必须**显式失败**（trait 默认实现），不得用一个空段冒充「回放结束」。
///
/// 首段与续段两条入口都要如此：调用方据此看见「这次回放读不完」，再决定回退到无界读取
/// （`load_session_history`）还是让本次读取失败。空段在契约里是合法的「已到上界」，因此
/// 默认实现返回 `Ok(空段)` 会把一次读不了历史的后端静默伪装成「这个会话没有历史」。
#[tokio::test]
async fn load_history_segment_defaults_to_unsupported() {
    let resources = SegmentlessResources;
    let first = resources
        .load_history_segment(&thread_id(), None, 16, 64 * 1024)
        .await
        .expect_err("a backend without this read must not answer with a segment");
    assert!(matches!(
        first.kind(),
        SessionResourceErrorKind::Unsupported
    ));

    let cursor = HistoryForwardCursor::new("1", "9");
    let resumed = resources
        .load_history_segment(&thread_id(), Some(&cursor), 16, 64 * 1024)
        .await
        .expect_err("a continuation must fail the same way as the first segment");
    assert!(matches!(
        resumed.kind(),
        SessionResourceErrorKind::Unsupported
    ));
}

/// 没有实现窗口读取的后端必须显式失败：空边界/空页/空块都是合法结果，不能用它们
/// 冒充「这个会话没有历史」或「已经读完」。
#[tokio::test]
async fn history_window_reads_default_to_unsupported() {
    use crate::history_window::{
        HistoryCommittedTail, HistoryReadTicket, HistorySource, HistoryWindowDirection,
    };

    let resources = SegmentlessResources;
    let id = thread_id();
    let plan = HistoryWindowPagePlan::open(
        HistorySource::Own,
        None,
        HistoryWindowDirection::Around,
        8,
        8,
        1024,
    );
    let tail = HistoryCommittedTail::new(&id, MessageId::new(), 1);
    let inherited_plan = HistoryWindowPagePlan {
        intent: HistoryWindowReadIntent::Open,
        source: HistorySource::inherited("inherited:default"),
        anchor: None,
        direction: HistoryWindowDirection::Before,
        include_anchor: false,
        before_limit: 8,
        after_limit: 0,
        byte_budget: 1024,
        tail: HistoryWindowTailExpectation::Fixed(None),
    };

    for error in [
        resources.history_window_bounds(&id).await.unwrap_err(),
        resources
            .load_history_window_page(&id, &plan)
            .await
            .unwrap_err(),
        resources
            .load_history_window_page(&id, &inherited_plan)
            .await
            .unwrap_err(),
        resources
            .seek_history_window(&id, HistorySource::Own, MessageId::new(), 8, 8, 1024, &tail)
            .await
            .unwrap_err(),
        resources
            .load_history_body_chunk(
                &id,
                HistorySource::Own,
                MessageId::new(),
                0,
                16,
                Some(&tail),
                &HistoryReadTicket::new("ticket"),
            )
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(
            error.kind(),
            SessionResourceErrorKind::Unsupported
        ));
    }
}
