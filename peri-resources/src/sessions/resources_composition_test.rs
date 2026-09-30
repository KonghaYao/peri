use super::*;

// ─── 双库：数据面在别处（远程组合的离线等价物） ───────────────────────────────

/// 执行面事实：与门面内部（`MutationGate::session_facts`）用的是同一个取法。
///
/// 直接驱动本机执行面的测试必须按同一组事实判定：「绑定/树根由数据面回答」这条契约不能只
/// 在门面里成立，否则测试锁的会是「本机恰好查得到自己那张表」这个实现细节。
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
        Some(ExecutionAvailability::Available)
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
        Some(ExecutionAvailability::Available)
    );
    assert_eq!(fixture.local_execution_row(&id).await, Some((1, false)));

    // ③ 取得所有权：绑定复核用数据面的字节，root-only 判定用数据面给出的树根。
    let recovered = fixture
        .facade
        .acquire_execution(&id, &workspace)
        .await
        .unwrap();
    assert_eq!(recovered.thread_id(), &id);
    assert_eq!(
        fixture.availability(&id).await,
        Some(ExecutionAvailability::Available)
    );
    // ④ 返回的租约可用：clean 落在**本机**执行代际上（数据面不写执行事实）。
    recovered.mark_clean().await.unwrap();
    assert_eq!(fixture.local_execution_row(&id).await, Some((2, true)));
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
        fixture
            .facade
            .append_history(id, &[payload("after run disposal")])
            .await
            .unwrap();
    }
    assert_eq!(fixture.data_rows(&child).await.2, 2);
    assert_eq!(fixture.data_rows(&root).await.2, 1);
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
        Some(ExecutionAvailability::Available)
    );

    // ordinary dirty：owner 消失后剩下的只有精确代际的未结清事实。
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
        Some(ExecutionAvailability::Available)
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
        Some(ExecutionAvailability::Available)
    );

    drop(lease);
}
