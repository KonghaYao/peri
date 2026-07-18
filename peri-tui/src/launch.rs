//! TUI 启动共享层——App + ACP server/client 构建与拆解。
//!
//! 把 App 初始化、ACP server/client 配对、插件/Hook 装配等步骤提取为
//! `build_app_and_acp` / `teardown_app` 公共函数，供 `kit::entry::run_kit_fullscreen` 调用。

use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::acp_client::{AcpNotification, AcpTuiClient};
use crate::acp_server::{AcpServerConfig, run_acp_server};
use crate::app::App;
use crate::app::agent::LlmProvider;
use crate::config::config_path;
use peri_acp::session::SessionManager;
use peri_acp::transport::mpsc::mpsc_transport_pair;
use peri_middlewares::prelude::PermissionMode;

/// TUI 启动选项——CLI 解析后由调用方填好传入。
///
/// 字段语义与 `main.rs::TuiOptions` 一致，但放在 lib 层供 kit 路径复用。
#[derive(Default, Clone)]
pub struct TuiLaunchOptions {
    pub approve: bool,
    pub permission_mode: Option<String>,
    pub skip_permissions: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub continue_session: bool,
    pub resume_session: Option<String>,
    pub session_id: Option<String>,
    pub session_name: Option<String>,
    pub settings: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
}

/// 构建 App + ACP server/client，并把 acp_client 注入 App。
///
/// 调用方负责后续：spawn kit 专用 notifier → `kit::acp_bridge` → atoms；spawn SUBMIT 消费者。
pub async fn build_app_and_acp(
    opts: &TuiLaunchOptions,
    _panic_notify_rx: Option<mpsc::UnboundedReceiver<String>>,
) -> Result<(
    App,
    Option<(AcpTuiClient, mpsc::UnboundedReceiver<AcpNotification>)>,
)> {
    let mut app = App::new().await;

    // (I17-D) panic_notify_rx 已退役——ServiceRegistry.panic_notify_rx 字段删除，
    // 该参数仅保留签名以维持调用方兼容；实际 panic 通知走 tracing log。

    // 根据环境变量/CLI 参数设置初始权限模式
    {
        let initial_mode = if opts.skip_permissions {
            PermissionMode::Bypass
        } else if let Some(ref mode_str) = opts.permission_mode {
            match mode_str.as_str() {
                "bypass" => PermissionMode::Bypass,
                "default" => PermissionMode::Default,
                "accept-edit" => PermissionMode::AcceptEdit,
                "auto-mode" => PermissionMode::AutoMode,
                _ => {
                    if std::env::var("YOLO_MODE")
                        .map(|v| !v.eq_ignore_ascii_case("false") && v != "0")
                        .unwrap_or(true)
                    {
                        PermissionMode::Bypass
                    } else {
                        PermissionMode::Default
                    }
                }
            }
        } else if opts.approve {
            PermissionMode::Default
        } else if std::env::var("YOLO_MODE")
            .map(|v| !v.eq_ignore_ascii_case("false") && v != "0")
            .unwrap_or(true)
        {
            PermissionMode::Bypass
        } else {
            PermissionMode::Default
        };
        app.services.permission_mode.store(initial_mode);
    }

    // --model 覆盖
    if let Some(ref model_str) = opts.model {
        let config = app.services.peri_config.read();
        if let Some(new_provider) = LlmProvider::from_config_for_alias(&config, model_str) {
            tracing::info!(model = %new_provider.model_name(), "CLI --model 覆盖生效");
        }
    }

    // 会话恢复：-c 恢复当前目录最近会话，-r <id> 恢复指定会话。
    //
    // (I17-A) launch 层仅 log 提示，实际的 thread 恢复由 kit/entry 在
    // acp_client + THREAD_LOAD_TX 就绪后异步触发 load_session（避免
    // 在 launch 同步阶段重复 list_threads 查询）。
    if let Some(ref session_id) = opts.resume_session {
        tracing::info!(session_id = %session_id, "-r: kit entry 将恢复指定会话");
    } else if opts.continue_session {
        tracing::info!("-c: kit entry 将恢复当前目录最近会话（若存在）");
    }

    // 检测是否需要 Setup 向导。
    //
    // (I17-B) 实际的 wizard 触发由 kit/entry.rs 在 atoms 初始化后通过
    // WIZARD_ACTIVE atom 设置，这里仅做日志提示。
    {
        let cfg = app.services.peri_config.read();
        if crate::app::setup_wizard::needs_setup(&cfg.config) {
            tracing::info!(
                "needs_setup=true: 首次启动未配置 Provider，kit entry 将触发 SetupWizard"
            );
        }
    }

    // 后台初始化 MCP 连接池（不阻塞 UI）
    app.spawn_mcp_init();

    // 加载已启用插件数据
    {
        let claude_dir = dirs_next::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".claude");
        app.services.plugin_data = Some(peri_middlewares::plugin::load_enabled_plugins_aggregated(
            &claude_dir,
        ));
        // (S13c-4b) plugin_commands + plugin_skills 注入已随 command/ 删除——
        // 插件技能/命令注册由 ACP server 侧 SkillsMiddleware + PluginMiddleware + HookMiddleware 负责。
    }

    // ── ACP Server + Client ─────────────────────────────────────────────
    let acp_client = {
        let provider = {
            let cfg_guard = app.services.peri_config.read();
            LlmProvider::from_config(&cfg_guard)
        }
        .or_else(LlmProvider::from_env);

        if let Some(provider) = provider {
            let plugin_skill_roots = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_skill_roots.clone())
                .unwrap_or_default();
            let plugin_agent_dirs = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_agent_dirs.clone())
                .unwrap_or_default();
            let plugin_lsp_servers = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_lsp_servers.clone())
                .unwrap_or_default();
            let plugin_hooks = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_hooks.clone())
                .unwrap_or_default();
            let plugin_loaded = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.plugins.clone())
                .unwrap_or_default();

            let mut hook_groups: Vec<Vec<peri_middlewares::hooks::RegisteredHook>> = Vec::new();
            if !plugin_hooks.is_empty() {
                hook_groups.push(plugin_hooks);
            }
            let global_hooks = peri_middlewares::hooks::loader::load_global_settings_hooks();
            if !global_hooks.is_empty() {
                hook_groups.push(global_hooks);
            }
            let local_hooks =
                peri_middlewares::hooks::loader::load_settings_local_hooks(&app.services.cwd);
            if !local_hooks.is_empty() {
                hook_groups.push(local_hooks);
            }

            let flat_hooks: Vec<peri_middlewares::hooks::RegisteredHook> =
                hook_groups.iter().flatten().cloned().collect();
            tracing::info!(
                groups = hook_groups.len(),
                total_hooks = flat_hooks.len(),
                "Hook groups assembled for ACP server"
            );

            let tool_search_index = Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new());
            let shared_tools =
                Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::new()));

            let shared_peri_config = app.services.peri_config.clone();
            let session_manager_peri_config_snapshot =
                Arc::new(app.services.peri_config.read().clone());
            let session_manager = SessionManager::new(
                app.services.thread_store.clone(),
                provider.clone(),
                session_manager_peri_config_snapshot,
                app.services.permission_mode.clone(),
                None,
            );

            // (I17-D) app.services.acp_session_manager 字段已退役——
            // 该句柄此前仅由 ServiceRegistry 持有但无任何消费者读取。

            let server_config = AcpServerConfig {
                provider: Arc::new(parking_lot::RwLock::new(provider.clone())),
                peri_config: shared_peri_config,
                permission_mode: app.services.permission_mode.clone(),
                cron_scheduler: Some(app.services.cron.scheduler.clone()),
                mcp_pool: app.services.mcp_pool.clone(),
                channel_state: None, // (S13c-4c) ServiceRegistry.channel_state 已删除
                plugin_skill_roots,
                plugin_agent_dirs,
                plugin_hooks: flat_hooks,
                plugin_loaded,
                hook_groups,
                plugin_lsp_servers,
                tool_search_index: tool_search_index.clone(),
                shared_tools: shared_tools.clone(),
                thread_store: app.services.thread_store.clone(),
                langfuse_session: {
                    if let Some(config) = peri_acp::langfuse::LangfuseConfig::from_env() {
                        tracing::info!("Langfuse tracing enabled (TUI mode)");
                        peri_acp::langfuse::LangfuseSession::new(config, "live".into())
                            .await
                            .map(Arc::new)
                    } else {
                        None
                    }
                },
                config_path: config_path(),
                session_manager,
            };

            let (client_transport, server_transport) = mpsc_transport_pair();
            tokio::spawn(async move {
                run_acp_server(Arc::new(server_transport), server_config).await;
            });

            let (acp_client, notification_rx) = AcpTuiClient::new(client_transport);
            acp_client.spawn_pump();

            app.acp_client = Some(acp_client.clone());

            Some((acp_client, notification_rx))
        } else {
            None
        }
    };

    Ok((app, acp_client))
}

/// App 关闭：fire SessionEnd hooks + MCP pool shutdown。
///
/// 对称 `build_app_and_acp`——所有路径在退出前都应该调用。
///
/// (I16-B) Langfuse flush 等待已退役——`ChatSession.langfuse` 字段删除后，
/// flush handle 永远是 None。Langfuse 退出 flush 由 ACP server 端的
/// `LangfuseSession` Drop 自动处理。
pub async fn teardown_app(app: &mut App) {
    // Fire SessionEnd hooks before shutdown
    {
        let mut hooks = app
            .services
            .plugin_data
            .as_ref()
            .map(|pd| pd.all_hooks.clone())
            .unwrap_or_default();
        hooks.extend(peri_middlewares::hooks::loader::load_global_settings_hooks());
        hooks.extend(peri_middlewares::hooks::loader::load_settings_local_hooks(
            &app.services.cwd,
        ));
        if !hooks.is_empty() {
            let cwd = app.services.cwd.clone();
            let provider_name = app.services.provider_name.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    peri_middlewares::hooks::middleware::fire_standalone_lifecycle_hooks(
                        &hooks,
                        peri_middlewares::hooks::types::HookEvent::SessionEnd,
                        &cwd,
                        "",
                        "",
                        &provider_name,
                        None,
                        Some("prompt_input_exit"),
                    )
                    .await;
                })
            });
        }
    }

    // 关闭 MCP 连接池
    if let Some(pool) = app.services.mcp_pool.take() {
        tracing::info!("正在关闭 MCP 连接池...");
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(pool.shutdown()));
        tracing::info!("MCP 连接池已关闭");
    }
}
