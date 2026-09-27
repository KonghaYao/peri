//! 门面契约的不变量测试：三个独立事实、效果确定性、定向更新语义。

use super::*;
use crate::thread::ThreadId;

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

    // 数据能力 Complete 不蕴含可执行：上次执行没有干净收尾时必须先按代际恢复。
    let writable_but_dirty = SessionAvailability {
        access: AccessMode::ReadWrite,
        capabilities: DataCapabilities::Complete,
        execution: Some(ExecutionAvailability::Dirty(RecoveryRequiredDetails {
            thread_id: thread_id(),
            generation: 3,
        })),
    };
    assert_ne!(
        writable_but_dirty.execution,
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
    let error: SessionResourceError = WorkspaceError::RecoveryRequired(RecoveryRequiredDetails {
        thread_id: thread_id(),
        generation: 7,
    })
    .into();

    assert_eq!(error.effect(), MutationOutcome::NotApplied);
    assert!(matches!(
        error.workspace_error(),
        Some(WorkspaceError::RecoveryRequired(details)) if details.generation == 7
    ));
    assert!(std::error::Error::source(&error).is_some());
    assert!(error.to_string().contains("recovery is required"));
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

#[test]
fn read_only_admission_keeps_degradation_and_blocking_separate() {
    use crate::workspace::{ReadOnlyAdmission, RecoveryRequiredDetails, WorkspaceError};

    // 只读存储：历史可按只读会话进入，「本节点给不出执行所有权」与既有原因同类。
    let read_only = SessionResourceError::new(SessionResourceErrorKind::ReadOnlyStore);
    assert_eq!(
        read_only.read_only_admission(),
        Some(ReadOnlyAdmission::ExecutionLeaseRequired)
    );

    // 本机 workspace 原因原样保留。
    let busy = SessionResourceError::from(WorkspaceError::ExecutionBusy);
    assert_eq!(
        busy.read_only_admission(),
        Some(ReadOnlyAdmission::ExecutionBusy)
    );
    let dirty_details = RecoveryRequiredDetails {
        thread_id: thread_id(),
        generation: 7,
    };
    let dirty = SessionResourceError::from(WorkspaceError::RecoveryRequired(dirty_details.clone()));
    assert_eq!(
        dirty.read_only_admission(),
        Some(ReadOnlyAdmission::RecoveryRequired(dirty_details))
    );

    // 连会话都没有的登记失败不降级。
    let registration = SessionResourceError::from(WorkspaceError::ReadOnlyStore);
    assert_eq!(registration.read_only_admission(), None);
    assert!(registration.workspace_error().is_some());

    // 未证明终态的写入不进入只读降级，也不能被自动 reset 分支吞掉。
    let uncertain = SessionResourceError::persistence_uncertain(Some(thread_id()));
    assert_eq!(uncertain.read_only_admission(), None);
    assert!(uncertain.is_persistence_uncertain());
}
