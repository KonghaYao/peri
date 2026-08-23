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
use crate::session::SessionManager;
use peri_agent::thread::FilesystemThreadStore;
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
fn make_session_manager(tmp: &tempfile::TempDir) -> SessionManager {
    make_manager_with_cron_option(tmp, None)
}

/// 构造关闭 `SkillsMiddleware` 的 SessionManager，用于验证 MetaHarness 对
/// slash 路由的关闭面也生效，避免 `/skill` 绕过 middleware 装配。
fn make_session_manager_skills_disabled(tmp: &tempfile::TempDir) -> SessionManager {
    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
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
    peri_config.config.meta_harness = Some(std::collections::HashMap::from([(
        "SkillsMiddleware".to_string(),
        false,
    )]));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    SessionManager::new(
        thread_store,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        Vec::new(),
        Vec::new(),
    )
}

/// 构造带 cron scheduler 的 SessionManager（session 级 cron bridge 测试用）。
///
/// scheduler 的 primary tx 直接丢弃（同 TUI `cron_state.rs:13` 模式）——
/// 本测试路径不消费 primary trigger 通道，只验证 extra_trigger_txs（bridge）路径。
fn make_session_manager_with_cron(
    tmp: &tempfile::TempDir,
) -> (
    SessionManager,
    Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>,
) {
    let scheduler = Arc::new(parking_lot::Mutex::new(
        peri_middlewares::cron::CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0),
    ));
    (
        make_manager_with_cron_option(tmp, Some(scheduler.clone())),
        scheduler,
    )
}

/// 同 make_session_manager，仅 SessionManager::new 末参按需传入 cron scheduler。
fn make_manager_with_cron_option(
    tmp: &tempfile::TempDir,
    cron_scheduler: Option<Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>>,
) -> SessionManager {
    make_manager_inner(tmp, cron_scheduler, Vec::new())
}

/// Phase 6 B2：构造带插件命令静态条目的 SessionManager（cron 无）。
fn make_manager_with_plugin_entries(
    tmp: &tempfile::TempDir,
    plugin_entries: Vec<RouteEntry>,
) -> SessionManager {
    make_manager_inner(tmp, None, plugin_entries)
}

/// 通用构造：cron scheduler + 插件命令静态条目可组合注入。
fn make_manager_inner(
    tmp: &tempfile::TempDir,
    cron_scheduler: Option<Arc<parking_lot::Mutex<peri_middlewares::cron::CronScheduler>>>,
    plugin_entries: Vec<RouteEntry>,
) -> SessionManager {
    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
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
        thread_store,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        cron_scheduler.map(|s| {
            Arc::new(peri_middlewares::cron::CronSchedulerPortHandle(s))
                as Arc<dyn peri_acp_types::cron::CronSchedulerPort>
        }),
        None, // MCP 订阅端口（测试无）
        None, // 无 bg 场景：fallback NoopTaskManager
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        plugin_entries,
        Vec::new(), // plugin skill roots（C1；测试无）
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
fn make_manager_with_mcp_subscription(
    tmp: &tempfile::TempDir,
    mcp_subscription: Option<Arc<dyn peri_acp_types::mcp::McpSubscriptionPort>>,
) -> SessionManager {
    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
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
        thread_store,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None, // cron 调度器（测试无）
        mcp_subscription,
        None, // 无 bg 场景：fallback NoopTaskManager
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
        Vec::new(), // plugin skill roots（C1；测试无）
    )
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

/// 验证 ensure_session 幂等：重复调用不会覆盖已有记录
#[tokio::test]
async fn test_ensure_session_幂等不覆盖已有记录() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);
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
    let mgr = make_session_manager(&tmp);
    assert!(
        mgr.goal_state_for("non-existent").is_none(),
        "不存在的 session_id 应返回 None"
    );
}

/// 验证 build_frozen_data 返回非空 system_prompt 且日期格式正确
#[tokio::test]
async fn test_build_frozen_data_返回非空system_prompt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);

    let frozen = mgr.build_frozen_data(tmp.path().to_str().unwrap(), &[], &[]);
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
    let mgr = make_session_manager(&tmp);
    // 不应 panic
    mgr.cancel_cascade_children_for("non-existent");
}

/// 验证 close_session 移除 AcpSession 记录后 goal_state_for 返回 None
#[tokio::test]
async fn test_close_session_移除记录后goal_state返回none() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);
    let session_id = "test-close-session";

    mgr.ensure_session(session_id, "/tmp");
    assert!(mgr.goal_state_for(session_id).is_some());

    mgr.close_session(session_id).await.unwrap();
    assert!(
        mgr.goal_state_for(session_id).is_none(),
        "close_session 后 goal_state_for 应返回 None"
    );
}

/// [回归] turn 以 Error 结束后 cron 触发仍能注入 session（不丢失）。
#[tokio::test]
async fn test_cron_bridge_survives_turn_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mgr, scheduler) = make_session_manager_with_cron(&tmp);
    let session_id = "test-cron-turn-error";
    mgr.ensure_session(session_id, "/tmp");

    // 第一 turn：build_stage_context 挂载 session 级 bridge（幂等）
    assert!(mgr.cron_bridge_for(session_id));

    // 模拟 turn：构造 per-turn V2Session（共享 session queue）后以 Error drop
    let queue = mgr.v2_queue_for(session_id).unwrap();
    {
        let cancel = Arc::new(tokio_util::sync::CancellationToken::new());
        let v2 = peri_agent::session::Session::new_with_cancel_and_queue(
            Arc::from("/tmp"),
            peri_agent::session::FrozenContext::builder().build(),
            None,
            cancel,
            queue.clone(),
        );
        drop(v2); // turn 结束（LoopResult::Error 路径）→ 旧实现此处杀死 bridge
    }

    // cron 到点触发（TUI tick 循环等价物）
    let id = scheduler
        .lock()
        .register("* * * * *", "turn-error-survival")
        .unwrap();
    {
        let mut sched = scheduler.lock();
        assert!(sched.force_next_fire_to_past(&id));
        sched.tick();
    }
    tokio::time::sleep(Duration::from_millis(50)).await; // 等 bridge 异步转发（cron_owner_test 同款 50ms 模式）

    // 触发必须已入队（queued，下一 turn 消费），而非被 retain 丢弃
    let inbox = mgr.session_inbox_for(session_id).unwrap();
    let drained = inbox.queue().drain_all();
    assert_eq!(drained.len(), 1, "turn Error 后 cron 触发不得丢失");
    assert_eq!(
        drained[0].source,
        peri_acp_types::session::MessageSource::CronTrigger
    );

    // 清理：close_session → bridge drop → abort（幂等，无 panic）
    mgr.close_session(session_id).await.unwrap();
}

/// [回归] idle 期（无 turn 运行）cron 触发入队不丢弃；"queued, not dropped"
/// （立即开新 turn 属后续增强，不在本期范围）。
#[tokio::test]
async fn test_cron_bridge_idle_trigger_queued_not_dropped() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (mgr, scheduler) = make_session_manager_with_cron(&tmp);
    let session_id = "test-cron-idle";
    mgr.ensure_session(session_id, "/tmp");
    assert!(mgr.cron_bridge_for(session_id));

    // idle：无 executor 运行，仅 TUI tick 循环存活
    let id = scheduler
        .lock()
        .register("* * * * *", "idle-survival")
        .unwrap();
    {
        let mut sched = scheduler.lock();
        assert!(sched.force_next_fire_to_past(&id));
        sched.tick();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;

    let inbox = mgr.session_inbox_for(session_id).unwrap();
    assert_eq!(
        inbox.queue().len(),
        1,
        "idle 期触发必须留在 queue（不丢弃）"
    );
    let drained = inbox.queue().drain_all();
    assert_eq!(
        drained[0].source,
        peri_acp_types::session::MessageSource::CronTrigger
    );

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
    let mgr = make_session_manager(&tmp);

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
    let mgr = make_session_manager(&tmp);
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
    let mgr = make_session_manager(&tmp);
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
    );
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
    );
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
    );
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
    let mgr = make_session_manager(&tmp);
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
    let mgr = make_session_manager(&tmp);
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

// ─── MetaHarness 冻结状态（设计 §2.3）───────────────────────────────────────

use std::collections::HashMap;

fn mh_cfg(entries: &[(&str, bool)]) -> HashMap<String, bool> {
    entries.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn default_state() -> peri_acp_types::meta_harness::MetaHarnessState {
    peri_acp_types::meta_harness::MetaHarnessState::default()
}

#[test]
fn build_meta_harness_state_empty_config_is_default() {
    let state = super::build_meta_harness_state(None, HashMap::new());
    assert_eq!(state, default_state());
    let state = super::build_meta_harness_state(Some(&HashMap::new()), HashMap::new());
    assert_eq!(state, default_state());
}

#[test]
fn build_meta_harness_state_section_true_with_doc_enters_overrides() {
    let mut docs = HashMap::new();
    docs.insert("01_intro".to_string(), "custom intro".to_string());
    let state = super::build_meta_harness_state(Some(&mh_cfg(&[("01_intro", true)])), docs);
    assert_eq!(
        state.section_overrides.get("01_intro").map(|s| s.as_ref()),
        Some("custom intro")
    );
    assert!(state.disabled_middlewares.is_empty());
}

#[test]
fn build_meta_harness_state_section_true_without_doc_warns_and_ignores() {
    let state =
        super::build_meta_harness_state(Some(&mh_cfg(&[("01_intro", true)])), HashMap::new());
    assert!(
        state.section_overrides.is_empty(),
        "文档缺失时忽略覆盖（保持内置段落）"
    );
}

#[test]
fn build_meta_harness_state_section_false_does_not_override() {
    let mut docs = HashMap::new();
    docs.insert("01_intro".to_string(), "custom intro".to_string());
    let state = super::build_meta_harness_state(Some(&mh_cfg(&[("01_intro", false)])), docs);
    assert!(
        state.section_overrides.is_empty(),
        "section + false = 显式不覆盖，即使文档存在"
    );
}

#[test]
fn build_meta_harness_state_middleware_false_enters_disabled() {
    let state =
        super::build_meta_harness_state(Some(&mh_cfg(&[("WebMiddleware", false)])), HashMap::new());
    assert!(state.disabled_middlewares.contains("WebMiddleware"));
    assert!(state.section_overrides.is_empty());
}

#[test]
fn build_meta_harness_state_middleware_true_not_disabled() {
    let state =
        super::build_meta_harness_state(Some(&mh_cfg(&[("WebMiddleware", true)])), HashMap::new());
    assert!(
        state.disabled_middlewares.is_empty(),
        "middleware + true = 显式恢复装配"
    );
}

#[test]
fn build_meta_harness_state_mixed_entries() {
    let mut docs = HashMap::new();
    docs.insert("01_intro".to_string(), "intro".to_string());
    docs.insert("05_using_tools".to_string(), "tools".to_string());
    let state = super::build_meta_harness_state(
        Some(&mh_cfg(&[
            ("01_intro", true),
            ("05_using_tools", false),
            ("WebMiddleware", false),
            ("FilesystemMiddleware", true),
        ])),
        docs,
    );
    assert_eq!(state.section_overrides.len(), 1, "仅 true+文档存在 进入");
    assert!(state.section_overrides.contains_key("01_intro"));
    assert!(!state.section_overrides.contains_key("05_using_tools"));
    assert_eq!(state.disabled_middlewares.len(), 1);
    assert!(state.disabled_middlewares.contains("WebMiddleware"));
    assert!(!state.disabled_middlewares.contains("FilesystemMiddleware"));
}

/// 集成：build_frozen_data 应用段落覆盖 + middleware 关闭集合到冻结载体；
/// 主 prompt 与 SubAgent 无 workflow prompt 共用同一覆盖。
#[tokio::test]
async fn test_build_frozen_data_applies_meta_harness_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();
    // .peri/meta/01_intro.md 与 .peri/meta/05_using_tools.md
    let meta_dir = std::path::Path::new(&cwd).join(".peri").join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    std::fs::write(meta_dir.join("01_intro.md"), "CUSTOM-INTRO-BODY").unwrap();
    std::fs::write(meta_dir.join("05_using_tools.md"), "CUSTOM-TOOLS-BODY").unwrap();

    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
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
    peri_config.config.meta_harness = Some(mh_cfg(&[
        ("01_intro", true),
        ("05_using_tools", true),
        ("WebMiddleware", false),
    ]));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mgr = SessionManager::new(
        thread_store,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
        Vec::new(), // plugin skill roots（C1；测试无）
    );

    let frozen = mgr.build_frozen_data(&cwd, &[], &[]);
    let state = frozen.meta_harness();
    assert_eq!(
        state.section_overrides.get("01_intro").map(|s| s.as_ref()),
        Some("CUSTOM-INTRO-BODY"),
        "冻结状态包含段落覆盖"
    );
    assert_eq!(
        state
            .section_overrides
            .get("05_using_tools")
            .map(|s| s.as_ref()),
        Some("CUSTOM-TOOLS-BODY")
    );
    assert!(
        state.disabled_middlewares.contains("WebMiddleware"),
        "冻结状态包含关闭集合"
    );
    // 主 prompt 应用覆盖（SubAgent / fork / workflow agent 直接复用主
    // prompt——子面向字段已随 C5 移除，无独立断言对象）
    assert!(
        frozen.system_prompt().contains("CUSTOM-INTRO-BODY"),
        "主 prompt 应用覆盖"
    );
    // accessor 与 v2_frozen 返回同一状态（单事实源）
    assert_eq!(
        frozen.meta_harness(),
        &frozen.v2_frozen().meta_harness,
        "accessor 与 FrozenContext 字段一致"
    );
}

/// 冻结语义：构造后修改/删除 .peri/meta 文件，已构造的 frozen data 不变；
/// 新建（重新 build_frozen_data）才看到新内容。
#[tokio::test]
async fn test_frozen_data_does_not_reread_meta_docs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();
    let meta_dir = std::path::Path::new(&cwd).join(".peri").join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    std::fs::write(meta_dir.join("01_intro.md"), "V1-BODY").unwrap();

    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
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
    peri_config.config.meta_harness = Some(mh_cfg(&[("01_intro", true)]));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mgr = SessionManager::new(
        thread_store,
        provider,
        Arc::new(peri_config),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        Vec::new(), // plugin 命令条目（Phase 6 B2；测试无）
        Vec::new(), // plugin skill roots（C1；测试无）
    );

    let frozen = mgr.build_frozen_data(&cwd, &[], &[]);
    assert!(frozen.system_prompt().contains("V1-BODY"));

    // 删除文件并重建：已构造的 frozen 不变；新 build 才看到变化（无覆盖）
    std::fs::remove_file(meta_dir.join("01_intro.md")).unwrap();
    assert!(
        frozen.system_prompt().contains("V1-BODY"),
        "已冻结的 prompt 不因磁盘变化而变（ARC-FROZEN-001）"
    );
    let frozen2 = mgr.build_frozen_data(&cwd, &[], &[]);
    assert!(
        !frozen2.system_prompt().contains("V1-BODY"),
        "新会话（新 build）才反映变更"
    );
    assert!(
        frozen2.meta_harness().section_overrides.is_empty(),
        "文档删除后新冻结状态无覆盖"
    );
}

/// 占位 handler：测试只断言路由层（注册 / 解析 / 投影），不触发执行。
struct TestHandler;

#[async_trait]
impl CommandHandler for TestHandler {
    async fn execute(&self, _ctx: peri_acp_types::command::CommandContext) -> CommandOutcome {
        CommandOutcome::Inject(String::new())
    }
}

// ─── Phase 6 B2/C1：会话创建注册本地 skills（core 域）+ 插件静态命令 ────

/// 写入本地 skill fixture：`{cwd}/.claude/skills/{dir}/SKILL.md`
/// （frontmatter name 可含任意字符串——含冒号形态由词法校验兜底）。
fn write_local_skill(cwd: &std::path::Path, dir: &str, skill_name: &str) {
    let dir_path = cwd.join(".claude").join("skills").join(dir);
    std::fs::create_dir_all(&dir_path).unwrap();
    std::fs::write(
        dir_path.join("SKILL.md"),
        format!("---\nname: \"{skill_name}\"\ndescription: \"test skill {skill_name}\"\n---\nBody"),
    )
    .unwrap();
}

/// C1：本地 skill 注册为 `core:{name}`（第一等级显式形态，kind = Skill），
/// 裸名快捷匹配可用（第一等级裸名 alias_index 登记）。
#[tokio::test]
async fn test_session_creation_registers_local_skills_core_domain() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_local_skill(tmp.path(), "hello", "hello");
    let mgr = make_session_manager(&tmp);
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    let resolved = reg.resolve("/hello").expect("裸名命中本地 skill");
    assert_eq!(resolved.entry.fullname, "core:hello");
    assert_eq!(resolved.entry.kind, CommandEntryKind::Skill);
    assert_eq!(resolved.entry.description, "test skill hello");

    let resolved = reg.resolve("/core:hello").expect("全名命中");
    assert_eq!(resolved.entry.kind, CommandEntryKind::Skill);
}

#[tokio::test]
async fn test_session_creation_registers_builtin_skill_alias() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp);
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    let resolved = reg.resolve("/ptc").expect("builtin alias 应命中");

    assert_eq!(resolved.entry.fullname, "core:programmatic-tool-calling");
    assert_eq!(resolved.entry.kind, CommandEntryKind::Skill);
}

/// MetaHarness 关闭 SkillsMiddleware 时，本地 skill 不得进入命令注册表；否则
/// slash 路由会经 AgentPassthrough 绕开 middleware 的装配期开关。
#[tokio::test]
async fn test_session_creation_does_not_register_skills_when_disabled() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_local_skill(tmp.path(), "hello", "hello");
    let mgr = make_session_manager_skills_disabled(&tmp);
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    assert!(reg.resolve("/hello").is_none());
    assert!(reg.resolve("/core:hello").is_none());
}

/// C1 冲突裁决：内置 compact 先注册 → 同名 skill 被拒 + 告警，注册表保持
/// 内置条目（不覆盖、不静默；冲突纯拒绝 + 装配顺序即优先级）。
#[tokio::test]
async fn test_session_creation_core_conflict_keeps_builtin() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_local_skill(tmp.path(), "compact", "compact");
    let mgr = make_session_manager(&tmp);
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    let snap = reg.snapshot();
    let compact = snap
        .iter()
        .find(|e| e.fullname == "core:compact")
        .expect("内置 core:compact 存在");
    assert_eq!(
        compact.kind,
        CommandEntryKind::Command,
        "同名 skill 被拒，注册表保持内置条目"
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

/// C1 名称规范化：扫描时将 skill 名中的冒号改为连字符，使其能注册为
/// `core:{name}` 第一等级显式形态，裸名快捷匹配可用。
#[tokio::test]
async fn test_session_creation_normalizes_skill_name_with_colon() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_local_skill(tmp.path(), "namespaced", "foo:bar");
    let mgr = make_session_manager(&tmp);
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    let resolved = reg.resolve("/foo-bar").expect("规范化名称应可解析");
    assert_eq!(resolved.entry.fullname, "core:foo-bar");
    assert_eq!(resolved.entry.kind, CommandEntryKind::Skill);
    assert!(
        reg.resolve("/foo:bar").is_none(),
        "原始冒号名称不再作为命令注册"
    );
}

/// B2 集成：会话创建注册插件静态命令 `plugin:{plugin}:{cmd}`（kind =
/// Command，provenance = Plugin{name} + Connected），第二等级完整形态可解析。
#[tokio::test]
async fn test_session_creation_registers_plugin_commands() {
    let tmp = tempfile::TempDir::new().unwrap();
    let plugin_entry = RouteEntry {
        fullname: "plugin:ecc:deploy".into(),
        aliases: vec![],
        description: "deploy command".into(),
        kind: CommandEntryKind::Command,
        category: None,
        args_schema: None,
        handler: Arc::new(TestHandler),
        provenance: CommandProvenance {
            source: CommandSource::Plugin { name: "ecc".into() },
            lifecycle: CommandLifecycle::Connected,
        },
    };
    let mgr = make_manager_with_plugin_entries(&tmp, vec![plugin_entry]);
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    let resolved = reg.resolve("/plugin:ecc:deploy").expect("插件命令命中");
    assert_eq!(resolved.entry.kind, CommandEntryKind::Command);
    assert_eq!(resolved.entry.description, "deploy command");
    assert_eq!(
        resolved.entry.provenance.source,
        CommandSource::Plugin { name: "ecc".into() },
        "provenance = 剥离 plugin: 前缀的插件名"
    );
    assert_eq!(
        resolved.entry.provenance.lifecycle,
        CommandLifecycle::Connected
    );
    // 第二等级不登记裸名（deploy 不可解析）。
    assert!(reg.resolve("/deploy").is_none());
}

/// B2 注册顺序：内置 → 本地 skills → 插件（先注册者占键）。插件与内置/
/// skill 键空间不相交（plugin: 域 vs core: 域），冲突裁决仅按键唯一性。
#[tokio::test]
async fn test_session_creation_register_order_builtin_skill_plugin() {
    let tmp = tempfile::TempDir::new().unwrap();
    write_local_skill(tmp.path(), "hello", "hello");
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
    );
    mgr.ensure_session("s1", tmp.path().to_str().unwrap());

    let reg = mgr.command_registry_for("s1").expect("session 注册表存在");
    // 内置（core:compact）与 skill（core:hello）、插件（plugin:ecc:deploy）共存。
    assert_eq!(
        reg.resolve("/compact").unwrap().entry.kind,
        CommandEntryKind::Command
    );
    assert_eq!(
        reg.resolve("/hello").unwrap().entry.kind,
        CommandEntryKind::Skill
    );
    assert_eq!(
        reg.resolve("/plugin:ecc:deploy").unwrap().entry.kind,
        CommandEntryKind::Command
    );
}
