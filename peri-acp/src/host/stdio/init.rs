//! ACP Stdio 环境的初始化逻辑。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::provider::LlmProvider;
use parking_lot::RwLock;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::store::ThreadStore;

use super::context::StdioContext;

/// stdio 宿主装配输入（M-TUI 收口：middlewares 具体实现由装配面内部
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
/// 执行顺序：cwd 解析（canonicalize）→ config/provider → thread store →
/// config source → `assemble_server_config`（与 TUI `launch.rs` 同构的
/// **统一装配**，middlewares/插件/Langfuse/session_manager 全数由
/// [`crate::host::assemble::assemble_server_config`] 构造；stdio 无 bare 语义、
/// 无 cron tick）→ 包 `StdioContext { cfg, sessions }`。
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

    // thread 存储经 peri-agent 工厂构造（§0：ACP 层不直接依赖 Resources；
    // M-res 收口——存储实例化点归 Agent 层声明边）
    let thread_store: Arc<dyn ThreadStore> = peri_agent::resources::open_thread_store_with(db_path)
        .await
        .map_err(|e| anyhow::anyhow!("无法初始化 Resources 层: {e}"))?;

    // 配置源（读写路径决策的唯一事实源）：stdio 的 cwd 语义由 cli 传入，
    // 必须按 canonicalize 后的 input.cwd 探测工作区布局——不能用
    // `ConfigSource::load()` 的进程 cwd。失败时回落 lenient（与上方
    // `provider::load().unwrap_or_default()` 的宽松语义一致，文件损坏
    // 按空配置继续并保留路径决策）。
    let config_source = Arc::new(
        crate::provider::ConfigSource::load_at(
            std::path::Path::new(&cwd),
            crate::provider::config_path(),
        )
        .unwrap_or_else(|_| {
            crate::provider::ConfigSource::load_at_lenient(
                std::path::Path::new(&cwd),
                crate::provider::config_path(),
            )
        }),
    );

    // ── M-TUI 收口：middlewares 具体实现（CronScheduler / McpClientPool /
    //    ToolSearchIndex / SkillsProvider / PluginManager / SettingsHooksLoader /
    //    插件聚合数据 / Langfuse / SessionManager）由 host 装配面统一构造
    //    （与 TUI/print 的 `assemble_server_config` 同源）；stdio 无 bare
    //    语义、无 cron tick。MCP 初始化（run_initialize）、孤儿插件清理即
    //    在 assemble 内部完成，此处不再重复 spawn。──
    let cfg =
        crate::host::assemble::assemble_server_config(crate::host::assemble::HostAssemblyInput {
            provider,
            peri_config: Arc::new(RwLock::new(peri_config)),
            config_source,
            permission_mode,
            thread_store,
            cwd: cwd.clone(),
            bare: false,
            drive_cron_tick: false,
        })
        .await;

    // 构建共享的 StdioContext，所有请求处理器通过 Arc 共享
    Ok(Arc::new(StdioContext {
        cfg,
        sessions: RwLock::new(HashMap::new()),
    }))
}
