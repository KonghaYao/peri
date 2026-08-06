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

/// host 装配输入：调用方（cli/TUI/print/stdio）已持有的组件。
pub struct HostAssemblyInput {
    pub provider: LlmProvider,
    pub peri_config: Arc<RwLock<PeriConfig>>,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    pub mcp_pool: Option<Arc<dyn McpPoolPort>>,
    /// 工具检索索引端口（宿主装配点构造 `ToolSearchIndex` 后 upcast 注入；
    /// ACP 侧不直接 new 资源类）。
    pub tool_search_index: Arc<dyn ToolSearchPort>,
    /// Skills 扫描端口（宿主装配点构造 `SkillsProvider` 后注入；命令面
    /// available-commands / agents 扫描经此访问，ACP 侧不直调业务面）。
    pub skills: Arc<dyn SkillsPort>,
    /// 插件管理端口（plugin/install 等命令面经此访问；实现留在实现方）。
    pub plugin_manager: Arc<dyn PluginManagerPort>,
    /// Settings hooks 加载端口（global/project/local 三级；装配面只做组序组合）。
    pub settings_hooks: Arc<dyn SettingsHooksPort>,
    pub thread_store: Arc<dyn ThreadStore>,
    /// 工作目录（用于加载 project/local settings hooks）
    pub cwd: String,
    /// 已加载的插件聚合数据（bare 模式为 None）
    pub plugin_data: Option<PluginLoadResult>,
    /// 跳过 settings hooks / LSP / 插件（print --bare 语义）
    pub bare: bool,
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
pub async fn assemble_server_config(input: HostAssemblyInput) -> AcpServerConfig {
    let HostAssemblyInput {
        provider,
        peri_config,
        permission_mode,
        cron_scheduler,
        mcp_pool,
        tool_search_index,
        skills,
        plugin_manager,
        settings_hooks,
        thread_store,
        cwd,
        plugin_data,
        bare,
    } = input;

    let plugin_skill_roots = plugin_data
        .as_ref()
        .map(|pd| pd.all_skill_roots.clone())
        .unwrap_or_default();
    let plugin_agent_dirs = plugin_data
        .as_ref()
        .map(|pd| pd.all_agent_dirs.clone())
        .unwrap_or_default();
    let plugin_lsp_servers = plugin_data
        .as_ref()
        .map(|pd| pd.all_lsp_servers.clone())
        .unwrap_or_default();
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
        plugin_loaded,
        hook_groups,
        plugin_lsp_servers,
        tool_search_index,
        skills,
        plugin_manager,
        settings_hooks,
        shared_tools,
        thread_store: thread_store.clone(),
        controller: Arc::new(peri_controller::Controller::new(thread_store.clone())),
        langfuse_session,
        config_path: config_path(),
        session_manager,
    }
}
