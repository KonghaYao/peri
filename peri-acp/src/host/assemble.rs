//! ACP Host 装配——TUI / print / stdio 三路径共用的 host 装配函数。
//!
//! 3.0 目标（`docs/top-level.md` §7/§8）：ACP Host = 部署单元，由 cli/TUI 作为
//! 部署装配点启动；客户端只经 ACP 拿数据。本模块收拢三处此前各自复制的主机
//! 装配（`launch.rs` 内嵌 server 装配、`cli_print.rs` 业务装配、stdio init），
//! 避免装配逻辑漂移。中间件链序事实源仍为 Agent 层 session 工厂
//! （ARC-MIDDLEWARE-001）：本模块只组装 hook 组（顺序与迁移前一致），
//! 不参与链序蓝本。

use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::{RegisteredHook, SettingsHooksPort};
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::{PluginLoadResult, PluginManagerPort};
use peri_acp_types::ports::{McpPoolPort, SkillsPort, ToolSearchPort};
use peri_acp_types::store::ThreadStore;

use crate::provider::{config_path, LlmProvider, PeriConfig};
use crate::session::SessionManager;

use super::AcpServerConfig;

/// host 装配输入：调用方（cli/TUI/print/stdio）持有的轻量输入。
///
/// M-TUI 收口（`spec/issues/2026-08-05-3.0-m-tui-acp-client-path.md`）：
/// middlewares 具体实现（CronScheduler / McpClientPool / ToolSearchIndex /
/// SkillsProvider / PluginManager / SettingsHooksLoader /
/// WorkflowAgentMiddlewareFactory / 插件聚合数据）全部由本装配面内部构造
/// ——「ACP Host = 部署单元」，TUI/print/stdio 只提供协议面输入
/// （provider / config / permission / thread_store / cwd），不再直接触碰
/// 业务 crate（§0 依赖方向，`docs/top-level.md` §7/§8）。
pub struct HostAssemblyInput {
    pub provider: LlmProvider,
    pub peri_config: Arc<RwLock<PeriConfig>>,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub thread_store: Arc<dyn ThreadStore>,
    /// 工作目录（用于加载 project/local settings hooks）
    pub cwd: String,
    /// 跳过 settings hooks / LSP / 插件（print --bare 语义）
    pub bare: bool,
    /// 驱动 cron tick（TUI=true，复刻迁移前 TUI 每秒 tick 行为；print/stdio
    /// 保持现状无 tick——行为零变化，L2 遗留登记 M-TUI issue）。
    pub drive_cron_tick: bool,
}

/// 组装 settings hook 组（plugin → global → project → local，顺序即迁移前
/// TUI/print/stdio 三处一致的既有顺序，ARC-MIDDLEWARE-001 不重排）。
///
/// `skip_settings_hooks`：bare 模式跳过 global/project/local（与 print 既有语义
/// 一致）；plugin hooks 为空时不产生空组。三级 settings hooks 经
/// [`SettingsHooksPort`] 注入（装配点构造，磁盘加载留在实现方）。
pub fn assemble_hook_groups(
    plugin_hooks: &[RegisteredHook],
    settings_hooks: &dyn SettingsHooksPort,
    cwd: &str,
    skip_settings_hooks: bool,
) -> Vec<Vec<RegisteredHook>> {
    let mut hook_groups: Vec<Vec<RegisteredHook>> = Vec::new();
    if !plugin_hooks.is_empty() {
        hook_groups.push(plugin_hooks.to_vec());
    }
    if skip_settings_hooks {
        return hook_groups;
    }
    let global_hooks = settings_hooks.global();
    if !global_hooks.is_empty() {
        hook_groups.push(global_hooks);
    }
    let project_hooks = settings_hooks.project(cwd);
    if !project_hooks.is_empty() {
        hook_groups.push(project_hooks);
    }
    let local_hooks = settings_hooks.local(cwd);
    if !local_hooks.is_empty() {
        hook_groups.push(local_hooks);
    }
    hook_groups
}

/// 构造共享 SessionManager（支撑 cascade cancel 子 agent 与 goal_state）。
///
/// 装配细节与迁移前 `launch.rs` / `cli_print.rs` / stdio init 三处一致：
/// peri_config 冻结快照 + cron scheduler（可选）注入。
pub fn build_session_manager(
    thread_store: Arc<dyn ThreadStore>,
    provider: LlmProvider,
    peri_config: &Arc<RwLock<PeriConfig>>,
    permission_mode: Arc<SharedPermissionMode>,
    cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    skills: Arc<dyn SkillsPort>,
) -> SessionManager {
    let peri_config_snapshot = Arc::new(peri_config.read().clone());
    SessionManager::new(
        thread_store,
        provider,
        peri_config_snapshot,
        permission_mode,
        None,
        cron_scheduler,
        // 装配注入面：per-session 后台任务管理器（Agent 层实现，per-session
        // 聚合：registry + bg shell 执行），由本装配点构造后注入（全路径引用）；
        // ACP 协议面只持有契约 `peri_acp_types::tasks::TaskManager`。
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        skills,
    )
}

/// 组装完整的 ACP host 配置（TUI / print 路径入口）。
///
/// 自迁移前 `launch.rs` 的内嵌 server 装配原样搬移：hook 组加载、tool search
/// index、shared tools、Langfuse（环境启用时创建）、SessionManager。
///
/// M-TUI 收口：middlewares 具体实现（cron / MCP 池 / 工具检索索引 / skills /
/// plugin / settings hooks / workflow 装配端口）与插件聚合数据在本装配面
/// 内部构造（`peri-middlewares` 引用豁免见 `scripts/import-exemptions.conf`
/// 边 2 assemble 路径）；行为与迁移前三路径（launch / cli_print / stdio）
/// 各自装配一致（cron tick 驱动、MCP 初始化、孤儿插件清理时机均复刻）。
pub async fn assemble_server_config(input: HostAssemblyInput) -> AcpServerConfig {
    let HostAssemblyInput {
        provider,
        peri_config,
        permission_mode,
        thread_store,
        cwd,
        bare,
        drive_cron_tick,
    } = input;

    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude");

    // ── 插件聚合数据（bare 时跳过；迁移前 TUI launch / cli_print 各自构造）──
    let plugin_data: Option<PluginLoadResult> = if bare {
        None
    } else {
        Some(peri_middlewares::plugin::load_enabled_plugins_aggregated(
            &claude_dir,
            Some(std::path::Path::new(&cwd)),
        ))
    };

    // ── cron 调度器（迁移前 TUI launch / cli_print 各自构造；tick 驱动仅
    //    TUI 复刻——drive_cron_tick flag，L2 遗留登记 M-TUI issue）──
    let cron_scheduler: Option<Arc<dyn CronSchedulerPort>> = {
        let scheduler = Arc::new(parking_lot::Mutex::new(
            peri_middlewares::cron::CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0),
        ));
        if drive_cron_tick {
            let tick_scheduler = scheduler.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
                loop {
                    interval.tick().await;
                    tick_scheduler.lock().tick();
                }
            });
        }
        Some(Arc::new(peri_middlewares::cron::CronSchedulerPortHandle(
            scheduler,
        )))
    };

    // ── MCP 连接池（bare 时跳过；后台初始化不阻塞，迁移前 cli_print 语义）──
    let mcp_pool: Option<Arc<dyn McpPoolPort>> = if bare {
        None
    } else {
        let pool = Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
        let pool_clone = pool.clone();
        let cwd_clone = cwd.clone();
        let claude_home_clone = claude_dir.clone();
        let (init_tx, _init_rx) =
            tokio::sync::watch::channel(peri_middlewares::mcp::McpInitStatus::Pending);
        tokio::spawn(async move {
            peri_middlewares::mcp::McpClientPool::run_initialize(
                pool_clone,
                std::path::Path::new(&cwd_clone),
                &claude_home_clone,
                init_tx,
                None,
                None,
            )
            .await;
        });
        Some(pool)
    };

    // ── 资源类/业务面端口默认实现（构造下沉：ACP Host = 部署单元）──
    let tool_search_index: Arc<dyn ToolSearchPort> =
        Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new());
    let skills: Arc<dyn SkillsPort> = Arc::new(peri_middlewares::host_ports::SkillsProvider);
    let plugin_manager: Arc<dyn PluginManagerPort> =
        Arc::new(peri_middlewares::host_ports::PluginManager);
    let settings_hooks: Arc<dyn SettingsHooksPort> =
        Arc::new(peri_middlewares::host_ports::SettingsHooksLoader);
    let workflow_middleware_factory =
        peri_middlewares::assembly::default_workflow_middleware_factory();

    // E2：启动时清理孤儿插件文件（迁移前 TUI launch 行为；bare 时跳过）
    if !bare {
        let claude_dir_clone = claude_dir.clone();
        tokio::spawn(async move {
            if let Err(e) =
                peri_middlewares::plugin::cleanup_orphaned_plugins(&claude_dir_clone).await
            {
                tracing::warn!(target: "peri", error = %e, "启动时清理孤儿插件文件失败");
            } else {
                tracing::info!(target: "peri", "启动时清理孤儿插件文件完成");
            }
        });
    }

    let plugin_skill_roots = plugin_data
        .as_ref()
        .map(|pd| pd.all_skill_roots.clone())
        .unwrap_or_default();
    let plugin_agent_dirs = plugin_data
        .as_ref()
        .map(|pd| pd.all_agent_dirs.clone())
        .unwrap_or_default();
    // H5：全局 settings.json（config.lspServers）与插件 LSP 服务器合并
    //（优先级对齐 MCP：global < plugin；无插件时全局配置单独生效）。
    // 读取路径跟随宿主全局配置加载机制（config_path，支持测试重定向）。
    let plugin_lsp_servers = peri_middlewares::assembly::load_merged_lsp_servers(
        &crate::provider::config_path(),
        plugin_data
            .as_ref()
            .map(|pd| pd.all_lsp_servers.clone())
            .unwrap_or_default(),
    );
    let plugin_hooks = plugin_data
        .as_ref()
        .map(|pd| pd.all_hooks.clone())
        .unwrap_or_default();
    let plugin_loaded = plugin_data
        .as_ref()
        .map(|pd| pd.plugins.clone())
        .unwrap_or_default();

    let hook_groups = assemble_hook_groups(&plugin_hooks, settings_hooks.as_ref(), &cwd, bare);
    let flat_hooks: Vec<RegisteredHook> = hook_groups.iter().flatten().cloned().collect();
    tracing::info!(
        groups = hook_groups.len(),
        total_hooks = flat_hooks.len(),
        "Hook groups assembled for ACP host"
    );

    let shared_tools = Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::new()));

    let session_manager = build_session_manager(
        thread_store.clone(),
        provider.clone(),
        &peri_config,
        permission_mode.clone(),
        cron_scheduler.clone(),
        skills.clone(),
    );

    // Langfuse 观测（与迁移前 TUI/stdio/print 一致：环境启用时创建）
    let langfuse_session =
        if let Some(config) = peri_controller::langfuse::LangfuseConfig::from_env() {
            tracing::info!("Langfuse tracing enabled (host mode)");
            peri_controller::langfuse::LangfuseSession::new(config, "live".into())
                .await
                .map(Arc::new)
        } else {
            None
        };

    AcpServerConfig {
        provider: Arc::new(RwLock::new(provider)),
        peri_config,
        permission_mode,
        cron_scheduler,
        mcp_pool,
        channel_state: None, // ServiceRegistry.channel_state 已删除
        plugin_skill_roots,
        plugin_agent_dirs,
        plugin_hooks: flat_hooks,
        // 仅插件 hooks（hooks 面板数据源；plugin/list 命令面返回，TUI 不再
        // 直读 plugin_data）
        plugin_hooks_only: plugin_hooks,
        plugin_loaded,
        hook_groups,
        plugin_lsp_servers,
        tool_search_index,
        skills,
        plugin_manager,
        settings_hooks,
        shared_tools,
        workflow_middleware_factory,
        thread_store: thread_store.clone(),
        controller: Arc::new(peri_controller::Controller::new(thread_store.clone())),
        langfuse_session,
        config_path: config_path(),
        session_manager,
    }
}
