//! ACP Stdio 环境的初始化逻辑。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use crate::provider::LlmProvider;
use parking_lot::RwLock;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::{RegisteredHook, SettingsHooksPort};
use peri_acp_types::lsp::LspServerConfig;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::LoadedPlugin;
use peri_acp_types::ports::{McpPoolPort, SkillsPort, ToolSearchPort};
use peri_acp_types::skills::SkillRoot;
use peri_acp_types::store::ThreadStore;

use super::context::StdioContext;

/// stdio 宿主装配输入（3.0 批 2 波 2）：具体实现由部署装配点（cli/TUI
/// 白名单文件）构造后以端口/协议类型注入；ACP 侧只持接口。
pub struct StdioAssemblyInput {
    pub cwd: String,
    pub thread_store: Arc<dyn ThreadStore>,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    pub mcp_pool: Option<Arc<dyn McpPoolPort>>,
    pub tool_search_index: Arc<dyn ToolSearchPort>,
    pub skills: Arc<dyn SkillsPort>,
    pub settings_hooks: Arc<dyn SettingsHooksPort>,
    pub plugin_skill_roots: Vec<SkillRoot>,
    pub plugin_agent_dirs: Vec<PathBuf>,
    pub plugin_hooks: Vec<RegisteredHook>,
    pub plugin_loaded: Vec<LoadedPlugin>,
    pub plugin_lsp_servers: Vec<LspServerConfig>,
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
        cron_scheduler,
        mcp_pool,
        tool_search_index,
        skills,
        settings_hooks,
        permission_mode,
        plugin_skill_roots,
        plugin_agent_dirs,
        plugin_hooks,
        plugin_loaded,
        plugin_lsp_servers,
        thread_store,
        ..
    } = input;

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

    // thread 存储由部署装配点（cli）注入（§0：ACP 层不直接依赖 Resources）
    let thread_store: Arc<dyn ThreadStore> = thread_store;

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
        sessions: RwLock::new(HashMap::new()),
        thread_store: thread_store.clone(),
        controller: Arc::new(peri_controller::Controller::new(thread_store.clone())),
        langfuse_session,
        session_manager,
    }))
}
