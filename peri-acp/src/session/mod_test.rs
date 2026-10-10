//! SessionManager 单元测试。
//!
//! 覆盖 `ensure_session` / `goal_state_for` / `cancel_cascade_children_for` /
//! `build_frozen_data` 四个新方法，验证 TUI/stdio 三合一重构后的行为契约。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::command::command_route::{
    CommandEntryKind, CommandLifecycle, CommandProvenance, CommandSource, RouteEntry,
};
use peri_acp_types::command::{CommandHandler, CommandOutcome};

use crate::provider::{
    LlmProvider, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels,
};
use crate::session::{SessionManager, TaskManagerFactory};
use peri_middlewares::prelude::{PermissionMode, SharedPermissionMode};

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn make_provider_config(id: &str, model: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: model.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 构造测试用 SessionManager + 临时 thread store
async fn make_session_manager(tmp: &tempfile::TempDir) -> SessionManager {
    make_manager_with_cron_option(tmp, None).await
}

/// 构造带 cron scheduler 的 SessionManager（session 级 cron bridge 测试用）。
///
/// scheduler 的 primary tx 直接丢弃（同 TUI `cron_state.rs:13` 模式）——
/// 本测试路径不消费 primary trigger 通道，只验证 extra_trigger_txs（bridge）路径。
async fn make_session_manager_with_cron(
    tmp: &tempfile::TempDir,
) -> (
    SessionManager,
    Arc<parking_lot::Mutex<peri_mcp_cron::CronScheduler>>,
    tokio::sync::mpsc::UnboundedReceiver<peri_acp_types::cron::CronContinuationRequest>,
) {
    let scheduler = Arc::new(parking_lot::Mutex::new(peri_mcp_cron::CronScheduler::new(
        tokio::sync::mpsc::unbounded_channel().0,
    )));
    let manager = make_manager_with_cron_option(tmp, Some(scheduler.clone())).await;
    let (continuation_tx, continuation_rx) = tokio::sync::mpsc::unbounded_channel();
    manager.bind_cron_continuation(continuation_tx);
    (manager, scheduler, continuation_rx)
}

async fn persist_cron_session(manager: &SessionManager, tmp: &tempfile::TempDir, session_id: &str) {
    use peri_acp_types::session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta};
    use peri_acp_types::workspace::SessionBinding;

    let workspace = manager
        .session_resources()
        .resolve_workspace(tmp.path())
        .await
        .unwrap();
    manager
        .session_resources()
        .create_session(&NewSession {
            thread_id: session_id.into(),
            created_at: peri_time::now_utc_rfc3339(),
            meta: NewSessionMeta {
                title: None,
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new(r#"{"v":1}"#),
        })
        .await
        .unwrap();
}

/// 同 make_session_manager，仅 SessionManager::new 末参按需传入 cron scheduler。
async fn make_manager_with_cron_option(
    tmp: &tempfile::TempDir,
    cron_scheduler: Option<Arc<parking_lot::Mutex<peri_mcp_cron::CronScheduler>>>,
) -> SessionManager {
    make_manager_inner(tmp, cron_scheduler, Vec::new(), None).await
}

/// Phase 6 B2：构造带插件命令静态条目的 SessionManager（cron 无）。
async fn make_manager_with_plugin_entries(
    tmp: &tempfile::TempDir,
    plugin_entries: Vec<RouteEntry>,
) -> SessionManager {
    make_manager_inner(tmp, None, plugin_entries, None).await
}

/// AW3-11：注入**真实** per-session `TaskManager` 工厂，使「工厂产出」与
/// 「外部携带」两条 manager 来源在用例里可辨识（`Arc::ptr_eq` / 判型）。
async fn make_manager_with_task_manager_factory(tmp: &tempfile::TempDir) -> SessionManager {
    make_manager_inner(
        tmp,
        None,
        Vec::new(),
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
    )
    .await
}

/// 通用构造：cron scheduler + 插件命令静态条目 + per-session TaskManager 工厂可组合注入。
async fn make_manager_inner(
    tmp: &tempfile::TempDir,
    cron_scheduler: Option<Arc<parking_lot::Mutex<peri_mcp_cron::CronScheduler>>>,
    plugin_entries: Vec<RouteEntry>,
    task_manager_factory: Option<TaskManagerFactory>,
) -> SessionManager {
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![make_provider_config("a", "gpt-4o")];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    SessionManager::new(
        session_resources,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        cron_scheduler.map(|s| {
            Arc::new(peri_mcp_cron::CronSchedulerPortHandle(s))
                as Arc<dyn peri_acp_types::cron::CronSchedulerPort>
        }),
        None, // MCP 订阅端口（测试无）
        None, // Dynamic MCP（测试无）
        task_manager_factory,
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        plugin_entries,
    )
}

/// 测试用 MCP 订阅端口：与真实实现（McpClientPool）同构——注册表为
/// session_id → InboxHandle 的 HashMap（insert 天然幂等）；另记录注销调用。
#[derive(Default)]
struct FakeMcpSubscriptionPort {
    inboxes:
        std::sync::Mutex<std::collections::HashMap<String, peri_acp_types::session::InboxHandle>>,
    unregistered: std::sync::Mutex<Vec<String>>,
}

impl FakeMcpSubscriptionPort {
    fn inbox_count(&self) -> usize {
        self.inboxes.lock().unwrap().len()
    }

    fn has_inbox(&self, session_id: &str) -> bool {
        self.inboxes.lock().unwrap().contains_key(session_id)
    }

    fn unregistered(&self) -> Vec<String> {
        self.unregistered.lock().unwrap().clone()
    }
}

impl peri_acp_types::mcp::McpSubscriptionPort for FakeMcpSubscriptionPort {
    fn register_inbox(&self, session_id: &str, handle: peri_acp_types::session::InboxHandle) {
        self.inboxes
            .lock()
            .unwrap()
            .insert(session_id.to_string(), handle);
    }

    fn unregister_inbox(&self, session_id: &str) {
        self.inboxes.lock().unwrap().remove(session_id);
        self.unregistered
            .lock()
            .unwrap()
            .push(session_id.to_string());
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// 同 make_session_manager，仅 MCP 订阅端口参数按需注入（mcp_subscription_for 测试用）。
async fn make_manager_with_mcp_subscription(
    tmp: &tempfile::TempDir,
    mcp_subscription: Option<Arc<dyn peri_acp_types::mcp::McpSubscriptionPort>>,
) -> SessionManager {
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![make_provider_config("a", "gpt-4o")];
    peri_config.config.profiles = Profiles {
        sonnet: ProfileConfig {
            provider: "a".to_string(),
            ..Default::default()
        },
        ..Default::default()
    };
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    SessionManager::new(
        session_resources,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None, // cron 调度器（测试无）
        mcp_subscription,
        None, // Dynamic MCP（测试无）
        None, // 无 bg 场景：fallback NoopTaskManager
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
    )
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

/// 验证 ensure_session 幂等：重复调用不会覆盖已有记录
#[tokio::test]
async fn test_ensure_session_幂等不覆盖已有记录() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    let session_id = "test-session-idempotent";

    // 第一次插入
    mgr.ensure_session(session_id, "/tmp");
    let goal_state_first = mgr.goal_state_for(session_id);
    assert!(
        goal_state_first.is_some(),
        "首次 ensure_session 后应能取到 goal_state"
    );

    // 第二次插入（幂等）— 不应覆盖已有记录
    mgr.ensure_session(session_id, "/tmp/different");
    let goal_state_second = mgr.goal_state_for(session_id);
    assert!(
        goal_state_second.is_some(),
        "幂等调用后仍应能取到 goal_state"
    );

    // 两次取出的 goal_state 应为同一句柄（Arc 共享）
    let g1 = goal_state_first.unwrap();
    let g2 = goal_state_second.unwrap();
    // 写入一条用户消息，验证两个句柄共享同一内部状态
    g1.put_pending_user_message("hello".to_string());
    assert_eq!(
        g2.take_pending_user_message(),
        Some("hello".to_string()),
        "两次 ensure_session 后的 goal_state 应共享内部状态"
    );
}

/// 验证 goal_state_for 在 session 不存在时返回 None
#[tokio::test]
async fn test_goal_state_for_不存在返回none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    assert!(
        mgr.goal_state_for("non-existent").is_none(),
        "不存在的 session_id 应返回 None"
    );
}

/// 验证 build_frozen_data 返回非空 system_prompt 且日期格式正确
#[tokio::test]
async fn test_build_frozen_data_返回非空system_prompt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;

    let frozen = mgr.build_frozen_data(tmp.path().to_str().unwrap());
    assert!(
        !frozen.system_prompt().is_empty(),
        "frozen system_prompt 不应为空"
    );
    // 日期格式 YYYY-MM-DD（10 字符，含两个连字符）
    let date_chars: Vec<char> = frozen.date().chars().collect();
    assert_eq!(date_chars.len(), 10, "日期长度应为 10");
    assert_eq!(date_chars[4], '-', "第 5 个字符应为连字符");
    assert_eq!(date_chars[7], '-', "第 8 个字符应为连字符");
}

/// 验证 cancel_cascade_children_for 在 session 不存在时不 panic
#[tokio::test]
async fn test_cancel_cascade_children_for_不存在不panic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    // 不应 panic
    mgr.cancel_cascade_children_for("non-existent");
}

/// 验证 close_session 移除 AcpSession 记录后 goal_state_for 返回 None
#[tokio::test]
async fn test_close_session_移除记录后goal_state返回none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    let session_id = "test-close-session";

    mgr.ensure_session(session_id, "/tmp");
    assert!(mgr.goal_state_for(session_id).is_some());

    mgr.close_session(session_id).await.unwrap();
    assert!(
        mgr.goal_state_for(session_id).is_none(),
        "close_session 后 goal_state_for 应返回 None"
    );
}

#[tokio::test]
async fn test_pre_close_cancels_but_preserves_record_until_terminal_close() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    let session_id = "pre-close-session";
    mgr.ensure_session(session_id, tmp.path().to_str().unwrap());
    let cancel = mgr
        .get_session(session_id)
        .expect("session exists")
        .cancel_token
        .clone();

    mgr.pre_close_session(session_id);

    assert!(cancel.is_cancelled());
    assert!(mgr.get_session(session_id).is_some());
    assert_eq!(mgr.session_ids(), vec![session_id.to_string()]);
    mgr.close_session(session_id).await.unwrap();
    assert!(mgr.session_ids().is_empty());
}

/// [回归] session 注册完成后、首个 turn 前到达的 cron trigger 不会丢失。
#[tokio::test]
async fn test_ensure_session_subscribes_cron_before_first_turn() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mgr, scheduler, mut continuation_rx) = make_session_manager_with_cron(&tmp).await;
    let session_id = "test-cron-before-first-turn";

    persist_cron_session(&mgr, &tmp, session_id).await;
    mgr.ensure_session(session_id, "/tmp");

    let task_id = scheduler
        .lock()
        .register("* * * * *", "before-first-turn")
        .unwrap();
    {
        let mut sched = scheduler.lock();
        assert!(sched.force_next_fire_to_past(&task_id));
        sched.tick();
    }

    let request = tokio::time::timeout(Duration::from_secs(1), continuation_rx.recv())
        .await
        .expect("session 发布时应已订阅，首个 turn 前的 trigger 应及时转发")
        .expect("Host continuation receiver 应存活");
    assert_eq!(request.session_id, session_id);
    assert_eq!(request.trigger.task_id, task_id);
    assert_eq!(request.trigger.prompt, "before-first-turn");

    mgr.close_session(session_id).await.unwrap();
}

/// [回归] turn 以 Error 结束后 cron bridge 仍转发完整 continuation 请求。
#[tokio::test]
async fn test_cron_bridge_survives_turn_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mgr, scheduler, mut continuation_rx) = make_session_manager_with_cron(&tmp).await;
    let session_id = "test-cron-turn-error";
    persist_cron_session(&mgr, &tmp, session_id).await;
    mgr.ensure_session(session_id, "/tmp");
    assert!(mgr.cron_bridge_for(session_id));

    // 模拟 turn：构造 per-turn V2Session（共享 session queue）后以 Error drop。
    let queue = mgr.v2_queue_for(session_id).unwrap();
    {
        let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
        let v2 = peri_agent::session::Session::new_with_cancel_and_queue(
            Arc::from("/tmp"),
            peri_agent::session::FrozenContext::builder().build(),
            None,
            cancel,
            queue,
        );
        drop(v2);
    }

    let task_id = scheduler
        .lock()
        .register("* * * * *", "turn-error-survival")
        .unwrap();
    {
        let mut sched = scheduler.lock();
        assert!(sched.force_next_fire_to_past(&task_id));
        sched.tick();
    }

    let request = tokio::time::timeout(Duration::from_secs(1), continuation_rx.recv())
        .await
        .expect("cron bridge 应及时转发")
        .expect("Host continuation receiver 应存活");
    assert_eq!(request.session_id, session_id);
    assert_eq!(request.trigger.task_id, task_id);
    assert_eq!(request.trigger.prompt, "turn-error-survival");
    assert!(mgr.v2_queue_for(session_id).unwrap().is_empty());

    mgr.close_session(session_id).await.unwrap();
}

/// [回归] idle 期 cron bridge 转发完整 continuation 请求，不提前写入 inbox。
#[tokio::test]
async fn test_cron_bridge_idle_trigger_forwards_continuation_without_early_enqueue() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mgr, scheduler, mut continuation_rx) = make_session_manager_with_cron(&tmp).await;
    let session_id = "test-cron-idle";
    persist_cron_session(&mgr, &tmp, session_id).await;
    mgr.ensure_session(session_id, "/tmp");
    assert!(mgr.cron_bridge_for(session_id));

    let task_id = scheduler
        .lock()
        .register("* * * * *", "idle-survival")
        .unwrap();
    {
        let mut sched = scheduler.lock();
        assert!(sched.force_next_fire_to_past(&task_id));
        sched.tick();
    }

    let request = tokio::time::timeout(Duration::from_secs(1), continuation_rx.recv())
        .await
        .expect("idle cron bridge 应及时转发")
        .expect("Host continuation receiver 应存活");
    assert_eq!(request.session_id, session_id);
    assert_eq!(request.trigger.task_id, task_id);
    assert_eq!(request.trigger.prompt, "idle-survival");
    assert!(mgr.v2_queue_for(session_id).unwrap().is_empty());

    mgr.close_session(session_id).await.unwrap();
}

/// [S1.1] 协商值只消费一次：同一 server 进程内第 2+ 个 session/new 仍拿到协商值。
///
/// stdio 路径复现（`acp_stdio/session/create.rs:106` 每次 session/new 都调
/// `consume_pending_caps`）：旧实现 take() 一次性消费，第 2 个 session 取到
/// None → 注册全 false caps；`session/load`/`resume`/`fork` 走 `ensure_session_caps`
/// 则回退 all_enabled——同一客户端不同 session 门控行为不同。
#[tokio::test]
async fn test_pending_caps_consumed_once_second_session_gets_negotiated() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;

    // initialize 协商：仅部分 cap 开启
    let negotiated = peri_acp_types::PeriCaps {
        replay: true,
        agent_event: true,
        ..Default::default()
    };
    mgr.set_pending_caps(negotiated.clone());

    // 第 1 个 session/new → 协商值
    let caps1 = mgr.consume_pending_caps("s1");
    assert_eq!(caps1, negotiated);

    // 第 2 个 session/new → 仍为协商值（旧实现取到 None → 全 false）
    let caps2 = mgr.consume_pending_caps("s2");
    assert_eq!(caps2, negotiated, "第 2+ 个 session/new 必须拿到协商值");

    // load/resume/fork 新 session id（registry 未命中）→ 也应为协商值（旧实现 all_enabled）
    let caps3 = mgr.ensure_session_caps("s3");
    assert_eq!(
        caps3, negotiated,
        "load/resume/fork 新 session 必须拿到协商值"
    );

    // registry 幂等：已注册 session 不被覆盖
    let caps1_again = mgr.ensure_session_caps("s1");
    assert_eq!(caps1_again, negotiated);
}

/// [S1.1] 双 fallback 语义必须保留：未协商时 consume=全 false、ensure=all_enabled。
///
/// 改坏任一侧都会翻转 TUI/stdio 行为（P0-3 对抗 review 确认）：consume 未协商
/// → `unwrap_or_default()`（全 false）；ensure 未协商 → `all_enabled()`。
#[tokio::test]
async fn test_pending_caps_double_fallback_semantics() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp).await;
    // 不调用 set_pending_caps（MpscTransport / TUI 内部路径，无 initialize）

    let consumed = mgr.consume_pending_caps("t1");
    assert_eq!(
        consumed,
        peri_acp_types::PeriCaps::default(),
        "consume 未协商 → 全 false（unwrap_or_default）"
    );

    let ensured = mgr.ensure_session_caps("t2");
    assert_eq!(
        ensured,
        peri_acp_types::PeriCaps::all_enabled(),
        "ensure 未协商 → all_enabled"
    );
}

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
