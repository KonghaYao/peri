//! SessionManager 单元测试。
//!
//! 覆盖 `ensure_session` / `goal_state_for` / `cancel_cascade_children_for` /
//! `build_frozen_data` 四个新方法，验证 TUI/stdio 三合一重构后的行为契约。

use std::sync::Arc;
use std::time::Duration;

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
        None, // 无 bg 场景：fallback NoopTaskManager
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
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

    let frozen = mgr.build_frozen_data(tmp.path().to_str().unwrap(), &[], &[], true);
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

/// [回归测试] last_notified_permission_mode 初始化为"未通知过"哨兵。
///
/// 历史背景（D2 / P3-2026-08-02）：10_hitl 不含 mode snapshot、Bypass 时
/// 10_hitl 不渲染，初始 mode 从不向模型公开。旧实现把 last_notified 初始化为
/// session 创建时的全局 mode，使首轮不产生通知——初始 mode 因此永久不可见。
/// 修复后初始化为 [`PERMISSION_MODE_NEVER_NOTIFIED`] 哨兵：首个模型可见
/// turn 公开初始 mode 一次，入队记账后不再重复；真实 mode 值（0..=4）
/// 不会与哨兵碰撞。
#[tokio::test]
async fn test_last_notified_permission_mode_initialized_to_never_notified() {
    use std::sync::atomic::Ordering;

    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_session_manager(&tmp); // make_session_manager 全局 mode = Bypass
    let session_id = "test-last-notified-init";

    mgr.ensure_session(session_id, "/tmp");
    let last = mgr
        .get_session(session_id)
        .map(|s| s.last_notified_permission_mode.load(Ordering::Relaxed))
        .expect("ensure_session 后应存在 AcpSession");
    assert_eq!(
        last,
        super::executor::PERMISSION_MODE_NEVER_NOTIFIED,
        "last_notified 应初始化为'未通知过'哨兵（首个模型可见 turn 公开初始 mode）"
    );
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
