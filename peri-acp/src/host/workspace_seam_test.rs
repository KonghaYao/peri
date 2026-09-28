//! AW3-11（MCP 适配 v4 wave 3）**发送端**的 host 侧 seam 用例。
//!
//! 覆盖面（每条对应主 plan §2 AW3-11 与 §2.1 的冻结口径）：
//!
//! 1. [`session_environment_holds_the_workspace_input_it_injected`]：会话环境装配
//!    产出的两名成员（per-session `TaskManager` + session 级 `on_bg_complete`）就是
//!    送进 builtin 上下文的那份——环境持有的 manager 与输入里的 manager 同一 `Arc`，
//!    且经 `ensure_session_with_task_manager` 登记后 `AcpSession::task_manager`
//!    仍是**同一份**（三个落点同源，不是三份等值对象）。同时确认它**不是**
//!    `NoopTaskManager`（装配注入了真实工厂）。
//! 2. [`session_bg_complete_callback_defers_into_the_registered_session`]：装配出的
//!    `on_bg_complete` 真能路由——注册 session 后调用一次（`BgTaskKind::Shell`），
//!    该 session 的 `v2_message_queue` 恰好出现 1 条 `Defer` / `MessageSource::
//!    ShellComplete`，且唤醒可观察（`SessionInbox::await_wake` 不挂到超时）。
//!
//! 夹具形态：走**生产装配面** `assemble_server_config`（`bare = true`：跳过插件，
//! 部署层不建池、会话层仅建 workspace 池），再取 `workspace_assembly` 开关调用
//! `SessionEnvironment::assemble`。注意本文件因此是本 crate 内**测试夹具**层面的
//! `HostAssemblyInput` 字面量点之一：它传 `workspace_input: None`（顶层装配形态——
//! pool 在 `session_resources = false` 时不构造，session 级输入由每 session 的会话
//! 环境装配产生），与生产三路径一致。
//!
//! 这里**不**断言 builtin `workspace` 实例内部的 `BashTool` 字段：池的
//! `BuiltinInstanceContext` 读取面（`builtin_instance_context()`）是
//! `peri-middlewares` 的 `pub(crate)`，宿主不可见（A33）。本文件断言到**注入面**
//! 为止（送进上下文的那份 == 环境持有 == 会话持有）；实例内部如何消费输入由
//! `peri-middlewares` 侧的 wave 3 用例覆盖。

use std::sync::Arc;

use peri_acp_types::{
    event::BackgroundTaskResult,
    permission::{PermissionMode, SharedPermissionMode},
    session::{MessageKind, MessageSource},
    tasks::BgTaskKind,
};

use super::workspace::SessionEnvironment;
use crate::host::assemble::{assemble_server_config, HostAssemblyInput};
use crate::provider::{ConfigSource, LlmProvider, PeriConfig, ProviderConfig, ProviderModels};

// ── 夹具 ─────────────────────────────────────────────────────────────────────

/// 生产同构的 host cfg + 规范化 cwd（`SessionEnvironment::assemble` 的
/// `same_directory` 分支要求 `canonicalize(startup_cwd) == cwd`）。
struct SeamFixture {
    cfg: crate::host::AcpServerConfig,
    cwd: String,
    _tmp: tempfile::TempDir,
}

async fn seam_fixture() -> SeamFixture {
    let tmp = tempfile::TempDir::new().unwrap();
    // macOS 上 `TempDir` 的原始路径（/var/...）与规范化路径（/private/var/...）
    // 不同：装配面的同目录判定用 `canonicalize(startup_cwd) == cwd`，故 cwd 取规范形态。
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();

    let peri_config = make_peri_config_with_provider(make_provider_config());
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let config_source = Arc::new(
        ConfigSource::load_at(
            &tmp.path().join("empty-cwd"),
            tmp.path().join("test_config.json"),
        )
        .expect("夹具配置源"),
    );

    let cfg = assemble_server_config(HostAssemblyInput {
        provider,
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        config_source,
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        session_resources,
        session_store_shutdown: None,
        cwd: cwd.clone(),
        // bare：跳过插件/settings hooks，会话只建 workspace 池。
        // 本文件验证激活前的输入注入与关闭，初始化受 activation 闸门保护。
        bare: true,
        drive_cron_tick: false,
        workspace_input: None,
        prepared_plugins: None,
    })
    .await;

    SeamFixture {
        cfg,
        cwd,
        _tmp: tmp,
    }
}

fn make_provider_config() -> ProviderConfig {
    ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn make_peri_config_with_provider(provider: ProviderConfig) -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![provider];
    peri_config
}

/// `BgTaskKind::Shell` 的完成 DTO 夹具（wave 3 用例面只用这些字段）。
fn shell_bg_result() -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: "bg-aw3-11".to_string(),
        agent_name: "shell".to_string(),
        prompt_summary: "sleep 0".to_string(),
        success: true,
        output: "done".to_string(),
        tool_calls_count: 0,
        duration_ms: 7,
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
}

// ── 用例 ─────────────────────────────────────────────────────────────────────

/// AW3-11 第一/二成员：装配面送进 builtin 上下文的那份 == 环境持有的那份 ==
/// 会话持有的那份（同一 `Arc`），且不是退化 fallback。
#[tokio::test]
async fn session_environment_holds_the_workspace_input_it_injected() {
    let fixture = seam_fixture().await;
    let session_id = "aw3-11-seam-b";
    let environment = SessionEnvironment::assemble(&fixture.cfg, &fixture.cwd, session_id)
        .await
        .expect("装配成功")
        .expect("workspace_assembly 为 Some ⇒ 必须产出会话环境");

    let input = environment
        .workspace_input()
        .expect("环境保留装配出的 workspace 输入");
    let injected = input
        .task_manager
        .as_ref()
        .expect("session 级输入必须携带 per-session TaskManager");
    assert!(
        Arc::ptr_eq(injected, &environment.task_manager()),
        "送进 builtin 上下文的那份 manager 必须就是环境持有的那份（同一 Arc）"
    );
    assert!(
        input.on_bg_complete.is_some(),
        "session 级输入必须携带 on_bg_complete（AW3-11 第二成员）"
    );
    assert!(
        !environment
            .task_manager()
            .as_any()
            .is::<peri_acp_types::tasks::NoopTaskManager>(),
        "host cfg 经装配面注入了真实工厂 ⇒ 不得 fallback NoopTaskManager"
    );

    // 第三个落点：会话登记用的是同一份（不是等值的第二份）。
    environment
        .cfg
        .session_manager
        .ensure_session_with_task_manager(
            session_id,
            &fixture.cwd,
            Some(environment.task_manager()),
        );
    let stored = Arc::clone(
        &environment
            .cfg
            .session_manager
            .get_session(session_id)
            .expect("会话已登记")
            .task_manager,
    );
    assert!(
        Arc::ptr_eq(&stored, injected),
        "AcpSession::task_manager 必须与送进 builtin 的那份同一 Arc"
    );

    assert!(environment.shutdown().await, "空会话环境应有界关闭");
}

/// AW3-11 第二成员的行为面：session 级 `on_bg_complete` 把 Shell 完成按
/// `Defer` / `MessageSource::ShellComplete` 投进**该 session** 的收件箱并唤醒。
#[tokio::test]
async fn session_bg_complete_callback_defers_into_the_registered_session() {
    let fixture = seam_fixture().await;
    let session_id = "aw3-11-seam-c";
    let environment = SessionEnvironment::assemble(&fixture.cfg, &fixture.cwd, session_id)
        .await
        .expect("装配成功")
        .expect("workspace_assembly 为 Some ⇒ 必须产出会话环境");
    environment
        .cfg
        .session_manager
        .ensure_session_with_task_manager(
            session_id,
            &fixture.cwd,
            Some(environment.task_manager()),
        );

    let inbox = environment
        .cfg
        .session_manager
        .session_inbox_for(session_id)
        .expect("session 已登记 ⇒ inbox 可 lazy 解析");
    assert!(
        !inbox.queue().has_wake_up(),
        "调用前队列必须无 wake-able 消息（否则唤醒断言无意义）"
    );

    // 装配出的回调（闭包体内 lazy resolve inbox）：注册后调用必须命中本 session。
    let on_bg_complete = environment
        .workspace_input()
        .expect("环境保留装配出的 workspace 输入")
        .on_bg_complete
        .clone()
        .expect("on_bg_complete 已装配");
    on_bg_complete(&shell_bg_result(), BgTaskKind::Shell);

    tokio::time::timeout(std::time::Duration::from_secs(1), inbox.await_wake())
        .await
        .expect("Shell 完成必须唤醒 idle 会话循环（await_wake 超时未返回）");

    let drained = inbox.queue().drain_all();
    assert_eq!(drained.len(), 1, "恰好 1 条 Defer，不得多投或漏投");
    assert_eq!(drained[0].kind, MessageKind::Defer);
    assert_eq!(drained[0].source, MessageSource::ShellComplete);

    assert!(environment.shutdown().await, "空会话环境应有界关闭");
}
