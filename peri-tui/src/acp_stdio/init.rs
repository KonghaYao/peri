//! ACP Stdio 环境的初始化逻辑。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use peri_agent::thread::ThreadStore;
use peri_middlewares::cron::CronScheduler;
use peri_middlewares::hooks::RegisteredHook;
use peri_middlewares::mcp::{McpClientPool, McpInitStatus};
use peri_middlewares::prelude::{PermissionMode, SharedPermissionMode};
use peri_middlewares::tool_search::ToolSearchIndex;
use peri_tui::app::agent::LlmProvider;

use super::context::StdioContext;

/// 初始化 ACP Stdio 运行环境，返回共享上下文。
///
/// 执行顺序：cwd 解析 → config/provider → cron → MCP 池 → 插件 → hooks →
/// permission → thread store → langfuse → 组装 StdioContext。
pub(super) async fn init_stdio_context(cwd: String) -> anyhow::Result<Arc<StdioContext>> {
    let _telemetry = peri_agent::telemetry::init_tracing("peri-acp");

    // 解析工作目录
    let cwd = std::path::Path::new(&cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&cwd))
        .to_string_lossy()
        .to_string();

    // 加载配置
    let peri_config = peri_tui::config::load().unwrap_or_default();
    let provider = LlmProvider::from_config(&peri_config)
        .or_else(LlmProvider::from_env)
        .ok_or_else(|| anyhow::anyhow!("No LLM provider configured. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, or configure ~/.peri/settings.json"))?;

    tracing::info!(
        provider = %provider.display_name(),
        model = %provider.model_name(),
        cwd = %cwd,
        "ACP stdio mode starting"
    );

    // 初始化 cron scheduler
    let cron_scheduler = {
        let scheduler = CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0);
        Arc::new(Mutex::new(scheduler))
    };

    // 初始化 MCP 连接池（后台）
    let mcp_pool = {
        let pool = Arc::new(McpClientPool::new_pending());
        let pool_clone = pool.clone();
        let (init_tx, _init_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        let cwd_clone = cwd.clone();
        let claude_home = dirs_next::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude");
        tokio::spawn(async move {
            McpClientPool::run_initialize(
                pool_clone,
                std::path::Path::new(&cwd_clone),
                &claude_home,
                init_tx,
                None,
                None,
            )
            .await;
        });
        Some(pool)
    };

    // 加载插件数据
    let claude_dir = dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude");
    let plugin_data = peri_middlewares::plugin::load_enabled_plugins_aggregated(&claude_dir);

    let plugin_skill_dirs = plugin_data.all_skill_dirs.clone();
    let plugin_agent_dirs = plugin_data.all_agent_dirs.clone();
    let plugin_lsp_servers = plugin_data.all_lsp_servers.clone();
    let plugin_hooks = plugin_data.all_hooks.clone();

    // 组装 hook groups
    let mut hook_groups: Vec<Vec<RegisteredHook>> = Vec::new();
    if !plugin_hooks.is_empty() {
        hook_groups.push(plugin_hooks);
    }
    let global_hooks = peri_middlewares::hooks::loader::load_global_settings_hooks();
    if !global_hooks.is_empty() {
        hook_groups.push(global_hooks);
    }
    let local_hooks = peri_middlewares::hooks::loader::load_settings_local_hooks(&cwd);
    if !local_hooks.is_empty() {
        hook_groups.push(local_hooks);
    }

    let permission_mode = SharedPermissionMode::new(PermissionMode::Bypass);
    let tool_search_index = Arc::new(ToolSearchIndex::new());
    let shared_tools = Arc::new(RwLock::new(HashMap::new()));

    // 初始化 thread 存储（失败时 fallback 到临时目录）
    let thread_store: Arc<dyn ThreadStore> =
        match peri_tui::thread::SqliteThreadStore::default_path().await {
            Ok(store) => Arc::new(store),
            Err(_) => Arc::new(
                peri_tui::thread::SqliteThreadStore::new(
                    std::env::temp_dir().join("zen-threads.db"),
                )
                .await
                .expect("无法创建临时 SQLite 数据库"),
            ),
        };

    // 初始化 Langfuse
    let langfuse_session = if let Some(config) = peri_acp::langfuse::LangfuseConfig::from_env() {
        peri_acp::langfuse::LangfuseSession::new(config)
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
        let peri_config_snapshot = peri_config.clone();
        let provider_snapshot = provider.clone();
        peri_acp::session::SessionManager::new(
            thread_store.clone(),
            provider_snapshot,
            Arc::new(peri_config_snapshot),
            permission_mode.clone(),
            None,
        )
    };

    // 构建共享的 ServerContext，所有请求处理器通过 Arc 共享
    Ok(Arc::new(StdioContext {
        provider: RwLock::new(provider),
        peri_config: RwLock::new(peri_config),
        permission_mode,
        cron_scheduler,
        mcp_pool,
        channel_state: None,
        plugin_skill_dirs,
        plugin_agent_dirs,
        hook_groups,
        plugin_lsp_servers,
        tool_search_index,
        shared_tools,
        sessions: RwLock::new(HashMap::new()),
        thread_store,
        langfuse_session,
        session_manager,
    }))
}
