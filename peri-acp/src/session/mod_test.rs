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

#[path = "cron_mcp_test_fixture.rs"]
mod cron_mcp_fixture;

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
/// Bridge reads a real rmcp connection; it never subscribes to scheduler channels.
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
    manager.bind_cron_mcp(Some(cron_mcp_fixture::connect(scheduler.clone()).await));
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
    assert!(mgr
        .v2_queue_for(session_id)
        .unwrap()
        .is_empty()
        .await
        .expect("test MQ len"));

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
    assert!(mgr
        .v2_queue_for(session_id)
        .unwrap()
        .is_empty()
        .await
        .expect("test MQ len"));

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

include!("mod_test_part2_test.rs");
