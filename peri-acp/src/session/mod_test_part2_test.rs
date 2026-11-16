#[tokio::test]
async fn test_effective_host_caps_requires_external_negotiation_but_preserves_internal_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    assert!(
        mgr.effective_host_caps().oauth,
        "未 initialize 的进程内 TUI 路径保持 all_enabled"
    );
    mgr.set_pending_caps(peri_acp_types::PeriCaps::default());
    assert!(
        !mgr.effective_host_caps().oauth,
        "外部 initialize 未声明 peri.oauth 时必须关闭"
    );
    mgr.set_pending_caps(peri_acp_types::PeriCaps {
        oauth: true,
        ..Default::default()
    });
    assert!(mgr.effective_host_caps().oauth);
}

// ── mcp_subscription_for（2026-07-28 subscriptions/listen 订阅 inbox 注册）───

/// mcp_subscription_for：首次调用惰性注册，重复调用幂等（只注册一次、不 panic）。
#[tokio::test]
async fn test_mcp_subscription_for_幂等注册() {
    let tmp = tempfile::TempDir::new().unwrap();
    let port = Arc::new(FakeMcpSubscriptionPort::default());
    let mgr = make_manager_with_mcp_subscription(
        &tmp,
        Some(port.clone() as Arc<dyn peri_acp_types::mcp::McpSubscriptionPort>),
    )
    .await;
    let session_id = "test-mcp-sub-idempotent";
    mgr.ensure_session(session_id, "/tmp");

    assert!(
        mgr.mcp_subscription_for(session_id),
        "session 存在时首次调用应返回 true"
    );
    assert_eq!(port.inbox_count(), 1, "首次调用应注册一个 inbox");
    assert!(
        mgr.mcp_subscription_for(session_id),
        "重复调用应仍返回 true（不 panic）"
    );
    assert_eq!(
        port.inbox_count(),
        1,
        "重复调用不得重复注册（insert 幂等，注册表条目不增长）"
    );
    assert!(port.has_inbox(session_id), "inbox 应保留在注册表中");
    assert!(port.unregistered().is_empty(), "注册后未 close 前不应注销");
}

/// mcp_subscription_for：session 不存在时返回 false（不注册、不 panic）。
#[tokio::test]
async fn test_mcp_subscription_for_session不存在返回false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let port = Arc::new(FakeMcpSubscriptionPort::default());
    let mgr = make_manager_with_mcp_subscription(
        &tmp,
        Some(port.clone() as Arc<dyn peri_acp_types::mcp::McpSubscriptionPort>),
    )
    .await;
    assert!(!mgr.mcp_subscription_for("non-existent"));
    assert_eq!(port.inbox_count(), 0, "session 不存在时不得注册");
}

/// mcp_subscription_for：close_session 后返回 false，且端口收到 unregister_inbox。
#[tokio::test]
async fn test_mcp_subscription_for_close_session后返回false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let port = Arc::new(FakeMcpSubscriptionPort::default());
    let mgr = make_manager_with_mcp_subscription(
        &tmp,
        Some(port.clone() as Arc<dyn peri_acp_types::mcp::McpSubscriptionPort>),
    )
    .await;
    let session_id = "test-mcp-sub-close";
    mgr.ensure_session(session_id, "/tmp");
    assert!(mgr.mcp_subscription_for(session_id));

    mgr.close_session(session_id).await.unwrap();
    assert!(
        !mgr.mcp_subscription_for(session_id),
        "close_session 后应返回 false"
    );
    assert_eq!(
        port.unregistered(),
        vec![session_id.to_string()],
        "close_session 必须注销 inbox（通知不再唤醒已关闭的会话）"
    );
    assert!(
        !port.has_inbox(session_id),
        "注销后注册表中不得残留该 session 条目"
    );
}

/// mcp_subscription_for：未注入端口时返回 false（不 panic）。
#[tokio::test]
async fn test_mcp_subscription_for未注入端口返回false() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    let session_id = "test-mcp-sub-no-port";
    mgr.ensure_session(session_id, "/tmp");
    assert!(
        !mgr.mcp_subscription_for(session_id),
        "未配置端口时应返回 false"
    );
}

/// MCP skill registry 生命周期（验收 14 半边）：ensure_session 后投影同 Arc、
/// 各 session 隔离；close_session 并把 manager/句柄 drop 干净后 Weak 升级失败
/// （registry 随 session 释放，无全局挂点）。
#[tokio::test]
async fn test_mcp_skill_registry_lifecycle_released_on_close() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    let session_id = "test-registry-lifecycle";
    mgr.ensure_session(session_id, "/tmp");

    let reg = mgr
        .mcp_skill_registry_for(session_id)
        .expect("ensure_session 后应能取到 registry Arc");
    // 同一 session 重复投影必须返回同一底层 registry（每轮透传语义）
    assert!(
        Arc::ptr_eq(
            &reg,
            &mgr.mcp_skill_registry_for(session_id)
                .expect("重复投影仍应命中")
        ),
        "同 session 每轮投影同一 registry"
    );

    // 不同 session 各自独立 registry（session 级隔离）
    mgr.ensure_session("other-registry-session", "/tmp");
    assert!(
        !Arc::ptr_eq(
            &reg,
            &mgr.mcp_skill_registry_for("other-registry-session")
                .expect("另一 session 应有自己的 registry")
        ),
        "不同 session 不得共享同一 registry"
    );

    let weak = Arc::downgrade(&reg);

    // close_session + 把 manager 与句柄 drop 干净（session 对象不得被测试变量
    // 继续持有）后，registry 必须释放
    mgr.close_session(session_id).await.unwrap();
    drop(reg);
    drop(mgr);

    assert!(
        weak.upgrade().is_none(),
        "close_session 后 registry Arc 必须释放（无全局挂点）"
    );
}

#[path = "mod_meta_harness_test.rs"]
mod meta_harness_tests;

/// 占位 handler：测试只断言路由层（注册 / 解析 / 投影），不触发执行。
struct TestHandler;

#[async_trait]
impl CommandHandler for TestHandler {
    async fn execute(&self, _ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
        CommandOutcome::Inject(String::new())
    }
}

// ─── Phase 6 B2/F6：会话创建命令面（内置 + 插件；技能命令由 MCP 发现投影）──

/// 写入本地 skill fixture：`{cwd}/.claude/skills/{dir}/SKILL.md`
///
/// W4b（F6/J5）后它只作为**反例**存在：宿主命令面不再扫盘，`.claude/skills`
/// 由一个 MCP 通道（builtin `workspace` 实例）提供，`core:{skill}` 裸名命令由
/// 发现完成后的 registry 投影写入（`peri-middlewares/src/mcp/skill_discovery.rs`
/// 的 `project_core_skill_commands`）。
fn write_local_skill(cwd: &std::path::Path, dir: &str, skill_name: &str) {
    let dir_path = cwd.join(".claude").join("skills").join(dir);
    std::fs::create_dir_all(&dir_path).unwrap();
    std::fs::write(
        dir_path.join("SKILL.md"),
        format!("---\nname: \"{skill_name}\"\ndescription: \"test skill {skill_name}\"\n---\nBody"),
    )
    .unwrap();
}

/// F6：会话构造**不**扫盘注册技能命令——磁盘上有 SKILL.md 也不产生 `core:hello`。
///
/// 技能命令面改由 MCP 发现异步投影（本用例锁死「构造期零技能 FS 读取」这一半；
/// 投影正例见 `peri-middlewares` 的 `skill_discovery` 用例与
/// `requests_skill_resources_test.rs` 的端到端用例）。
#[tokio::test]
async fn test_session_creation_does_not_scan_disk_for_skill_commands() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_local_skill(tmp.path(), "hello", "hello");
    let mgr = make_session_manager(&tmp).await;
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    assert!(
        reg.resolve("/hello").is_none(),
        "构造期不得从磁盘注册裸名技能命令"
    );
    assert!(
        reg.resolve("/core:hello").is_none(),
        "构造期不得从磁盘注册 core 域技能命令"
    );
    // 内置命令照常注册（命令面本身可用）。
    assert_eq!(
        reg.resolve("/compact").unwrap().entry.kind,
        CommandEntryKind::Command
    );
}

/// B2 注册顺序：内置 → 插件（先注册者占键）；技能命令由发现面后写，
/// core 域同名冲突在内置侧纯拒绝。
#[tokio::test]
async fn test_session_creation_register_order_builtin_then_plugin() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager_with_plugin_entries(
        &tmp,
        vec![RouteEntry {
            fullname: "plugin:ecc:deploy".into(),
            aliases: vec![],
            description: "deploy".into(),
            kind: CommandEntryKind::Command,
            category: None,
            args_schema: None,
            handler: Arc::new(TestHandler),
            provenance: CommandProvenance {
                source: CommandSource::Plugin { name: "ecc".into() },
                lifecycle: CommandLifecycle::Connected,
            },
        }],
    )
    .await;
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    // 内置（core:compact）与插件（plugin:ecc:deploy）共存。
    assert_eq!(
        reg.resolve("/compact").unwrap().entry.kind,
        CommandEntryKind::Command
    );
    assert_eq!(
        reg.resolve("/plugin:ecc:deploy").unwrap().entry.kind,
        CommandEntryKind::Command
    );
}

/// F6 冲突裁决（registry 层，不依赖磁盘）：内置 compact 先注册 → 同名
/// `core:compact` 技能条目被纯拒绝，注册表保持内置条目（不覆盖、不静默）。
#[tokio::test]
async fn test_builtin_command_wins_over_same_name_skill_projection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());
    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");

    // 发现面投影形态：`core:{skill}` + kind = Skill + source = Core
    let (removed, added) = reg.reconcile(
        &[],
        vec![RouteEntry {
            fullname: "core:compact".into(),
            aliases: vec![],
            description: "same-name skill".into(),
            kind: CommandEntryKind::Skill,
            category: None,
            args_schema: None,
            handler: Arc::new(TestHandler),
            provenance: CommandProvenance {
                source: CommandSource::Core,
                lifecycle: CommandLifecycle::Connected,
            },
        }],
    );
    assert_eq!(
        (removed, added),
        (0, 0),
        "同名技能条目必须被拒（不覆盖内置）"
    );

    let snap = reg.snapshot();
    let compact = snap
        .iter()
        .find(|e| e.fullname == "core:compact")
        .expect("内置 core:compact 存在");
    assert_eq!(
        compact.kind,
        CommandEntryKind::Command,
        "注册表保持内置条目"
    );
    assert!(
        !snap
            .iter()
            .any(|e| e.fullname == "core:compact" && e.kind == CommandEntryKind::Skill),
        "Skill 形态的 core:compact 不得存在（不覆盖）"
    );
    let resolved = reg.resolve("/compact").expect("裸名命中内置");
    assert_eq!(resolved.entry.kind, CommandEntryKind::Command);
}

// ── AW3-11：per-session TaskManager 的携带路径 ────────────────────────────────

/// `ensure_session_with_task_manager` 携带的 manager 必须**原样**成为会话持有的
/// `AcpSession::task_manager`（同一 `Arc`，不是复制、不是重新工厂化）；`None` 路径
/// 仍走装配注入的工厂——两条来源在同一个 manager 上可辨识。
#[tokio::test]
async fn ensure_session_with_task_manager_keeps_the_same_arc() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager_with_task_manager_factory(&tmp).await;
    let carried: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    mgr.ensure_session_with_task_manager("aw3-11-carried", "/tmp", Some(Arc::clone(&carried)));
    let stored = Arc::clone(&mgr.get_session("aw3-11-carried").unwrap().task_manager);
    assert!(
        Arc::ptr_eq(&stored, &carried),
        "会话持有的必须是送进来的同一份 Arc"
    );
    assert!(
        !stored
            .as_any()
            .is::<peri_acp_types::tasks::NoopTaskManager>(),
        "携带路径不得退化成 Noop"
    );

    // 既有签名（delegate 到 `None`）不变：走工厂，与携带的那份非同一实例。
    mgr.ensure_session("aw3-11-factory", "/tmp");
    let from_factory = Arc::clone(&mgr.get_session("aw3-11-factory").unwrap().task_manager);
    assert!(
        !Arc::ptr_eq(&from_factory, &carried),
        "None 路径必须走工厂，不得复用外部携带的 manager"
    );
    assert!(
        !from_factory
            .as_any()
            .is::<peri_acp_types::tasks::NoopTaskManager>(),
        "工厂已注入 ⇒ 不得 fallback NoopTaskManager"
    );
}

/// 未注入工厂时 `None` 路径仍是既有的 `NoopTaskManager` fallback（行为不变）。
#[tokio::test]
async fn ensure_session_without_factory_still_falls_back_to_noop() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    mgr.ensure_session("aw3-11-noop", "/tmp");
    assert!(mgr
        .get_session("aw3-11-noop")
        .unwrap()
        .task_manager
        .as_any()
        .is::<peri_acp_types::tasks::NoopTaskManager>());
}

// ── 优雅退出：会话关闭必须结算 dormant child 资源 ────────────────────────────

/// 记录 `close_child_resources` 调用并按需注入失败的最小 `TaskManager`。
struct CloseTrackingTaskManager {
    close_calls: std::sync::atomic::AtomicUsize,
    close_failure: Option<String>,
}

impl CloseTrackingTaskManager {
    fn new(close_failure: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            close_calls: std::sync::atomic::AtomicUsize::new(0),
            close_failure: close_failure.map(str::to_owned),
        })
    }

    fn close_calls(&self) -> usize {
        self.close_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl peri_acp_types::tasks::TaskManager for CloseTrackingTaskManager {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn active_count(&self) -> usize {
        0
    }

    fn register(
        &self,
        _request: peri_acp_types::tasks::BgTaskRegistration,
    ) -> Result<peri_acp_types::tasks::TaskRegistrationTicket, String> {
        Err("test manager does not register tasks".into())
    }

    fn complete(
        &self,
        _ticket: &peri_acp_types::tasks::TaskRegistrationTicket,
        _result: peri_acp_types::event::BackgroundTaskResult,
    ) -> bool {
        false
    }

    fn settle_completed(
        &self,
        _ticket: &peri_acp_types::tasks::TaskRegistrationTicket,
        _result: peri_acp_types::event::BackgroundTaskResult,
        _delivery: peri_acp_types::tasks::OnBgCompleteFn,
    ) -> Result<bool, String> {
        Err("test manager does not deliver results".into())
    }

    fn set_event_sender(
        &self,
        _sender: tokio::sync::mpsc::UnboundedSender<peri_acp_types::tasks::BgRegistryEvent>,
        _session_id: String,
    ) {
    }

    fn retry_pending_deliveries(&self) -> usize {
        0
    }

    fn cancel(&self, _task_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn cancel_all(&self) {}

    fn shutdown(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = peri_acp_types::tasks::TaskShutdownReport> + Send + '_,
        >,
    > {
        Box::pin(async { peri_acp_types::tasks::TaskShutdownReport::Complete })
    }

    fn close_child_resources(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + '_>> {
        self.close_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let failure = self.close_failure.clone();
        Box::pin(async move { failure.map_or(Ok(()), Err) })
    }

    fn spawn_shell(
        &self,
        _command: String,
        _cwd: String,
        _timeout_ms: Option<u64>,
        _on_bg_complete: Option<peri_acp_types::tasks::OnBgCompleteFn>,
    ) -> Result<peri_acp_types::tasks::BgShellHandle, Box<dyn std::error::Error + Send + Sync>>
    {
        Err("test manager does not spawn shells".into())
    }

    fn finalize_bg_shell(
        &self,
        _on_bg_complete: Option<&peri_acp_types::tasks::OnBgCompleteFn>,
        _ticket: peri_acp_types::tasks::TaskRegistrationTicket,
        _prompt_summary: String,
        _success: bool,
        _output: String,
        _duration_ms: u64,
        _timed_out: bool,
        _shell_output: Option<peri_acp_types::event::ShellOutput>,
    ) {
    }
}

/// [回归] 优雅退出路径必须走既有 `close_child_resources`：关闭会话时调用一次，
/// 且不阻塞、不影响其他会话记录。
#[tokio::test]
async fn close_session_settles_dormant_child_resources_without_affecting_other_sessions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    let tracking = CloseTrackingTaskManager::new(None);
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> = tracking.clone();
    mgr.ensure_session_with_task_manager(
        "dormant-close",
        tmp.path().to_str().unwrap(),
        Some(manager),
    );
    mgr.ensure_session("bystander-root", tmp.path().to_str().unwrap());

    tokio::time::timeout(Duration::from_secs(5), mgr.close_session("dormant-close"))
        .await
        .expect("close must not hang")
        .unwrap();

    assert_eq!(
        tracking.close_calls(),
        1,
        "会话关闭必须先走 dormant child 资源结算"
    );
    assert!(
        mgr.get_session("bystander-root").is_some(),
        "关闭一个会话不得影响其他根会话"
    );
}

/// [回归] child 结算失败必须上报并保留会话记录供退出重试，而不是静默成功。
#[tokio::test]
async fn close_session_reports_dormant_child_close_failure_and_retains_record() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    let tracking =
        CloseTrackingTaskManager::new(Some("Incomplete: dormant child MQ settlement failed"));
    let manager: Arc<dyn peri_acp_types::tasks::TaskManager> = tracking.clone();
    mgr.ensure_session_with_task_manager(
        "dormant-close-failure",
        tmp.path().to_str().unwrap(),
        Some(manager),
    );

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        mgr.close_session("dormant-close-failure"),
    )
    .await
    .expect("close must not hang")
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("owned resources are still active"),
        "关闭失败必须作为 Incomplete 上报: {error}"
    );
    assert_eq!(tracking.close_calls(), 1);
    assert!(
        mgr.get_session("dormant-close-failure").is_some(),
        "失败后必须保留会话记录以便退出重试"
    );

    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        mgr.close_session("dormant-close-failure"),
    )
    .await
    .expect("retry must not hang");
    assert_eq!(
        tracking.close_calls(),
        2,
        "重试关闭必须再次执行真实结算检查"
    );
}
