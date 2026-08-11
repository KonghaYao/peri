//! ACP Stdio 环境的初始化逻辑。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use crate::provider::LlmProvider;
use parking_lot::RwLock;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::SettingsHooksPort;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::ports::{McpPoolPort, SkillsPort, ToolSearchPort};
use peri_acp_types::store::ThreadStore;

use super::context::StdioContext;

/// stdio 宿主装配输入（M-TUI 收口：middlewares 具体实现由本装配面内部
/// 构造——「ACP Host = 部署单元」；cli 只提供协议面输入）。
pub struct StdioAssemblyInput {
    pub cwd: String,
    pub permission_mode: Arc<SharedPermissionMode>,
    /// 显式指定 SQLite 会话数据库路径；`None` 保持默认路径 + fallback
    /// 临时目录行为（`open_thread_store_with`）。
    pub db_path: Option<PathBuf>,
}

/// 初始化 ACP Stdio 运行环境，返回共享上下文。
///
/// 执行顺序：cwd 解析 → config/provider → hooks 组 → permission →
/// thread store → langfuse → 组装 StdioContext。
///
/// cron/MCP 池/工具检索索引/插件数据由部署装配点（cli 白名单文件）构造
/// 后经 [`StdioAssemblyInput`] 注入（§0 依赖方向，`docs/top-level.md`）；
/// ACP 层不直接依赖 Resources / 业务 crate。
pub(super) async fn init_stdio_context(
    input: StdioAssemblyInput,
) -> anyhow::Result<Arc<StdioContext>> {
    let _telemetry = peri_agent::telemetry::init_tracing("peri-acp");

    // 解析工作目录
    let cwd = std::path::Path::new(&input.cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&input.cwd))
        .to_string_lossy()
        .to_string();

    // 加载配置
    let peri_config = crate::provider::load().unwrap_or_default();
    let provider = LlmProvider::from_config(&peri_config)
        .or_else(LlmProvider::from_env)
        .ok_or_else(|| anyhow::anyhow!("No LLM provider configured. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, or configure ~/.peri/settings.json"))?;

    tracing::info!(
        provider = %provider.display_name(),
        model = %provider.model_name(),
        cwd = %cwd,
        "ACP stdio mode starting"
    );

    let StdioAssemblyInput {
        cwd: input_cwd,
        permission_mode,
        db_path,
    } = input;
    let _ = input_cwd;

    // ── M-TUI 收口：middlewares 具体实现由本装配面内部构造（与 TUI/print
    //    的 `assemble_server_config` 同源；stdio 无 bare 语义、无 cron tick，
    //    行为与迁移前 main.rs stdio 装配一致）──
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude");

    let plugin_data = peri_middlewares::plugin::load_enabled_plugins_aggregated(
        &claude_dir,
        Some(std::path::Path::new(&cwd)),
    );
    let plugin_skill_roots = plugin_data.all_skill_roots;
    let plugin_agent_dirs = plugin_data.all_agent_dirs;
    let plugin_hooks = plugin_data.all_hooks;
    let plugin_loaded = plugin_data.plugins;
    // H5：全局 settings.json（config.lspServers）与插件 LSP 服务器合并
    //（优先级对齐 MCP：global < plugin；无插件时全局配置单独生效）。
    // 读取路径跟随宿主全局配置加载机制（config_path，支持测试重定向）。
    let plugin_lsp_servers = peri_middlewares::assembly::load_merged_lsp_servers(
        &crate::provider::config_path(),
        plugin_data.all_lsp_servers,
    );

    let cron_scheduler: Option<Arc<dyn CronSchedulerPort>> = {
        let scheduler = Arc::new(parking_lot::Mutex::new(
            peri_middlewares::cron::CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0),
        ));
        Some(Arc::new(peri_middlewares::cron::CronSchedulerPortHandle(
            scheduler,
        )))
    };
    let mcp_pool: Option<Arc<dyn McpPoolPort>> = {
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
    let tool_search_index: Arc<dyn ToolSearchPort> =
        Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new());
    let skills: Arc<dyn SkillsPort> = Arc::new(peri_middlewares::host_ports::SkillsProvider);
    let settings_hooks: Arc<dyn SettingsHooksPort> =
        Arc::new(peri_middlewares::host_ports::SettingsHooksLoader);
    let workflow_middleware_factory =
        peri_middlewares::assembly::default_workflow_middleware_factory();

    // thread 存储经 peri-agent 工厂构造（§0：ACP 层不直接依赖 Resources；
    // M-res 收口——存储实例化点归 Agent 层声明边）
    let thread_store: Arc<dyn ThreadStore> = peri_agent::resources::open_thread_store_with(db_path)
        .await
        .map_err(|e| anyhow::anyhow!("无法初始化 Resources 层: {e}"))?;

    // 组装 hook groups（顺序与迁移前一致：plugin → global → project → local；
    // 经 host::assemble 统一装配，ARC-MIDDLEWARE-001 链序不重排；三级 settings
    // hooks 经注入端口加载，磁盘读取留在实现方）
    let hook_groups = crate::host::assemble::assemble_hook_groups(
        &plugin_hooks,
        settings_hooks.as_ref(),
        &cwd,
        false,
    );

    let shared_tools = Arc::new(RwLock::new(BTreeMap::new()));

    // 初始化 Langfuse
    let langfuse_session =
        if let Some(config) = peri_controller::langfuse::LangfuseConfig::from_env() {
            peri_controller::langfuse::LangfuseSession::new(config, "live".into())
                .await
                .map(Arc::new)
        } else {
            None
        };
    if langfuse_session.is_some() {
        tracing::info!("Langfuse tracing enabled (stdio mode)");
    }

    // 构建 SessionManager：支撑 SubAgent cascade cancel 与 goal_state 跨 prompt 共享。
    // stdio 本地仍维护 SessionInfo（history/frozen/agent_pool 等），SessionManager
    // 只持有 AcpSession 元数据 + active_agents + goal_state。
    let session_manager = {
        let peri_config_arc = Arc::new(RwLock::new(peri_config.clone()));
        crate::host::assemble::build_session_manager(
            thread_store.clone(),
            provider.clone(),
            &peri_config_arc,
            permission_mode.clone(),
            cron_scheduler.clone(),
            skills.clone(),
        )
    };

    // 构建共享的 ServerContext，所有请求处理器通过 Arc 共享
    Ok(Arc::new(StdioContext {
        provider: Arc::new(RwLock::new(provider)),
        peri_config: RwLock::new(peri_config),
        permission_mode,
        cron_scheduler: cron_scheduler
            .clone()
            .expect("stdio cron scheduler 由宿主装配点注入"),
        mcp_pool,
        channel_state: None,
        plugin_skill_roots,
        plugin_agent_dirs,
        plugin_loaded,
        hook_groups,
        plugin_lsp_servers,
        tool_search_index,
        skills,
        shared_tools,
        workflow_middleware_factory,
        sessions: RwLock::new(HashMap::new()),
        thread_store: thread_store.clone(),
        controller: Arc::new(peri_controller::Controller::new(thread_store.clone())),
        langfuse_session,
        session_manager,
    }))
}
