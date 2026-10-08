use super::*;

#[tokio::test]
async fn terminal_reminder_replay_has_one_canonical_history_entry() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };

    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-reminder-replay".to_owned();
    fixture.create(&id, &workspace).await;
    let make_reminder = |body: &str| {
        TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Task,
                source: ReminderSource("task_manager".into()),
                kind: "terminal".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Required,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: body.into(),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap()
    };
    let message_id = MessageId::new();
    assert!(fixture
        .facade
        .append_reminder_if_absent(&id, message_id, &make_reminder("done"))
        .await
        .unwrap());
    assert!(!fixture
        .facade
        .append_reminder_if_absent(&id, message_id, &make_reminder("done"))
        .await
        .unwrap());
    assert!(fixture
        .facade
        .append_reminder_if_absent(&id, message_id, &make_reminder("different"))
        .await
        .is_err());
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 1));
}

#[tokio::test]
async fn explicit_close_intent_survives_resource_reopen() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-explicit-close".to_owned();
    fixture.create(&id, &workspace).await;
    assert!(!fixture.facade.is_session_closing(&id).await.unwrap());
    fixture.facade.mark_session_closing(&id).await.unwrap();
    fixture.facade.mark_session_closing(&id).await.unwrap();
    let reopened_local = LocalExecution::open(fixture._dirs.1.path().join("threads.db"))
        .await
        .unwrap();
    let reopened = SessionResourcesImpl::from_ports(
        Arc::new(fixture.data.data_port()),
        Arc::new(reopened_local),
        SessionDataHome::RemoteStore,
    );
    assert!(reopened.is_session_closing(&id).await.unwrap());
    reopened.finish_close(&id).await.unwrap();
    assert!(!reopened.is_session_closing(&id).await.unwrap());
    assert!(!fixture.facade.is_session_closing(&id).await.unwrap());
}

#[tokio::test]
async fn close_intent_finishes_by_session_identity() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-close-after-drain".to_owned();
    fixture.create(&id, &workspace).await;
    fixture.facade.mark_session_closing(&id).await.unwrap();
    fixture.facade.drain_persistence(&id).await.unwrap();
    fixture.facade.finish_close(&id).await.unwrap();
    assert!(!fixture.facade.is_session_closing(&id).await.unwrap());
    assert_eq!(
        fixture.facade.close_settlement(&id).await.unwrap(),
        CloseSettlement::Finished
    );
}

#[tokio::test]
async fn test_double_db_reopen_preserves_data_plane_snapshot() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-cold-root".to_owned();

    fixture.create(&id, &workspace).await;
    // 本机只留执行事实：没有会话行、没有绑定行、没有历史。
    assert_eq!(fixture.local_session_rows(&id).await, (0, 0));
    // 数据面有完整数据（会话行 + 绑定行）。
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 0));
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::Available)
    );
    fixture
        .facade
        .append_history(&id, &[payload("first turn")])
        .await
        .unwrap();
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 1));

    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::Available)
    );

    let reopened_local = LocalExecution::open(fixture._dirs.1.path().join("threads.db"))
        .await
        .unwrap();
    let reopened = SessionResourcesImpl::from_ports(
        Arc::new(fixture.data.data_port()),
        Arc::new(reopened_local),
        SessionDataHome::RemoteStore,
    );
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::Available)
    );
    let after = reopened.load_session_snapshot(&id).await.unwrap();
    assert_eq!(after.binding, before.binding);
    assert_eq!(after.frozen, before.frozen);
    assert_eq!(after.payloads.len(), before.payloads.len());
    reopened
        .append_history(&id, &[payload("reopened instance")])
        .await
        .unwrap();
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 2));
    assert_eq!(fixture.local_session_rows(&id).await, (0, 0));
}
/// 按既有语义被拒绝。修复前 `bound` 恒为 false ⇒ `WriteScope::Concurrent(None)` ⇒ 静默放行。
#[tokio::test]
async fn test_double_db_execution_availability_matches_the_local_verdicts() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let owned = "r-avail-owned".to_owned();
    fixture.create(&owned, &workspace).await;
    assert_eq!(
        fixture.availability(&owned).await,
        Some(ExecutionAvailability::Available)
    );

    let dirty = "r-avail-dirty".to_owned();
    fixture.create(&dirty, &workspace).await;
    assert_eq!(
        fixture.availability(&dirty).await,
        Some(ExecutionAvailability::Available)
    );

    // 绑定缺失：数据面上只有会话行（远端 store 里的历史会话），本机没有任何执行事实。
    let missing = "r-avail-missing".to_owned();
    fixture.save_bindingless_session(&missing, &workspace).await;
    assert_eq!(
        fixture.availability(&missing).await,
        Some(ExecutionAvailability::WorkspaceUnavailable)
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
        Some(ExecutionAvailability::WorkspaceUnavailable)
    );
}

struct DoubleDbFixture {
    facade: Arc<SessionResourcesImpl>,
    repo: TempDir,
    /// 数据面所在的库（远端 store 的等价物）。
    data: LocalExecution,
    /// 本机执行面所在的库，仅持久保存 workspace 登记。
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

    /// 工作区归属是**本机**执行事实：解析只落在本机库里。
    ///
    /// 数据面库需要同一份归属行，只是因为这里用本机 adapter 顶替远端 store 而远端 store
    /// 不共享本机库的事实：复制的是**同一行**（同 id、同证据字节）与它引用的项目，
    /// 不是第二份证据。这样两边的绑定指向同一个 workspace，测试打的仍然是「数据面在别的库」
    /// 这件事本身。
    async fn workspace(&self) -> ResolvedWorkspace {
        let workspace = self
            .facade
            .resolve_workspace(self.repo.path())
            .await
            .unwrap();
        self.mirror_workspace(&workspace).await;
        workspace
    }

    /// 把本机库里的归属行与它引用的项目原样复制到数据面库（见 [`Self::workspace`]）。
    ///
    /// 两个库是两条独立连接，不能跨库 `INSERT ... SELECT`，因此逐行读出再写入。
    async fn mirror_workspace(&self, workspace: &ResolvedWorkspace) {
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
        let row: (
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT id, machine_id, path, path_source, project_id, identity, discovery
                 FROM workspaces WHERE id = ?1",
        )
        .bind(workspace.workspace_id.to_string())
        .fetch_one(self.local.pool())
        .await
        .unwrap();
        sqlx::query("INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')")
            .bind(&row.1)
            .execute(self.data.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO workspaces(id, machine_id, path, path_source, project_id, identity, discovery)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&row.0)
        .bind(&row.1)
        .bind(&row.2)
        .bind(&row.3)
        .bind(&row.4)
        .bind(&row.5)
        .bind(&row.6)
        .execute(self.data.pool())
        .await
        .unwrap();
    }

    fn binding(workspace: &ResolvedWorkspace) -> SessionBinding {
        SessionBinding::from_workspace(workspace)
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
    async fn create(&self, id: &str, workspace: &ResolvedWorkspace) {
        self.facade
            .create_session(&self.session(id, workspace, None))
            .await
            .unwrap()
    }
    async fn save_child(&self, child: &str, root: &str, workspace: &ResolvedWorkspace) {
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
            .save_child(&ChildSnapshot {
                target,
                parent_id: root.to_owned(),
                root_id: root.to_owned(),
                inherited: peri_acp_types::store::InheritedContext {
                    payloads: Vec::new(),
                    flags: std::collections::HashMap::new(),
                },
            })
            .await
            .unwrap();
    }

    /// 数据面上只有会话行、没有绑定行的历史会话（远端 store 里的 legacy 历史）。
    async fn save_bindingless_session(&self, id: &str, workspace: &ResolvedWorkspace) {
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, agent_status, workspace_id)
             VALUES (?1, ?2, ?3, ?4, ?4, 0, 'active', ?5)",
        )
        .bind(id)
        .bind(format!("legacy {id}"))
        .bind(workspace.cwd.to_string_lossy().into_owned())
        .bind("2026-09-26T00:00:00Z")
        .bind(workspace.workspace_id.to_string())
        .execute(self.data.pool())
        .await
        .unwrap();
    }

    /// 本机库里恰好有一条同 id 的行（cwd 落在已登记工作区内）：远端会话不能被它冒充。
    async fn save_local_lookalike_row(&self, id: &str, workspace: &ResolvedWorkspace) {
        sqlx::query(
            "INSERT INTO threads (id, title, cwd, created_at, updated_at, message_count, agent_status, workspace_id)
             VALUES (?1, ?2, ?3, ?4, ?4, 0, 'active', ?5)",
        )
        .bind(id)
        .bind(format!("lookalike {id}"))
        .bind(workspace.cwd.to_string_lossy().into_owned())
        .bind("2026-09-26T00:00:00Z")
        .bind(workspace.workspace_id.to_string())
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

#[tokio::test]
async fn test_double_db_child_mutation_uses_data_plane_root_gate() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let root = "r-root".to_owned();
    let child = "r-child".to_owned();
    fixture.create(&root, &workspace).await;
    fixture.save_child(&child, &root, &workspace).await;
    assert_eq!(fixture.local_session_rows(&child).await, (0, 0));
    drop(fixture.facade.gate.admit(&root).await.unwrap());
    assert!(fixture
        .facade
        .append_history(&child, &[payload("blocked")])
        .await
        .is_err());
    fixture
        .facade
        .recover_session_persistence(&root)
        .await
        .unwrap();
    fixture
        .facade
        .append_history(&child, &[payload("child turn")])
        .await
        .unwrap();
    assert_eq!(fixture.data_rows(&child).await, (1, 1, 1));
}

#[tokio::test]
async fn unknown_persistence_prevents_close_intent_from_finishing() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-close-unknown".to_owned();
    fixture.create(&id, &workspace).await;
    fixture.facade.mark_session_closing(&id).await.unwrap();
    assert_eq!(
        fixture.facade.close_settlement(&id).await.unwrap(),
        CloseSettlement::Pending
    );
    drop(fixture.facade.gate.admit(&id).await.unwrap());
    assert!(fixture.facade.finish_close(&id).await.is_err());
    assert_eq!(
        fixture.facade.close_settlement(&id).await.unwrap(),
        CloseSettlement::Pending
    );
    assert!(fixture.facade.is_session_closing(&id).await.unwrap());
    fixture
        .facade
        .recover_session_persistence(&id)
        .await
        .unwrap();
    fixture.facade.finish_close(&id).await.unwrap();
    assert_eq!(
        fixture.facade.close_settlement(&id).await.unwrap(),
        CloseSettlement::Finished
    );
}
