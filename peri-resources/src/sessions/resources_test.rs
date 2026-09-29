//! 会话资源门面行为测试（本机 SQLite）。
//!
//! 断言以可观察结果为准：写入准入是否统一、效果结清是否只认确定性、创建/撤销/认领的
//! 后置条件、删除与未决证据在级联之后是否仍可判定。构造「数据已保存、执行代际未写」
//! 这类崩溃态时直接经数据端口写入——那正是远程保存或上次进程留下的状态。

use super::*;
use crate::sessions::local_port::SessionFacts;
use crate::sessions::sqlite_store::{commit_failure, write_failure};
use crate::SessionStoreShutdownOwner;
use peri_acp_types::session_resources::{
    FrozenState, NewSessionDraft, NewSessionMeta, SessionInitialization, SessionResourceResult,
    SessionResources, SessionStoreShutdownPort,
};
use peri_acp_types::workspace::{ResetDirtyRequest, SESSION_BINDING_VERSION};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "-q"]);
    git(
        directory.path(),
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    );
    directory
}

struct Fixture {
    /// 具体实例：`Arc` 让业务侧与部署 owner 指向同一份事实（生产装配同形）。
    facade: Arc<SessionResourcesImpl>,
    repo: TempDir,
    _db: TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let repo = repository();
        let db = tempfile::tempdir().unwrap();
        let facade = Arc::new(
            SessionResourcesImpl::open(db.path().join("threads.db"))
                .await
                .unwrap(),
        );
        Self {
            facade,
            repo,
            _db: db,
        }
    }

    /// 部署 owner 的关闭路径：装配点交出的唯一关闭权（门面自身不再对业务暴露关闭）。
    async fn shutdown(&self) -> SessionResourceResult<()> {
        SessionStoreShutdownOwner::take(Arc::clone(&self.facade))
            .shutdown()
            .await
    }

    async fn workspace(&self) -> ResolvedWorkspace {
        self.facade
            .resolve_workspace(self.repo.path())
            .await
            .unwrap()
    }

    fn binding(workspace: &ResolvedWorkspace) -> SessionBinding {
        SessionBinding {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: workspace.project_id,
            workspace_id: workspace.workspace_id,
            cwd_relative_to_workspace: workspace.relative_cwd.clone(),
        }
    }

    fn session(&self, id: &str, workspace: &ResolvedWorkspace, frozen: &str) -> NewSession {
        NewSession {
            thread_id: id.to_owned(),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: Some(format!("session {id}")),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: Self::binding(workspace),
            frozen: FrozenSnapshotBytes::new(frozen.to_owned()),
        }
    }

    /// 建一个完整会话并返回 owner。
    async fn create(&self, id: &str) -> Arc<dyn SessionExecutionLease> {
        let workspace = self.workspace().await;
        let input = self.session(id, &workspace, &format!(r#"{{"v":1,"id":"{id}"}}"#));
        self.facade.create_session(&input).await.unwrap()
    }

    /// 未发布创建（J2 第一阶段）：草稿 + lease，frozen 尚未提交。
    async fn begin(&self, id: &str) -> Arc<dyn SessionInitialization> {
        let workspace = self.workspace().await;
        let draft = NewSessionDraft {
            thread_id: id.to_owned(),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: Some(format!("session {id}")),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: Self::binding(&workspace),
        };
        self.facade.begin_initialization(&draft).await.unwrap()
    }

    /// 直接经数据面落一份「数据已保存、执行代际未写」的会话（远程保存或崩溃留下的状态）。
    async fn save_without_admission(&self, id: &str, workspace: &ResolvedWorkspace) {
        let input = self.session(id, workspace, &format!(r#"{{"v":1,"id":"{id}"}}"#));
        self.facade
            .gate
            .data()
            .save_new_session(&input)
            .await
            .unwrap();
    }

    async fn count_threads(&self, id: &str) -> i64 {
        self.count("SELECT COUNT(*) FROM threads WHERE id = ?1", id)
            .await
    }

    async fn count_messages(&self, id: &str) -> i64 {
        self.count("SELECT COUNT(*) FROM messages WHERE thread_id = ?1", id)
            .await
    }

    async fn count_bindings(&self, id: &str) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM session_bindings WHERE thread_id = ?1",
            id,
        )
        .await
    }

    async fn count_execution_runs(&self, id: &str) -> i64 {
        self.count(
            "SELECT COUNT(*) FROM execution_runs WHERE thread_id = ?1",
            id,
        )
        .await
    }

    /// 本机执行代际行（`None` 表示这条 identity 没有行）。
    async fn execution_row(&self, id: &str) -> Option<(i64, bool)> {
        sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?1")
            .bind(id)
            .fetch_optional(self.facade.local_pool())
            .await
            .unwrap()
    }

    async fn count(&self, sql: &'static str, id: &str) -> i64 {
        let row: (i64,) = sqlx::query_as(sql)
            .bind(id)
            .fetch_one(self.facade.local_pool())
            .await
            .unwrap();
        row.0
    }
}

fn payload(text: &str) -> PersistedPayload {
    PersistedPayload::Message(peri_acp_types::messages::BaseMessage::human(text))
}

fn error_kind(error: &SessionResourceError) -> &SessionResourceErrorKind {
    error.kind()
}

// ─── 创建：完整数据 + owner 一次成立 ───────────────────────────────────────────

#[tokio::test]
async fn test_create_session_saves_complete_data_and_owner_in_one_step() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    let lease = fixture.create("s-new").await;

    // 完整数据：metadata、binding、frozen 都已可读。
    let snapshot = fixture
        .facade
        .load_session_snapshot(&"s-new".to_owned())
        .await
        .unwrap();
    assert_eq!(snapshot.meta.cwd, workspace.cwd.to_string_lossy());
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(Fixture::binding(&workspace))
    );
    assert_eq!(
        snapshot.frozen,
        FrozenState::Present(FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-new"}"#))
    );

    // 执行代际：一次提交里就带着 owner 事实（未结清）。
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&"s-new".to_owned())
            .await
            .unwrap(),
        Some((1, false))
    );
    assert_eq!(lease.thread_id(), &"s-new".to_owned());
    // 活 owner 在册：本次不能再次取得执行权（「有主」不是「需要恢复」）。
    assert_eq!(
        fixture
            .facade
            .inspect_availability(Some(&"s-new".to_owned()))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::OwnedElsewhere)
    );
    // owner 消失（崩溃等价）后剩下的才是代际事实：精确代际的普通 dirty。
    drop(lease);
    assert_eq!(
        fixture
            .facade
            .inspect_availability(Some(&"s-new".to_owned()))
            .await
            .unwrap()
            .execution,
        Some(ExecutionAvailability::Dirty(RecoveryRequiredDetails {
            thread_id: "s-new".to_owned(),
            generation: 1,
        }))
    );
}

#[tokio::test]
async fn test_create_session_collapses_data_and_generation_into_one_commit() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    // 用一条会失败的输入（binding 指向未登记工作区）证明失败时什么都不留：
    // 事务整体回滚，不会留下没有执行代际的会话行。
    let mut input = fixture.session("s-fail", &workspace, r#"{"v":1}"#);
    input.binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();
    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::InvalidBinding)
    ));
    assert_eq!(fixture.count_threads("s-fail").await, 0);
    assert_eq!(fixture.count_execution_runs("s-fail").await, 0);
}

#[tokio::test]
async fn test_create_session_rejects_a_reused_identity() {
    let fixture = Fixture::new().await;
    let _lease = fixture.create("s-dup").await;
    let workspace = fixture.workspace().await;
    let input = fixture.session("s-dup", &workspace, r#"{"v":1}"#);
    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
}

#[tokio::test]
async fn test_create_session_converges_when_data_was_saved_without_admission() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    // 崩在「数据已保存、执行代际未写」之间：下次同一个 ThreadId 的创建收敛准入，
    // 不重造 binding/frozen，也不报「已存在」。
    fixture
        .save_without_admission("s-converge", &workspace)
        .await;
    let input = fixture.session("s-converge", &workspace, r#"{"v":1,"id":"s-converge"}"#);
    let lease = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(lease.thread_id(), &"s-converge".to_owned());
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&"s-converge".to_owned())
            .await
            .unwrap(),
        Some((1, false))
    );
    assert_eq!(fixture.count_threads("s-converge").await, 1);
    assert_eq!(fixture.count_bindings("s-converge").await, 1);
    drop(lease);
}

#[tokio::test]
async fn test_create_session_reports_saved_but_not_admitted_when_premise_changed() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    fixture
        .save_without_admission("s-premise", &workspace)
        .await;
    // 同 identity 但换了一份绑定：数据已保存这一事实不变，准入前提不再成立。
    let mut input = fixture.session("s-premise", &workspace, r#"{"v":1}"#);
    input.binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();

    let error = match fixture.facade.create_session(&input).await {
        Ok(_) => panic!("expected create to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::SavedButNotAdmitted { .. }
    ));
    // 效果是「已生效」：数据仍在，不得被调用方据此删除。
    assert_eq!(
        error.effect(),
        peri_acp_types::session_resources::MutationOutcome::Applied
    );
    assert_eq!(fixture.count_threads("s-premise").await, 1);
}

// ─── 统一写入准入 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_read_only_store_refuses_registration_and_session_writes() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-readonly").await;
    lease.mark_clean().await.unwrap();
    drop(lease);
    let db_path = fixture._db.path().join("threads.db");

    let before: Vec<std::ffi::OsString> = std::fs::read_dir(fixture._db.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let read_only = SessionResourcesImpl::open_existing_read_only(&db_path)
        .await
        .unwrap();
    // 登记新身份：连会话都还没有，没有可降级的对象。
    let error = match read_only.resolve_workspace(fixture.repo.path()).await {
        Ok(_) => panic!("expected resolve_workspace to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ReadOnlyStore)
    ));
    // 已有会话上的写入：历史可读、执行权不可得。
    let error = read_only
        .append_history(&"s-readonly".to_owned(), &[payload("late")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    let error = match read_only
        .acquire_execution(
            &"s-readonly".to_owned(),
            &read_only_workspace(&fixture).await,
        )
        .await
    {
        Ok(_) => panic!("expected acquire_execution to fail on a read-only store"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::ReadOnlyStore
    ));
    // 只读路径不创建任何文件：目录内容与只读打开之前逐项一致（包括不建锁文件）。
    let after: Vec<std::ffi::OsString> = std::fs::read_dir(fixture._db.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
    assert!(!db_path
        .with_file_name("threads.db.execution-locks")
        .join("new.lock")
        .exists());
    // 读取仍然成立。
    let meta = read_only
        .load_session_meta(&"s-readonly".to_owned())
        .await
        .unwrap();
    assert_eq!(meta.title.as_deref(), Some("session s-readonly"));
}

async fn read_only_workspace(fixture: &Fixture) -> ResolvedWorkspace {
    fixture.workspace().await
}

#[tokio::test]
async fn test_mutation_without_live_owner_is_rejected() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    // 有绑定但没有活 owner（数据面保存过的会话）：不能因为没有 owner 就免授权。
    fixture.save_without_admission("s-owner", &workspace).await;
    let error = fixture
        .facade
        .append_history(&"s-owner".to_owned(), &[payload("no owner")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    assert_eq!(
        fixture
            .facade
            .inspect_availability(Some(&"s-owner".to_owned()))
            .await
            .unwrap()
            .execution,
        // 有绑定、无活 owner、无 dirty：执行所有权可以取得，这就是「可用」。
        Some(ExecutionAvailability::Available)
    );
}

/// 一次「已准入、效果无法证明」的写入：阻塞面与唯一的出路。
///
/// 未决证据只在**进程内的租约**上（v10 删除了本机 durable 锚点：不做跨安装的终态判定）。
/// 因此这里用真实的准入 + 未知效果驱动同一条 `Drop` 语义，再验证后续写入/删除/排空/clean
/// 被拒绝、读取面不受影响、被拒的写入没有留下效果；owner 消失之后，durable 事实只剩
/// `execution_runs` 的未结清代际，由显式风险接受收敛，下一代 owner 从头开始。
#[tokio::test]
async fn test_unresolved_write_blocks_until_explicit_recovery() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-unsure").await;
    let id = "s-unsure".to_owned();
    fixture
        .facade
        .append_history(&id, &[payload("first turn")])
        .await
        .unwrap();

    // 提交阶段的失败与取消走同一条路径：准入范围按 Unknown 结清，未决证据留在租约上。
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    scope.settle(&Err::<(), _>(SessionResourceError::persistence_uncertain(
        Some(id.clone()),
    )));

    // 后续写入与删除都被拒绝：不能在一个结果未知的写入之后继续写。
    let error = fixture
        .facade
        .append_history(&id, &[payload("blocked")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(_))
    ));
    let error = fixture.facade.delete_session_tree(&id).await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(_))
    ));
    // 未结清不能被伪装成已排空，也不能被当成干净收尾。
    let error = fixture.facade.drain_persistence(&id).await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    assert!(lease.mark_clean().await.is_err());
    // 被拒的写入没有留下效果：历史仍是第一轮那一条。
    assert_eq!(fixture.count_messages(&id).await, 1);
    // 读取面不受未决写影响：历史可读性与执行资格分开表达。
    assert!(fixture.facade.load_session_meta(&id).await.is_ok());
    let page = fixture
        .facade
        .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
            scope: peri_acp_types::workspace::ThreadScope::All,
            cursor: None,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(page.entries.iter().any(|entry| entry.thread.id == id));

    // owner 消失（进程结束等价）：剩下的 durable 事实只有未结清的代际本身。
    drop(lease);
    let error = match fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
    {
        Ok(_) => panic!("expected the unresolved generation to block a fresh owner"),
        Err(error) => error,
    };
    let SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(details)) =
        error_kind(&error)
    else {
        panic!("expected a dirty generation, got: {error:?}");
    };
    assert_eq!(details.generation, 1);
    // 显式风险接受是唯一出路：结清的是代际事实，下一代 owner 从 generation 2 开始。
    fixture
        .facade
        .reset_dirty_execution(&ResetDirtyRequest {
            target: RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation: details.generation,
            },
            accept_risk: true,
        })
        .await
        .unwrap();
    assert_eq!(fixture.execution_row(&id).await, Some((1, true)));
    let next = fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .unwrap();
    assert_eq!(next.thread_id(), &"s-unsure".to_owned());
    next.mark_clean().await.unwrap();
}

// ─── guard：只有效果确定才结清 ─────────────────────────────────────────────────

#[tokio::test]
async fn test_write_scope_settles_only_on_determinate_effect() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-settle").await;
    let id = "s-settle".to_owned();

    // 已证明未生效（NotApplied）：结清，后续写入仍然可以进入。
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    let not_applied: SessionResourceResult<()> = Err(SessionResourceError::new(
        SessionResourceErrorKind::InvalidInput {
            detail: "rejected before side effects".to_owned(),
        },
    ));
    scope.settle(&not_applied);
    fixture
        .facade
        .append_history(&id, &[payload("after not applied")])
        .await
        .unwrap();

    // 无法证明终态（Unknown）：不结清，同根后续写入与 clean 都被拒绝。
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    let unknown: SessionResourceResult<()> = Err(SessionResourceError::persistence_uncertain(
        Some(id.clone()),
    ));
    scope.settle(&unknown);
    let error = fixture
        .facade
        .append_history(&id, &[payload("after unknown")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(_))
    ));
    assert!(lease.mark_clean().await.is_err());
    // dirty 代际保持在库内：未结清的写入不会被当成干净收尾。
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&id)
            .await
            .unwrap(),
        Some((1, false))
    );
    drop(lease);
}

/// 提交阶段的失败必须被报成 Unknown：`NotApplied` 会让 `settle` 释放写入范围，
/// 而「提交是否落盘」在这一刻无从证明。这里用数据面提交映射经 `anyhow` 传播后的
/// 真实产物驱动准入（compaction 事务与本机执行面正是这条链路）。
#[tokio::test]
async fn test_commit_stage_failure_blocks_writes_and_clean() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-commit").await;
    let id = "s-commit".to_owned();

    let error = write_failure(anyhow::Error::new(commit_failure(Some(id.clone()))));
    assert!(error.is_persistence_uncertain());
    let scope = fixture.facade.gate.admit(&id).await.unwrap();
    scope.settle(&Err::<(), _>(error));

    // Unknown 不结清范围：同根后续写入与 clean 都被拒绝，库内代际仍是 dirty。
    let blocked = fixture
        .facade
        .append_history(&id, &[payload("after commit failure")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&blocked),
        SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(_))
    ));
    assert!(lease.mark_clean().await.is_err());
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&id)
            .await
            .unwrap(),
        Some((1, false))
    );
    drop(lease);
}

#[tokio::test]
async fn test_cancelled_mutation_leaves_uncertain_lease_and_blocks_clean() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-cancel").await;
    let id = "s-cancel".to_owned();
    {
        // 丢弃准入范围而不结清，等价于写入 future 在提交边界被取消。
        let _scope = fixture.facade.gate.admit(&id).await.unwrap();
    }
    assert!(lease.mark_clean().await.is_err());
    let error = fixture
        .facade
        .append_history(&id, &[payload("after cancel")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(_))
    ));
    // 排空同样不得把未决写入当成已结清。
    let error = fixture.facade.drain_persistence(&id).await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    drop(lease);
}

// ─── 撤销未发布创建 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_abandon_initialization_revokes_data_and_allows_a_fresh_retry() {
    let fixture = Fixture::new().await;
    // 撤销的对象是**未提交 frozen** 的草稿（两阶段的第一阶段）；已提交的会话不是
    // 「未发布创建」，见 `test_abandon_refuses_a_draft_whose_frozen_was_committed`。
    let initialization = fixture.begin("s-abandon").await;
    let lease = initialization.execution_lease();
    let other = fixture.create("s-other").await;
    let id = "s-abandon".to_owned();

    // 别的 owner 不能替它承担补偿：撤销会删执行行，必须由持有它的同一所有权发起。
    let error = fixture
        .facade
        .abandon_initialization(&id, &other)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    assert_eq!(fixture.count_threads(&id).await, 1);

    initialization.clone().abandon().await.unwrap();
    // 数据与执行代际行一起撤销，本机不留第二份痕迹：撤销判定只依据现有数据事实
    // （v10 删掉了「初始化被放弃」的终态锚点表）。
    assert_eq!(fixture.count_threads(&id).await, 0);
    assert_eq!(fixture.count_bindings(&id).await, 0);
    assert_eq!(fixture.count_execution_runs(&id).await, 0);
    // 补偿走的是放弃所有权，不是 clean：这里不会写出一条假的 clean 记录。
    lease.mark_clean().await.unwrap();
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&id)
            .await
            .unwrap(),
        None
    );
    // 同一 identity 可以重来：这条创建从没发布过，客户端按同一个 id 重试是正常动作
    // （删除则更彻底——它删的是已发布会话的数据，见 `test_delete_ends_data_execution_facts_and_ownership`）。
    let workspace = fixture.workspace().await;
    let input = fixture.session(&id, &workspace, r#"{"v":1,"id":"s-abandon"}"#);
    let fresh = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fresh.thread_id().as_str(), id);
    assert_eq!(
        fixture.execution_row(&id).await,
        Some((1, false)),
        "重试建出的是全新会话：代际从 1 开始且未结清"
    );
    drop(other);
}

// ─── J2 两阶段：草稿 → commit_frozen / abandon ────────────────────────────────

impl Fixture {
    /// `threads.frozen_context` 原值（`None` = 从未提交/半写草稿）。
    async fn frozen_of(&self, id: &str) -> Option<String> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT frozen_context FROM threads WHERE id = ?1")
                .bind(id)
                .fetch_optional(self.facade.local_pool())
                .await
                .unwrap();
        row.and_then(|(frozen,)| frozen)
    }

    /// 同一库的第二个宿主句柄（另一条连接、另一套进程内登记）。
    async fn second_host(&self) -> Arc<SessionResourcesImpl> {
        Arc::new(
            SessionResourcesImpl::open(self._db.path().join("threads.db"))
                .await
                .unwrap(),
        )
    }

    async fn visible_ids(&self) -> Vec<String> {
        self.facade
            .list_sessions(&peri_acp_types::workspace::ScopedThreadQuery {
                scope: peri_acp_types::workspace::ThreadScope::All,
                cursor: None,
                limit: 50,
            })
            .await
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.thread.id.clone())
            .collect()
    }
}

/// 第一阶段：草稿行成立（frozen 暂空、代际未结清）、不出现在列表、同 identity 只有一个 owner。
#[tokio::test]
async fn test_begin_initialization_writes_draft_without_frozen() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-draft").await;
    let id = "s-draft".to_owned();

    assert_eq!(initialization.thread_id(), &id);
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
    assert_eq!(fixture.execution_row(&id).await, Some((1, false)));
    assert_eq!(fixture.frozen_of(&id).await, None, "草稿不得带 frozen");
    assert!(
        !fixture.visible_ids().await.contains(&id),
        "未发布草稿不得出现在会话列表"
    );

    // 第二个宿主不能同时取得这条 identity 的执行所有权（sidecar OS 锁）。
    let second = fixture.second_host().await;
    let workspace = fixture.workspace().await;
    let error = match second.acquire_execution(&id, &workspace).await {
        Ok(_) => panic!("second host must not own the same identity"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBusy)
    ));
}

/// 第二阶段：commit 后 frozen 可读；重复 commit / 已提交后 abandon 都是 typed 冲突且不覆盖。
#[tokio::test]
async fn test_commit_frozen_is_write_once_and_blocks_abandon() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-commit").await;
    let id = "s-commit".to_owned();
    let frozen = FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-commit"}"#);

    initialization.commit_frozen(&frozen).await.unwrap();
    assert_eq!(
        fixture.frozen_of(&id).await.as_deref(),
        Some(frozen.as_str())
    );
    let snapshot = fixture.facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen.clone()));
    assert_eq!(fixture.execution_row(&id).await, Some((1, false)));

    // 重复提交：typed 冲突，且不覆盖已提交字节。
    let other = FrozenSnapshotBytes::new(r#"{"v":1,"id":"second-write"}"#);
    let error = initialization.commit_frozen(&other).await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(
        fixture.frozen_of(&id).await.as_deref(),
        Some(frozen.as_str())
    );

    // 已提交的草稿不是「未发布创建」：撤销必须拒绝，且一条行都不删。
    let error = initialization.clone().abandon().await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads(&id).await, 1);
    assert_eq!(fixture.count_bindings(&id).await, 1);
    assert_eq!(fixture.count_execution_runs(&id).await, 1);
}

/// abandon 幂等：先成功后重复调用仍成功（行已不在，目标已达成）。
#[tokio::test]
async fn test_abandon_is_idempotent_after_success() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-idem").await;
    let id = "s-idem".to_owned();

    initialization.clone().abandon().await.unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    initialization.clone().abandon().await.unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    assert_eq!(fixture.count_execution_runs(&id).await, 0);
}

/// 崩溃矩阵（未提交）：重开之后草稿仍不可见、cold load 得到「bound 但无 frozen」，
/// 清理判据成立时删除全部行；已提交的会话不走清理（交给 dirty 恢复）。
#[tokio::test]
async fn test_crash_before_commit_leaves_removable_draft() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-crash").await;
    let id = "s-crash".to_owned();
    // 崩溃等价：进程内的 owner 随句柄一起消失（sidecar 锁由内核释放）。
    drop(initialization);

    let reopened = fixture.second_host().await;
    assert!(
        !fixture.visible_ids().await.contains(&id),
        "半写草稿不得出现在会话列表"
    );
    let snapshot = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(
        snapshot.binding,
        BindingState::Bound(Fixture::binding(&fixture.workspace().await)),
        "绑定已成立"
    );
    assert_eq!(
        snapshot.frozen,
        FrozenState::LegacyAbsent,
        "bound 且无 frozen 是半写判据（ACP 侧按 typed 错误 fail-closed）"
    );

    reopened
        .discard_incomplete_initialization(&id)
        .await
        .unwrap();
    assert_eq!(fixture.count_threads(&id).await, 0);
    assert_eq!(fixture.count_bindings(&id).await, 0);
    assert_eq!(fixture.count_execution_runs(&id).await, 0);
}

/// 崩溃矩阵（已提交）：cold load 可读，下一次准入是精确代际的 dirty，显式接受风险后可用。
#[tokio::test]
async fn test_crash_after_commit_is_readable_and_needs_dirty_recovery() {
    let fixture = Fixture::new().await;
    let initialization = fixture.begin("s-post").await;
    let frozen = FrozenSnapshotBytes::new(r#"{"v":1,"id":"s-post"}"#);
    initialization.commit_frozen(&frozen).await.unwrap();
    let id = "s-post".to_owned();
    drop(initialization);

    let reopened = fixture.second_host().await;
    let snapshot = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(snapshot.frozen, FrozenState::Present(frozen.clone()));

    let workspace = fixture.workspace().await;
    let error = match reopened.acquire_execution(&id, &workspace).await {
        Ok(_) => panic!("a crashed generation must not be acquireable without recovery"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(_))
    ));
    // 「已提交未发布」不得被自动清理。
    let error = reopened
        .discard_incomplete_initialization(&id)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads(&id).await, 1);

    // 既有 dirty 恢复（显式接受风险）之后可以继续执行。
    reopened
        .reset_dirty_execution(&ResetDirtyRequest {
            target: RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation: 1,
            },
            accept_risk: true,
        })
        .await
        .unwrap();
    let lease = reopened.acquire_execution(&id, &workspace).await.unwrap();
    assert_eq!(lease.thread_id(), &id);
}

/// 清理判据与 legacy 互斥：无绑定行的会话不被清理（它不是半写草稿）。
#[tokio::test]
async fn test_discard_refuses_a_session_without_binding() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace().await;
    // legacy 形态：有行、有 cwd、无绑定、无执行代际。
    sqlx::query(
        "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count,
            parent_thread_id, snapshot_at_message_id, hidden, cancel_policy, config,
            cached_context, frozen_context, agent_status, context_cache_epoch)
         VALUES ('s-legacy', NULL, ?1, '2026-09-26T00:00:00Z', '2026-09-26T00:00:00Z', 0,
            NULL, NULL, 0, '{}', NULL, NULL, NULL, 'active', 0)",
    )
    .bind(workspace.cwd.to_string_lossy().into_owned())
    .execute(fixture.facade.local_pool())
    .await
    .unwrap();

    let error = fixture
        .facade
        .discard_incomplete_initialization(&"s-legacy".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads("s-legacy").await, 1);
}

// ─── child：沿用 root owner ───────────────────────────────────────────────────

impl Fixture {
    /// 在 root 之下建一个 child（frozen 取自 root 的已保存快照）。
    async fn child(
        &self,
        child_id: &str,
        root: &str,
        root_lease: &Arc<dyn SessionExecutionLease>,
    ) -> ChildSnapshot {
        let snapshot = self.child_snapshot(child_id, root).await;
        self.facade.save_child(&snapshot, root_lease).await.unwrap();
        snapshot
    }

    /// 只构造 child 快照、不保存：用于在准入被占用时观察写入是否真的等门禁。
    async fn child_snapshot(&self, child_id: &str, root: &str) -> ChildSnapshot {
        let workspace = self.workspace().await;
        let root_frozen = self
            .facade
            .load_session_snapshot(&root.to_owned())
            .await
            .unwrap()
            .frozen;
        let FrozenState::Present(root_frozen) = root_frozen else {
            panic!("root frozen snapshot is missing");
        };
        ChildSnapshot {
            target: NewSession {
                thread_id: child_id.to_owned(),
                created_at: "2026-09-26T00:00:01Z".to_owned(),
                meta: NewSessionMeta {
                    title: Some(format!("child {child_id}")),
                    cwd: workspace.cwd.to_string_lossy().into_owned(),
                    parent_thread_id: Some(root.to_owned()),
                    hidden: true,
                    cancel_policy: Default::default(),
                    snapshot_at_message_id: None,
                },
                binding: Fixture::binding(&workspace),
                frozen: root_frozen,
            },
            parent_id: root.to_owned(),
            root_id: root.to_owned(),
            inherited: peri_acp_types::store::InheritedContext {
                payloads: Vec::new(),
                flags: std::collections::HashMap::new(),
            },
        }
    }
}

#[tokio::test]
async fn test_save_child_requires_the_root_owner_and_shares_its_gate() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-child-root").await;
    let foreign = fixture.create("s-child-foreign").await;
    let workspace = fixture.workspace().await;
    let root_frozen = match fixture
        .facade
        .load_session_snapshot(&"s-child-root".to_owned())
        .await
        .unwrap()
        .frozen
    {
        FrozenState::Present(frozen) => frozen,
        other => panic!("root frozen snapshot is missing: {other:?}"),
    };
    let snapshot = ChildSnapshot {
        target: NewSession {
            thread_id: "s-child".to_owned(),
            created_at: "2026-09-26T00:00:01Z".to_owned(),
            meta: NewSessionMeta {
                title: Some("child".to_owned()),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: Some("s-child-root".to_owned()),
                hidden: true,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: Fixture::binding(&workspace),
            frozen: root_frozen,
        },
        parent_id: "s-child-root".to_owned(),
        root_id: "s-child-root".to_owned(),
        inherited: peri_acp_types::store::InheritedContext {
            payloads: Vec::new(),
            flags: std::collections::HashMap::new(),
        },
    };
    // 别的 root 的 owner 不能借来写这条 child。
    let error = fixture
        .facade
        .save_child(&snapshot, &foreign)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    assert_eq!(fixture.count_threads("s-child").await, 0);

    fixture
        .facade
        .save_child(&snapshot, &root_lease)
        .await
        .unwrap();
    // 子会话没有自己的执行代际：写入落在 root 的 owner 上。
    assert_eq!(fixture.count_execution_runs("s-child").await, 0);
    fixture
        .facade
        .append_history(&"s-child".to_owned(), &[payload("child turn")])
        .await
        .unwrap();
    // root owner 关闭后，child 的写入同样被拒绝。
    root_lease.mark_clean().await.unwrap();
    drop(root_lease);
    let error = fixture
        .facade
        .append_history(&"s-child".to_owned(), &[payload("after clean")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    drop(foreign);
}

/// child 的写入归属 root 执行域：它必须与 root 的写入共享同一条门禁，而不是因为
/// 「child 自己还没有 identity/owner」就免于门禁（B §4.1.3）。
#[tokio::test]
async fn test_save_child_write_waits_for_the_root_gate() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-gate-root").await;
    let snapshot = fixture.child_snapshot("s-gate-child", "s-gate-root").await;

    // 占住 root 的写侧门禁（等价于该 owner 上另一次 mutation 正在检查+写入之间）。
    let facts = facts_of(&fixture.facade, "s-gate-root").await;
    let held = fixture
        .facade
        .gate
        .local()
        .exclusive_guard(&"s-gate-root".to_owned(), &facts)
        .await
        .unwrap()
        .expect("the root owner is alive");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fixture.facade.save_child(&snapshot, &root_lease),
        )
        .await
        .is_err(),
        "child save must wait for the root's write gate"
    );
    assert_eq!(
        fixture.count_threads("s-gate-child").await,
        0,
        "no child data may be written while the root gate is held"
    );
    held.finish();

    // 门禁释放后同一次保存成立，且没有把 root 标成未决（结清只认确定性）。
    fixture
        .facade
        .save_child(&snapshot, &root_lease)
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-gate-child").await, 1);
    root_lease.mark_clean().await.unwrap();
    drop(root_lease);
}

/// 父子关系在 child 快照里出现两次：`parent_id`（声明）与 `target.meta.parent_thread_id`
/// （落库用的那一份）。只校验前者、照后者落库，会把「声明了合法父/根」的 child 写成一条
/// **没有父的独立 root**——此后它还能自己取得执行权。门面必须在任何副作用之前拒绝，
/// 并且拒绝不留痕（零行、无 lease、root 原样）。
#[tokio::test]
async fn test_save_child_refuses_a_snapshot_that_disagrees_with_its_parent_relation() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-rel-root").await;
    let legal = fixture.child_snapshot("s-rel-child", "s-rel-root").await;

    // 目标 meta 里没有父；父与声明不同；自指父关系；把自己当根。
    let mut without_parent = legal.clone();
    without_parent.target.meta.parent_thread_id = None;
    let mut other_parent = legal.clone();
    other_parent.target.meta.parent_thread_id = Some("s-rel-other".to_owned());
    let mut self_parent = legal.clone();
    self_parent.parent_id = "s-rel-child".to_owned();
    self_parent.target.meta.parent_thread_id = Some("s-rel-child".to_owned());
    let mut own_root = legal.clone();
    own_root.root_id = "s-rel-child".to_owned();

    for (label, snapshot) in [
        ("target without a parent", &without_parent),
        ("target under another parent", &other_parent),
        ("self parent", &self_parent),
        ("own root", &own_root),
    ] {
        let error = fixture
            .facade
            .save_child(snapshot, &root_lease)
            .await
            .unwrap_err();
        assert!(
            matches!(
                error_kind(&error),
                SessionResourceErrorKind::InvalidInput { .. }
            ),
            "{label}: expected InvalidInput, got {error:?}"
        );
        // 零行：没有会话、没有绑定、没有执行代际。
        assert_eq!(fixture.count_threads("s-rel-child").await, 0, "{label}");
        assert_eq!(fixture.count_bindings("s-rel-child").await, 0, "{label}");
        assert_eq!(
            fixture.count_execution_runs("s-rel-child").await,
            0,
            "{label}"
        );
        // 无 lease：这条 identity 不存在，也就没有独立 root 可取得执行权。
        let facts = facts_of(&fixture.facade, "s-rel-child").await;
        assert!(
            fixture
                .facade
                .gate
                .local()
                .owner_lease(&"s-rel-child".to_owned(), &facts)
                .await
                .unwrap()
                .is_none(),
            "{label}"
        );
    }
    // 原 root 不变：仍是独立 root、树里只有自己、children 为空，且它的 owner 照常可写。
    let root_meta = fixture
        .facade
        .load_session_meta(&"s-rel-root".to_owned())
        .await
        .unwrap();
    assert_eq!(root_meta.parent_thread_id, None);
    assert_eq!(
        fixture
            .facade
            .list_session_tree(&"s-rel-root".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(fixture
        .facade
        .list_children(&"s-rel-root".to_owned())
        .await
        .unwrap()
        .is_empty());
    fixture
        .facade
        .append_history(&"s-rel-root".to_owned(), &[payload("root turn")])
        .await
        .unwrap();

    // 合法的 child 仍然成立，且仍然挂在 root 之下（不是独立 root，也没有自己的执行代际）。
    fixture
        .facade
        .save_child(&legal, &root_lease)
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-rel-child").await, 1);
    assert_eq!(fixture.count_execution_runs("s-rel-child").await, 0);
    assert_eq!(
        fixture
            .facade
            .load_session_meta(&"s-rel-child".to_owned())
            .await
            .unwrap()
            .parent_thread_id
            .as_deref(),
        Some("s-rel-root")
    );
    assert_eq!(
        fixture
            .facade
            .list_children(&"s-rel-root".to_owned())
            .await
            .unwrap()
            .len(),
        1
    );
    // 子会话没有自己的执行代际：它解析到的是 root 的那条 owner，而不是自己当 root。
    let facts = facts_of(&fixture.facade, "s-rel-child").await;
    let owned = fixture
        .facade
        .gate
        .local()
        .owner_lease(&"s-rel-child".to_owned(), &facts)
        .await
        .unwrap()
        .expect("the child belongs to the root's execution domain");
    assert!(owned.is_active());
    assert_eq!(owned.thread_id(), &"s-rel-root".to_owned());
    root_lease.mark_clean().await.unwrap();
    drop(root_lease);
}

#[tokio::test]
async fn test_claim_child_resume_serializes_and_restores_previous_state() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-claim-root").await;
    let child = fixture
        .child("s-claim-child", "s-claim-root", &root_lease)
        .await;
    let child_id = child.target.thread_id.clone();
    let root_id = child.root_id.clone();
    // 认领前的状态：Done（非 active），用于观察恢复是否真的发生了。
    fixture
        .facade
        .update_session_meta(
            &child_id,
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Active);
    assert!(record.claimed);
    // 仍在 active：并发/重复认领被拒绝，不会有两个执行者。
    let error = match fixture.facade.claim_child_resume(&child_id, &root_id).await {
        Ok(_) => panic!("expected the active child to refuse a second claim"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    // 准备失败：恢复到认领前，不留 active 残留。
    claim.mark_failed().await.unwrap();
    let record = fixture
        .facade
        .gate
        .data()
        .load_child_resume_record(&child_id)
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Done);
    assert!(!record.claimed);

    // 移交后台后，前台的终止声明不能覆盖后台持有的终态。
    let claim = fixture
        .facade
        .claim_child_resume(&child_id, &root_id)
        .await
        .unwrap();
    claim.hand_off_to_background().await.unwrap();
    let error = claim.mark_terminated().await.unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));

    // root owner 不在本进程时不能认领。
    drop(root_lease);
    let error = match fixture.facade.claim_child_resume(&child_id, &root_id).await {
        Ok(_) => panic!("expected claim without a live root owner to fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
}

// ─── 删除与恢复证据 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_delete_ends_data_execution_facts_and_ownership() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-del-root").await;
    let _child = fixture
        .child("s-del-child", "s-del-root", &root_lease)
        .await;

    fixture
        .facade
        .delete_session_tree(&"s-del-root".to_owned())
        .await
        .unwrap();
    // 删除即删除：整棵树的数据、绑定、执行代际行都不在，也没有第二份「被删过」的痕迹
    // （v10 之后本机不为终止状态留锚点——删除的对象是数据，不是身份）。
    assert_eq!(fixture.count_threads("s-del-root").await, 0);
    assert_eq!(fixture.count_threads("s-del-child").await, 0);
    assert_eq!(fixture.count_bindings("s-del-root").await, 0);
    assert_eq!(fixture.count_execution_runs("s-del-root").await, 0);
    assert_eq!(fixture.count_execution_runs("s-del-child").await, 0);
    // 删除同时结束本次所有权：owner 不再接收写入（下面按「没有活 owner」被拒），锁也已
    // 释放——这一点由本测试末尾用同一 identity 重新创建证明：重建要重新取得同一把
    // sidecar 锁，锁没释放就会是 `ExecutionBusy`。
    root_lease.mark_clean().await.unwrap();
    assert_eq!(fixture.count_execution_runs("s-del-root").await, 0);
    // 收敛读取没有对象：会话不存在，就没有「可重载」这回事。
    let error = fixture
        .facade
        .recover_session_persistence(&"s-del-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::NotFound
    ));
    // 收尾中的 owner 仍在册：此刻的写入按「没有活 owner」被拒绝——所有权事实优先于
    // 数据事实，重试收尾不会被悄悄放行。
    let error = fixture
        .facade
        .delete_session_tree(&"s-del-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    drop(root_lease);
    // owner 释放后，剩下的结论才是数据事实：这条会话已不存在。
    let error = fixture
        .facade
        .delete_session_tree(&"s-del-root".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::NotFound
    ));
    // 同一 identity 可以重新创建：删除的对象是这条会话的数据与执行事实，不是这个名字。
    // 没有 durable 痕迹时不留「不许再用」的封印——那需要一张跨进程存活的表，而本机
    // 不再有那样的表（见 schema v10 的删除清单）。
    let workspace = fixture.workspace().await;
    let input = fixture.session("s-del-root", &workspace, r#"{"v":1}"#);
    let fresh = fixture.facade.create_session(&input).await.unwrap();
    assert_eq!(fresh.thread_id().as_str(), "s-del-root");
    assert_eq!(
        fixture.execution_row("s-del-root").await,
        Some((1, false)),
        "重新创建是全新的一条会话，代际从 1 开始且未结清"
    );
}

// ─── 排空与关闭 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_close_stops_new_writes_and_reports_unsettled_owners() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-close").await;
    let id = "s-close".to_owned();
    fixture.facade.drain_persistence(&id).await.unwrap();

    fixture.shutdown().await.unwrap();
    // 关闭后不再接受新写入；读取仍然可用（历史可解释性不受影响）。
    let error = fixture
        .facade
        .append_history(&id, &[payload("after close")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Unavailable { .. }
    ));
    assert!(fixture.facade.load_session_meta(&id).await.is_ok());
    // 重复关闭是幂等成功。
    fixture.shutdown().await.unwrap();
    // 收尾仍由 owner 完成：关闭不等于替 owner 写完 clean。
    lease.mark_clean().await.unwrap();
    drop(lease);

    // 未结清的写入存在时，关闭不宣告完成。
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-close-uncertain").await;
    {
        let _scope = fixture
            .facade
            .gate
            .admit(&"s-close-uncertain".to_owned())
            .await
            .unwrap();
    }
    let error = fixture.shutdown().await.unwrap_err();
    assert!(error.is_persistence_uncertain());
    drop(lease);
}

// ─── 跨进程：他处所有权、崩溃后的 dirty 与排空 ─────────────────────────────────

/// 子进程入口：按环境变量在**另一进程**里执行同一门面动作。
///
/// 没有环境变量时直接返回，因此它只在被父测试拉起时工作。
#[tokio::test]
async fn test_facade_child_process() {
    let Ok(db) = std::env::var("PERI_TEST_FACADE_DB") else {
        return;
    };
    let id = std::env::var("PERI_TEST_FACADE_ID").unwrap();
    let repo = std::env::var("PERI_TEST_FACADE_REPO").unwrap();
    let expected = std::env::var("PERI_TEST_FACADE_EXPECT").unwrap();
    let facade = SessionResourcesImpl::open(db).await.unwrap();
    let workspace = facade.resolve_workspace(Path::new(&repo)).await.unwrap();
    match expected.as_str() {
        // 他处持有 owner：本进程只能报忙，不能取得所有权。
        "busy" => {
            let error = match facade.acquire_execution(&id, &workspace).await {
                Ok(_) => panic!("acquired ownership while another process holds it"),
                Err(error) => error,
            };
            assert!(matches!(
                error_kind(&error),
                SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBusy)
            ));
        }
        // 崩溃：取得所有权后直接退出，不写 clean，锁由 OS 释放。
        "crash" => {
            let _lease = facade.acquire_execution(&id, &workspace).await.unwrap();
            std::process::exit(0);
        }
        other => panic!("unknown expected child result: {other}"),
    }
}

fn facade_process(db: &Path, repo: &Path, id: &str, expected: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::resources::tests::test_facade_child_process",
            "--nocapture",
        ])
        .env("PERI_TEST_FACADE_DB", db)
        .env("PERI_TEST_FACADE_REPO", repo)
        .env("PERI_TEST_FACADE_ID", id)
        .env("PERI_TEST_FACADE_EXPECT", expected)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "facade child failed ({expected}): {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_facade_owner_competes_across_processes_and_crash_stays_dirty() {
    let fixture = Fixture::new().await;
    let id = "s-xproc".to_owned();
    let lease = fixture.create(&id).await;
    let db = fixture._db.path().join("threads.db");
    let repo = fixture.repo.path();

    // 本进程持有 owner：另一进程的取得所有权必须失败，且不产生第二代。
    facade_process(&db, repo, &id, "busy");
    assert_eq!(
        fixture
            .facade
            .gate
            .local()
            .execution_state(&id)
            .await
            .unwrap(),
        Some((1, false))
    );

    // 干净收尾后释放所有权，另一进程取得第二代并崩溃：脏代际跨进程保留。
    lease.mark_clean().await.unwrap();
    drop(lease);
    facade_process(&db, repo, &id, "crash");
    let error = match fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
    {
        Ok(_) => panic!("expected recovery to be required after the other process crashed"),
        Err(error) => error,
    };
    let SessionResourceErrorKind::Workspace(WorkspaceError::RecoveryRequired(details)) =
        error_kind(&error)
    else {
        panic!("expected a dirty generation, got: {error:?}");
    };
    assert_eq!(details.generation, 2);
    // 崩溃留下的只是普通 dirty：排空不会把它当成未决写入，解除仍要显式接受风险。
    fixture.facade.drain_persistence(&id).await.unwrap();
    let error = fixture
        .facade
        .reset_dirty_execution(&ResetDirtyRequest {
            target: RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation: 2,
            },
            accept_risk: false,
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    fixture
        .facade
        .reset_dirty_execution(&ResetDirtyRequest {
            target: RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation: 2,
            },
            accept_risk: true,
        })
        .await
        .unwrap();
    // 精确解除后同一 identity 仍可正常取得所有权并收尾。
    let next = fixture
        .facade
        .acquire_execution(&id, &fixture.workspace().await)
        .await
        .unwrap();
    assert_eq!(next.thread_id(), &id);
    next.mark_clean().await.unwrap();
}

// ─── fork 的收敛与撤销边界 ───────────────────────────────────────────────────

#[tokio::test]
async fn test_save_fork_converges_when_the_target_was_saved_without_admission() {
    let fixture = Fixture::new().await;
    let source_lease = fixture.create("s-fork-source").await;
    let workspace = fixture.workspace().await;
    let fork = ForkSnapshot {
        target: fixture.session(
            "s-fork-target",
            &workspace,
            r#"{"v":1,"id":"s-fork-source"}"#,
        ),
        source_id: "s-fork-source".to_owned(),
        payloads: vec![payload("forked turn")],
        flags: std::collections::HashMap::new(),
    };
    // 数据先落库、准入未成立（远程保存或上次进程在准入前结束），再重试同一次 fork：
    // 收敛准入，不重复写历史，也不把「已保存」报成「已存在」。
    fixture.facade.gate.data().save_fork(&fork).await.unwrap();
    let lease = fixture.facade.save_fork(&fork).await.unwrap();
    assert_eq!(lease.thread_id(), &"s-fork-target".to_owned());
    assert_eq!(fixture.count_execution_runs("s-fork-target").await, 1);
    assert_eq!(fixture.count_messages("s-fork-target").await, 1);
    // 数据已保存但前提变化时才报「已保存、未准入」。
    let mut changed = fork.clone();
    changed.target.binding.workspace_id = peri_acp_types::workspace::WorkspaceId::new();
    let error = match fixture.facade.save_fork(&changed).await {
        Ok(_) => panic!("expected the changed premise to refuse admission"),
        Err(error) => error,
    };
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    drop(lease);
    drop(source_lease);
}

/// fork 目标的失败补偿是 write-once 语义：目标创建即带 frozen，撤销仍必须把它整条删掉
/// （「已提交 frozen 拒绝删除」只约束两阶段草稿，见
/// `test_commit_frozen_is_write_once_and_blocks_abandon`）。
#[tokio::test]
async fn test_abandon_initialization_deletes_a_fork_target_with_frozen() {
    let fixture = Fixture::new().await;
    let _source_lease = fixture.create("s-fork-source").await;
    let workspace = fixture.workspace().await;
    let fork = ForkSnapshot {
        target: fixture.session(
            "s-fork-target",
            &workspace,
            r#"{"v":1,"id":"s-fork-source"}"#,
        ),
        source_id: "s-fork-source".to_owned(),
        payloads: vec![payload("forked turn")],
        flags: std::collections::HashMap::new(),
    };
    let target_lease = fixture.facade.save_fork(&fork).await.unwrap();
    assert_eq!(fixture.count_execution_runs("s-fork-target").await, 1);
    assert_eq!(fixture.count_messages("s-fork-target").await, 1);

    // 与 `handle_fork` 的失败补偿同一条调用：装配/身份失败后撤销未发布目标。
    fixture
        .facade
        .abandon_initialization(&"s-fork-target".to_owned(), &target_lease)
        .await
        .unwrap();

    // 目标不留残渣：会话行、绑定、历史与执行代际一起消失。
    assert_eq!(fixture.count_threads("s-fork-target").await, 0);
    assert_eq!(fixture.count_bindings("s-fork-target").await, 0);
    assert_eq!(fixture.count_messages("s-fork-target").await, 0);
    assert_eq!(fixture.count_execution_runs("s-fork-target").await, 0);
    // source 原样保留：撤销不是通用 rollback。
    assert_eq!(fixture.count_threads("s-fork-source").await, 1);
    assert_eq!(
        fixture.execution_row("s-fork-source").await,
        Some((1, false))
    );
}

/// 数据面两条撤销入口的判据分档：write-once（不叠加 frozen 判据）与两阶段草稿（叠加）。
#[tokio::test]
async fn test_revoke_entry_points_differ_only_by_the_frozen_criterion() {
    let fixture = Fixture::new().await;
    let committed = fixture.create("s-write-once").await;
    let draft = fixture.begin("s-draft-only").await;

    // 两阶段草稿：已提交 ⇒ 拒绝；未提交 ⇒ 删除。
    let error = fixture
        .facade
        .gate
        .data()
        .revoke_unpublished_draft(&"s-write-once".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Conflict { .. }
    ));
    assert_eq!(fixture.count_threads("s-write-once").await, 1);
    fixture
        .facade
        .gate
        .data()
        .revoke_unpublished_draft(&"s-draft-only".to_owned())
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-draft-only").await, 0);

    // write-once：同一条已提交会话走这条入口必须真的删除（fork 补偿语义）。
    fixture
        .facade
        .gate
        .data()
        .revoke_unpublished_session(&"s-write-once".to_owned())
        .await
        .unwrap();
    assert_eq!(fixture.count_threads("s-write-once").await, 0);
    drop(committed);
    drop(draft);
}

#[tokio::test]
async fn test_revoke_refuses_a_session_that_already_has_children() {
    let fixture = Fixture::new().await;
    let root_lease = fixture.create("s-revoke-root").await;
    let _child = fixture
        .child("s-revoke-child", "s-revoke-root", &root_lease)
        .await;

    // 已派生过子会话的 identity 不能被补偿掉：否则子会话会指向不存在的父节点。
    let error = fixture
        .facade
        .abandon_initialization(&"s-revoke-root".to_owned(), &root_lease)
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert_eq!(fixture.count_threads("s-revoke-root").await, 1);
    assert_eq!(fixture.count_threads("s-revoke-child").await, 1);
    assert_eq!(fixture.count_execution_runs("s-revoke-root").await, 1);
    // 失败不留半撤销状态：owner 与两条会话都仍然可用。
    fixture
        .facade
        .append_history(&"s-revoke-child".to_owned(), &[payload("still usable")])
        .await
        .unwrap();
    root_lease.mark_clean().await.unwrap();
}

#[tokio::test]
async fn test_cancelled_close_does_not_become_success() {
    let fixture = Fixture::new().await;
    let lease = fixture.create("s-close-cancel").await;
    let id = "s-close-cancel".to_owned();
    // 一条已准入、未结清的写入：关闭必须先等它结束，而不是宣告完成。
    let scope = fixture.facade.gate.admit(&id).await.unwrap();

    // 第一次关闭在等待中被取消（调用方超时放弃）。
    let cancelled = tokio::time::timeout(Duration::from_millis(50), fixture.shutdown()).await;
    assert!(
        cancelled.is_err(),
        "close must wait for the in-flight write"
    );
    // 取消不构成任何确认：下一次关闭仍要真实等待，不能直接成功。
    let again = tokio::time::timeout(Duration::from_millis(50), fixture.shutdown()).await;
    assert!(
        again.is_err(),
        "a cancelled close must not be recorded as closed"
    );

    // 在途写入结清之后关闭才成立；此时重复关闭才是幂等成功。
    scope.settle(&Ok::<(), SessionResourceError>(()));
    fixture.shutdown().await.unwrap();
    fixture.shutdown().await.unwrap();
    drop(lease);
}

// ─── 双库：数据面在别处（远程组合的离线等价物） ───────────────────────────────

/// 执行面事实：与门面内部（`MutationGate::session_facts`）用的是同一个取法。
///
/// 直接驱动本机执行面的测试必须按同一组事实判定：「绑定/树根由数据面回答」这条契约不能只
/// 在门面里成立，否则测试锁的会是「本机恰好查得到自己那张表」这个实现细节。
async fn facts_of(facade: &SessionResourcesImpl, id: &str) -> SessionFacts {
    facade.gate.session_facts(&id.to_owned()).await.unwrap()
}

/// 数据面与本机执行面在**两个**库里的门面：远程组合的离线等价物。
///
/// 装配点与远程组合相同（`SessionResourcesImpl::from_ports` + `SessionDataHome::RemoteStore`），
/// 只是数据面用另一个真 sqlite 顶替远端 adapter：本机执行面库因此**没有**这条会话的任何
/// 会话表行（`threads` / `session_bindings`），与远端会话在本机的处境逐条相同，从而可以在
/// 离线环境里证明执行面只按数据面给出的事实判定。
struct DoubleDbFixture {
    facade: Arc<SessionResourcesImpl>,
    repo: TempDir,
    /// 数据面所在的库（远端 store 的等价物）。
    data: LocalExecution,
    /// 本机执行面所在的库（workspace 登记、执行代际、sidecar 锁）。
    local: LocalExecution,
    _dirs: (TempDir, TempDir),
}

impl DoubleDbFixture {
    async fn new() -> Self {
        let repo = repository();
        let data_dir = tempfile::tempdir().unwrap();
        let local_dir = tempfile::tempdir().unwrap();
        let data = LocalExecution::open(data_dir.path().join("remote.db"))
            .await
            .unwrap();
        let local = LocalExecution::open(local_dir.path().join("threads.db"))
            .await
            .unwrap();
        let facade = Arc::new(SessionResourcesImpl::from_ports(
            Arc::new(data.data_port()),
            Arc::new(local.clone()),
            SessionDataHome::RemoteStore,
        ));
        Self {
            facade,
            repo,
            data,
            local,
            _dirs: (data_dir, local_dir),
        }
    }

    /// 工作区登记是**本机**执行事实：解析只落在本机库里。
    ///
    /// 数据面库需要同一份登记，只是因为这里用本机 adapter 顶替远端 store 而远端 store 根本
    /// 没有登记表（绑定直接落在会话行上）：复制的是**同一份**登记（同 id、同快照字节），
    /// 不是第二份证据。这样两边的绑定指向同一个 workspace，测试打的仍然是「数据面在别的库」
    /// 这件事本身。
    async fn workspace(&self) -> ResolvedWorkspace {
        let workspace = self
            .facade
            .resolve_workspace(self.repo.path())
            .await
            .unwrap();
        self.mirror_registration(&workspace).await;
        workspace
    }

    /// 把本机库里的登记原样复制到数据面库（见 [`Self::workspace`]）。
    ///
    /// 两个库是两条独立连接，不能跨库 `INSERT ... SELECT`，因此逐行读出再写入。
    async fn mirror_registration(&self, workspace: &ResolvedWorkspace) {
        let project: (String, String, String) =
            sqlx::query_as("SELECT id, locator, object_identity FROM projects WHERE id = ?1")
                .bind(workspace.project_id.to_string())
                .fetch_one(self.local.pool())
                .await
                .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO projects (id, locator, object_identity) VALUES (?1, ?2, ?3)",
        )
        .bind(&project.0)
        .bind(&project.1)
        .bind(&project.2)
        .execute(self.data.pool())
        .await
        .unwrap();
        let row: (String, String, String, String, String) = sqlx::query_as(
            "SELECT id, project_id, root, root_identity, discovery FROM workspaces WHERE id = ?1 AND project_id = ?2",
        )
        .bind(workspace.workspace_id.to_string())
        .bind(workspace.project_id.to_string())
        .fetch_one(self.local.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO workspaces (id, project_id, root, root_identity, discovery)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&row.0)
        .bind(&row.1)
        .bind(&row.2)
        .bind(&row.3)
        .bind(&row.4)
        .execute(self.data.pool())
        .await
        .unwrap();
    }

    fn binding(workspace: &ResolvedWorkspace) -> SessionBinding {
        SessionBinding {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: workspace.project_id,
            workspace_id: workspace.workspace_id,
            cwd_relative_to_workspace: workspace.relative_cwd.clone(),
        }
    }

    fn session(&self, id: &str, workspace: &ResolvedWorkspace, parent: Option<&str>) -> NewSession {
        NewSession {
            thread_id: id.to_owned(),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: Some(format!("session {id}")),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: parent.map(str::to_owned),
                hidden: parent.is_some(),
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: Self::binding(workspace),
            frozen: FrozenSnapshotBytes::new(format!(r#"{{"v":1,"id":"{id}"}}"#)),
        }
    }

    /// 远程组合的创建：数据面 durable 保存 + 本机执行准入两步。
    async fn create(
        &self,
        id: &str,
        workspace: &ResolvedWorkspace,
    ) -> Arc<dyn SessionExecutionLease> {
        self.facade
            .create_session(&self.session(id, workspace, None))
            .await
            .unwrap()
    }

    /// child：数据面写继承区与父子关系，沿用 root owner（child 自己没有执行代际）。
    async fn save_child(
        &self,
        child: &str,
        root: &str,
        workspace: &ResolvedWorkspace,
        root_lease: &Arc<dyn SessionExecutionLease>,
    ) {
        let root_frozen = match self
            .facade
            .load_session_snapshot(&root.to_owned())
            .await
            .unwrap()
            .frozen
        {
            FrozenState::Present(frozen) => frozen,
            other => panic!("root frozen must be present: {other:?}"),
        };
        let mut target = self.session(child, workspace, Some(root));
        target.frozen = root_frozen;
        self.facade
            .save_child(
                &ChildSnapshot {
                    target,
                    parent_id: root.to_owned(),
                    root_id: root.to_owned(),
                    inherited: peri_acp_types::store::InheritedContext {
                        payloads: Vec::new(),
                        flags: std::collections::HashMap::new(),
                    },
                },
                root_lease,
            )
            .await
            .unwrap();
    }

    /// 数据面上只有会话行、没有绑定行的历史会话（远端 store 里的 legacy 历史）。
    async fn save_bindingless_session(&self, id: &str, workspace: &ResolvedWorkspace) {
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, agent_status)
             VALUES (?1, ?2, ?3, ?4, ?4, 0, 'active')",
        )
        .bind(id)
        .bind(format!("legacy {id}"))
        .bind(workspace.cwd.to_string_lossy().into_owned())
        .bind("2026-09-26T00:00:00Z")
        .execute(self.data.pool())
        .await
        .unwrap();
    }

    /// 本机库里恰好有一条同 id 的行（cwd 落在已登记工作区内）：远端会话不能被它冒充。
    async fn save_local_lookalike_row(&self, id: &str, workspace: &ResolvedWorkspace) {
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, agent_status)
             VALUES (?1, ?2, ?3, ?4, ?4, 0, 'active')",
        )
        .bind(id)
        .bind(format!("lookalike {id}"))
        .bind(workspace.cwd.to_string_lossy().into_owned())
        .bind("2026-09-26T00:00:00Z")
        .execute(self.local.pool())
        .await
        .unwrap();
    }

    async fn count(pool: &sqlx::SqlitePool, sql: &'static str, id: &str) -> i64 {
        let row: (i64,) = sqlx::query_as(sql).bind(id).fetch_one(pool).await.unwrap();
        row.0
    }

    /// 本机会话表行数：远端会话在本机必须一行都没有。
    async fn local_session_rows(&self, id: &str) -> (i64, i64) {
        (
            Self::count(
                self.local.pool(),
                "SELECT COUNT(*) FROM threads WHERE id = ?1",
                id,
            )
            .await,
            Self::count(
                self.local.pool(),
                "SELECT COUNT(*) FROM session_bindings WHERE thread_id = ?1",
                id,
            )
            .await,
        )
    }

    async fn local_execution_row(&self, id: &str) -> Option<(i64, bool)> {
        sqlx::query_as("SELECT generation, clean FROM execution_runs WHERE thread_id = ?1")
            .bind(id)
            .fetch_optional(self.local.pool())
            .await
            .unwrap()
    }

    async fn data_rows(&self, id: &str) -> (i64, i64, i64) {
        (
            Self::count(
                self.data.pool(),
                "SELECT COUNT(*) FROM threads WHERE id = ?1",
                id,
            )
            .await,
            Self::count(
                self.data.pool(),
                "SELECT COUNT(*) FROM session_bindings WHERE thread_id = ?1",
                id,
            )
            .await,
            Self::count(
                self.data.pool(),
                "SELECT COUNT(*) FROM messages WHERE thread_id = ?1",
                id,
            )
            .await,
        )
    }

    async fn availability(&self, id: &str) -> Option<ExecutionAvailability> {
        self.facade
            .inspect_availability(Some(&id.to_owned()))
            .await
            .unwrap()
            .execution
    }
}

/// 数据面在**别的**库时，冷恢复必须成立：绑定与树根由数据面回答，本机执行面不去本机会话表
/// 找它们。修复前这条路径在 `acquire_execution` 处按 `Workspace(BindingMissing)` 失败。
#[tokio::test]
async fn test_double_db_cold_recovery_acquires_execution_from_data_plane_facts() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-cold-root".to_owned();

    // ① 远程组合的创建是两步：数据面 durable 保存，本机执行面再建立准入（数据与代际不在
    //    同一个库，因此没有「一次提交」可塌缩）。
    let lease = fixture.create(&id, &workspace).await;
    assert_eq!(lease.thread_id(), &id);
    // 本机只留执行事实：没有会话行、没有绑定行、没有历史。
    assert_eq!(fixture.local_session_rows(&id).await, (0, 0));
    assert_eq!(fixture.local_execution_row(&id).await, Some((1, false)));
    // 数据面有完整数据（会话行 + 绑定行）。
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 0));
    // 有主不是「需要恢复」。
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::OwnedElsewhere)
    );
    // 活 owner 上仍可正常写入：门禁挂在数据面给出的 root 上。
    fixture
        .facade
        .append_history(&id, &[payload("first turn")])
        .await
        .unwrap();
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 1));

    // ② 冷进程等价物：上一个进程退出而没有写 clean，本机剩下的只有未结清的代际。
    drop(lease);
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::Dirty(RecoveryRequiredDetails {
            thread_id: id.clone(),
            generation: 1,
        }))
    );
    fixture
        .facade
        .reset_dirty_execution(&ResetDirtyRequest {
            target: RecoveryRequiredDetails {
                thread_id: id.clone(),
                generation: 1,
            },
            accept_risk: true,
        })
        .await
        .unwrap();

    // ③ 取得所有权：绑定复核用数据面的字节，root-only 判定用数据面给出的树根。
    let recovered = fixture
        .facade
        .acquire_execution(&id, &workspace)
        .await
        .unwrap();
    assert_eq!(recovered.thread_id(), &id);
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::OwnedElsewhere)
    );
    // ④ 返回的租约可用：clean 落在**本机**执行代际上（数据面不写执行事实）。
    recovered.mark_clean().await.unwrap();
    assert_eq!(fixture.local_execution_row(&id).await, Some((2, true)));
    assert_eq!(fixture.local_session_rows(&id).await, (0, 0));
}

/// child 的写入归属 root 执行域：数据面在别的库时它必须仍然拿到**真**门禁，且 owner 关闭后
/// 按既有语义被拒绝。修复前 `bound` 恒为 false ⇒ `WriteScope::Concurrent(None)` ⇒ 静默放行。
#[tokio::test]
async fn test_double_db_child_mutation_holds_the_root_owner_and_fails_without_it() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let root = "r-owner-root".to_owned();
    let child = "r-owner-child".to_owned();
    let root_lease = fixture.create(&root, &workspace).await;
    fixture
        .save_child(&child, &root, &workspace, &root_lease)
        .await;
    // child 在本机同样一行都没有：它的写入归属只能由数据面回答（root 在远端父链上）。
    assert_eq!(fixture.local_session_rows(&child).await, (0, 0));
    assert_eq!(fixture.local_execution_row(&child).await, None);

    // 有活 owner：child 的 mutation 落在 root 的门禁上，而不是无门禁放行。
    let root_facts = facts_of(&fixture.facade, &root).await;
    let held = fixture
        .facade
        .gate
        .local()
        .exclusive_guard(&root, &root_facts)
        .await
        .unwrap()
        .expect("the root owner is alive");
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            fixture.facade.append_history(&child, &[payload("blocked")]),
        )
        .await
        .is_err(),
        "a child mutation must wait for the root's write gate"
    );
    assert_eq!(fixture.data_rows(&child).await.2, 0);
    held.finish();
    fixture
        .facade
        .append_history(&child, &[payload("child turn")])
        .await
        .unwrap();
    assert_eq!(fixture.data_rows(&child).await.2, 1);

    // owner 关闭（clean 已落地但本进程仍看得见这条租约）后，同一调用按既有语义失败。
    root_lease.mark_clean().await.unwrap();
    let error = fixture
        .facade
        .append_history(&child, &[payload("after clean")])
        .await
        .unwrap_err();
    assert!(matches!(
        error_kind(&error),
        SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
    ));
    // 租约消失（进程结束等价）后同样被拒绝：有绑定而没有 owner 不是「无主」，不能免授权。
    drop(root_lease);
    for id in [&child, &root] {
        let error = fixture
            .facade
            .append_history(id, &[payload("after drop")])
            .await
            .unwrap_err();
        assert!(
            matches!(
                error_kind(&error),
                SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionLeaseRequired)
            ),
            "{id}: expected ExecutionLeaseRequired, got {error:?}"
        );
    }
    // 被拒的写入没有留下效果。
    assert_eq!(fixture.data_rows(&child).await.2, 1);
}

/// `execution_availability` 在「有活 owner / ordinary dirty / 绑定缺失」三种情形下的结论与
/// 本机组合一致；「绑定缺失」也不会被本机恰好存在的同 id 行冒充成 legacy。
#[tokio::test]
async fn test_double_db_execution_availability_matches_the_local_verdicts() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;

    // 有活 owner：有主不是「需要恢复」。
    let owned = "r-avail-owned".to_owned();
    let lease = fixture.create(&owned, &workspace).await;
    assert_eq!(
        fixture.availability(&owned).await,
        Some(ExecutionAvailability::OwnedElsewhere)
    );

    // ordinary dirty：owner 消失后剩下的只有精确代际的未结清事实。
    let dirty = "r-avail-dirty".to_owned();
    let dirty_lease = fixture.create(&dirty, &workspace).await;
    drop(dirty_lease);
    assert_eq!(
        fixture.availability(&dirty).await,
        Some(ExecutionAvailability::Dirty(RecoveryRequiredDetails {
            thread_id: dirty.clone(),
            generation: 1,
        }))
    );

    // 绑定缺失：数据面上只有会话行（远端 store 里的历史会话），本机没有任何执行事实。
    let missing = "r-avail-missing".to_owned();
    fixture.save_bindingless_session(&missing, &workspace).await;
    assert_eq!(
        fixture.availability(&missing).await,
        Some(ExecutionAvailability::BindingMissing)
    );
    // 本机恰好有一条同 id、cwd 落在已登记工作区里的无绑定行：那是**本机** legacy 的来源证据，
    // 远端会话不看它（否则远端历史会被判成 legacy）。
    assert_eq!(
        fixture.facade.load_session_binding(&missing).await.unwrap(),
        BindingState::Missing
    );
    fixture.save_local_lookalike_row(&missing, &workspace).await;
    assert_eq!(
        fixture.facade.load_session_binding(&missing).await.unwrap(),
        BindingState::Missing
    );
    assert_eq!(
        fixture.availability(&missing).await,
        Some(ExecutionAvailability::BindingMissing)
    );

    drop(lease);
}
