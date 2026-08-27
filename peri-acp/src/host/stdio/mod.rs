//! ACP Stdio 模式：通过 stdin/stdout JSON-RPC 与 IDE client 通信。
//!
//! 批 3（acp-host-unify）：stdio 业务处理整体切换到统一宿主
//! [`super::run_acp_server`]——删除 typed handler 层（`stdio/session/*`）与
//! `StdioContext`，[`StdioTransport`] 作为 [`AcpTransport`] 多态实现接入。
//! 装配（`assemble_server_config`，与 TUI/print 同源）收拢在
//! [`super::assemble`]，本模块只做部署装配点职责：协议面输入 → 装配 →
//! transport 挂载（含 legacy `type:cancel` 全 session 兜底中断钩子）。
//!
//! stdio host 位于 ACP 层（部署装配点，`docs/top-level.md` §7/§19）；外部
//! 系统通道（thread 存储）由部署单元（cli）打开后经 `thread_store` 注入，
//! ACP 层不直接依赖 Resources（§0 依赖方向）。

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::store::ThreadStore;

use crate::provider::LlmProvider;
use crate::transport::stdio::StdioTransport;
use crate::transport::AcpTransport;

/// stdio 宿主装配输入（M-TUI 收口：middlewares 具体实现由装配面内部
/// 构造——「ACP Host = 部署单元」；cli 只提供协议面输入）。
pub struct StdioInput {
    pub cwd: String,
    pub permission_mode: Arc<SharedPermissionMode>,
    /// 显式指定 SQLite 会话数据库路径；`None` 保持默认路径 + fallback
    /// 临时目录行为（`open_thread_store_with`）。
    pub db_path: Option<PathBuf>,
}

/// 启动 ACP stdio 宿主（批 3：统一宿主 `run_acp_server` 接管全部业务处理）。
///
/// 装配输入（cron/MCP 池/工具检索索引/插件数据等具体实现）由部署装配点
/// （cli 白名单文件，见 `peri-tui/src/main.rs`）构造后经 [`StdioInput`] 注入；
/// ACP 层只持端口接口（3.0 批 2 波 2，§0 依赖方向）。
pub async fn run_acp_stdio(input: StdioInput) -> anyhow::Result<()> {
    let cfg = assemble_stdio_config(input).await?;
    let cancel_task_spawner = cfg.host_task_spawner.clone();

    // 共享 session 集合：legacy `type:cancel` 全 session 兜底中断回调与宿主
    // `run_acp_server` 遍历**同一 map**（回调在构造 transport 时注入，因此
    // session map 必须由本装配点创建并经 `run_acp_server_with_sessions` 注入）。
    let sessions: super::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let cancel_sessions = sessions.clone();
    let transport = StdioTransport::new().with_cancel_hook(Some(Arc::new(move |_line| {
        // 全 session 兜底中断（无 sessionId）：遍历全部 SessionState 对
        // `cancel_token.cancel()`。与标准 `session/cancel`（按 sessionId +
        // writer lease + continuation 武装，`host/notify.rs`）并存——type:cancel
        // 无客户端身份、无续跑语义，仅作 IDE 强停兜底（批 3 §7 #10）。
        let sessions = cancel_sessions.clone();
        let _ = cancel_task_spawner.spawn(
            super::task_scope::HostTaskOwnerKind::Host,
            super::task_scope::HostTaskKind::LegacyCancelHook,
            async move {
                let sessions = sessions.lock().await;
                for (sid, state) in sessions.iter() {
                    if let Some(ref token) = state.cancel_token {
                        token.cancel();
                        tracing::info!(session_id = %sid, "Cancelled via type:cancel");
                    }
                }
            },
        );
    })));
    super::run_acp_server_with_sessions(
        Arc::new(transport) as Arc<dyn AcpTransport>,
        cfg,
        sessions,
    )
    .await;
    Ok(())
}

/// 批 3 并入 `run_acp_stdio`（原 `host/stdio/init.rs::init_stdio_context`，已删）。
///
/// 执行顺序：cwd 解析（canonicalize）→ config/provider → thread store →
/// config source → `assemble_server_config`（与 TUI `launch.rs` 同构的
/// **统一装配**，middlewares/插件/Langfuse/session_manager 全数由
/// [`crate::host::assemble::assemble_server_config`] 构造；stdio 无 bare 语义、
/// 无 cron tick）。
async fn assemble_stdio_config(input: StdioInput) -> anyhow::Result<super::AcpServerConfig> {
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

    let StdioInput {
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
    let apps_enabled = std::env::var_os(peri_acp_types::mcp_apps::MCP_APPS_ENV).is_some();
    let mut cfg = crate::host::assemble::assemble_server_config_with_mcp_apps(
        crate::host::assemble::HostAssemblyInput {
            provider,
            peri_config: Arc::new(RwLock::new(peri_config)),
            config_source,
            permission_mode,
            thread_store,
            cwd: cwd.clone(),
            bare: false,
            drive_cron_tick: false,
        },
        apps_enabled,
    )
    .await;

    // stdio 部署过滤 rewind/clear（IDE 客户端自管理；不拦截，fall-through 进模型）。
    cfg.stdio_command_filter = true;

    Ok(cfg)
}

#[cfg(test)]
#[path = "run_server_integration_test.rs"]
mod run_server_integration_tests;
