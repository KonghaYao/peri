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
    let _lease = fixture.create(&id, &workspace).await;
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
    let lease = fixture.create(&id, &workspace).await;
    assert!(!fixture.facade.is_session_closing(&id).await.unwrap());
    fixture.facade.mark_session_closing(&id).await.unwrap();
    fixture.facade.mark_session_closing(&id).await.unwrap();
    assert!(fixture.facade.is_session_closing(&id).await.unwrap());
    let token = lease.owner_token().unwrap();
    lease.mark_clean().await.unwrap();
    fixture.facade.release_execution_owner(&token).await.unwrap();
    let reopened_local = LocalExecution::open(fixture._dirs.1.path().join("threads.db"))
        .await
        .unwrap();
    let reopened = SessionResourcesImpl::from_ports(
        Arc::new(fixture.data.data_port()),
        Arc::new(reopened_local),
        SessionDataHome::RemoteStore,
    );
    assert!(reopened.is_session_closing(&id).await.unwrap());
    let takeover = reopened.claim_closing_execution(&id, token.epoch).await.unwrap();
    let retry = reopened.claim_closing_execution(&id, takeover.owner_token().unwrap().epoch).await.unwrap();
    assert_eq!(retry.owner_token(), takeover.owner_token(), "an incomplete close reuses its exact Store generation");
    reopened.finish_close(&takeover.owner_token().unwrap()).await.unwrap();
    assert!(!reopened.is_session_closing(&id).await.unwrap());
    assert!(!fixture.facade.is_session_closing(&id).await.unwrap());
}

#[tokio::test]
async fn close_intent_and_owner_finish_atomically_after_execution_is_clean() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-close-after-clean".to_owned();
    let lease = fixture.create(&id, &workspace).await;
    fixture.facade.mark_session_closing(&id).await.unwrap();
    let token = lease.owner_token().unwrap();
    lease.mark_clean().await.unwrap();
    fixture.facade.finish_close(&token).await.unwrap();
    assert!(!fixture.facade.is_session_closing(&id).await.unwrap());
}

// ─── 双库：数据面在别处（旧双库门禁夹具） ───────────────────────────────

/// 执行面事实：与门面内部（`MutationGate::session_facts`）用的是同一个取法。
///
/// 直接驱动本机执行面的测试必须按同一组事实判定：「绑定/树根由数据面回答」这条契约不能只
/// 在门面里成立，否则测试锁的会是「本机恰好查得到自己那张表」这个实现细节。
/// 数据面与本机执行面在**两个**库里的门面：旧双库门禁夹具。
///
/// 装配点与远程组合相同（`SessionResourcesImpl::from_ports` + `SessionDataHome::RemoteStore`），
/// 只是数据面用另一个真 sqlite 顶替远端 adapter：本机执行面库因此**没有**这条会话的任何
/// 会话表行（`threads` / `session_bindings`），与远端会话与执行端口分离这一门禁形状相同，从而可以在
/// 离线环境里证明执行面只按数据面给出的事实判定。
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
            "SELECT id, project_id, root, root_identity, discovery FROM legacy_execution_registrations WHERE id = ?1 AND project_id = ?2",
        )
        .bind(workspace.execution_registration_id.to_string())
        .bind(workspace.project_id.to_string())
        .fetch_one(self.local.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT OR IGNORE INTO legacy_execution_registrations (id, project_id, root, root_identity, discovery)
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
        let owner: (String, String, String, String) = sqlx::query_as(
            "SELECT id, machine_id, path, path_source FROM workspaces WHERE id = ?1",
        )
        .bind(workspace.workspace_id.to_string())
        .fetch_one(self.local.pool())
        .await
        .unwrap();
        sqlx::query("INSERT OR IGNORE INTO machines(id, name, identity_kind) VALUES (?1, '我的电脑', 'known')")
            .bind(&owner.1).execute(self.data.pool()).await.unwrap();
        sqlx::query("INSERT OR IGNORE INTO workspaces(id, machine_id, path, path_source) VALUES (?1, ?2, ?3, ?4)")
            .bind(&owner.0).bind(&owner.1).bind(&owner.2).bind(&owner.3)
            .execute(self.data.pool()).await.unwrap();
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

    /// child：数据面写继承区与父子关系，沿用 root runtime owner。
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

/// 数据面在**别的**库时，冷恢复必须成立：绑定与树根由数据面回答，本机执行面不去本机会话表
/// 找它们。修复前这条路径在 `acquire_execution` 处按 `Workspace(BindingMissing)` 失败。
#[tokio::test]
async fn test_double_db_cold_recovery_acquires_execution_from_data_plane_facts() {
    let fixture = DoubleDbFixture::new().await;
    let workspace = fixture.workspace().await;
    let id = "r-cold-root".to_owned();

    let lease = fixture.create(&id, &workspace).await;
    assert_eq!(lease.thread_id(), &id);
    // 本机只留执行事实：没有会话行、没有绑定行、没有历史。
    assert_eq!(fixture.local_session_rows(&id).await, (0, 0));
    // 数据面有完整数据（会话行 + 绑定行）。
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 0));
    // 有主不是「需要恢复」。
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::Available)
    );
    // 活 owner 上仍可正常写入：门禁挂在数据面给出的 root 上。
    fixture
        .facade
        .append_history(&id, &[payload("first turn")])
        .await
        .unwrap();
    assert_eq!(fixture.data_rows(&id).await, (1, 1, 1));

    let before = fixture.facade.load_session_snapshot(&id).await.unwrap();
    drop(lease);
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
    expire_owner(fixture.data.pool(), &id).await;
    let recovered = reopened.acquire_execution(&id, &workspace).await.unwrap();
    assert_eq!(recovered.thread_id(), &id);
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
    recovered.mark_clean().await.unwrap();
    assert_eq!(fixture.local_session_rows(&id).await, (0, 0));
}

/// child 的写入归属 root 执行域：数据面在别的库时它必须仍然拿到**真**门禁，且 owner 关闭后
/// 按既有语义被拒绝。修复前 `bound` 恒为 false ⇒ `WriteScope::Concurrent(None)` ⇒ 静默放行。
#[tokio::test]
async fn test_double_db_child_mutation_shares_live_root_gate_and_allows_disposal() {
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
    drop(root_lease);
    assert!(fixture
        .facade
        .append_history(&child, &[payload("dropped closed owner")])
        .await
        .is_err());
    let next = fixture
        .facade
        .acquire_execution(&root, &workspace)
        .await
        .unwrap();
    for id in [&child, &root] {
        fixture
            .facade
            .append_history(id, &[payload("after run disposal")])
            .await
            .unwrap();
    }
    assert_eq!(fixture.data_rows(&child).await.2, 2);
    assert_eq!(fixture.data_rows(&root).await.2, 1);
    next.mark_clean().await.unwrap();
}

/// `execution_availability` 在「有活 owner / 丢弃调用方句柄 / 绑定缺失」三种情形下的结论与
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
        Some(ExecutionAvailability::Available)
    );

    let dirty = "r-avail-dirty".to_owned();
    let dirty_lease = fixture.create(&dirty, &workspace).await;
    drop(dirty_lease);
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

    drop(lease);
}
