//! ratatui-kit 全屏 TUI 入口——事件循环和渲染。
//!
//! 这是 kit 总线：替代旧的 main_loop，走完全不同的运行栈——
//! AcpNotification → AcpEventData（kit notifier）→ BridgeState（acp_bridge）
//! → Atom 写入（acp_events）→ ratatui-kit 组件 use_store 重渲染。
//! 用户提交则反向：InputArea → SUBMIT_TX → submit_consumer → acp_client.prompt()。
//! 服务快照：service_snapshot task → SERVICE_SNAPSHOT / THREAD_LIST / CRON_JOBS atoms。

use std::sync::Arc;

use anyhow::Result;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::app::service_registry::ProcessResourceMonitor;
use crate::kit::acp_bridge::spawn_acp_bridge;
use crate::kit::acp_notifier::spawn_kit_notifier;
use crate::kit::acp_types::AcpEventWithEpoch;
use crate::kit::app_shell::AppShell;
use crate::kit::ask_user_action::{AskUserResponseAction, spawn_ask_user_consumer};
use crate::kit::atoms;
use crate::kit::hitl_response::{HitlResponseAction, spawn_hitl_response_consumer};
use crate::kit::input_history;
use crate::kit::rewind_action::spawn_rewind_consumer;
use crate::kit::service_snapshot::{SnapshotSource, spawn_service_snapshot};
use crate::kit::submit_consumer::{spawn_cancel_consumer, spawn_submit_consumer};
use crate::kit::submit_request::SubmitRequest;
use crate::kit::thread_load_consumer::spawn_thread_load_consumer;
use crate::launch::{TuiLaunchOptions, build_app_and_acp, teardown_app};
use ratatui_kit::{
    crossterm::{
        event::{
            DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
            EnableMouseCapture,
        },
        execute,
    },
    prelude::*,
};

/// 使用 ratatui-kit 的全屏模式启动 TUI。
///
/// 接收 CLI 选项 + panic 通知 rx，完成 App+ACP 构建后
/// spawn kit 四链路（notifier / bridge / submit_consumer / service_snapshot），
/// 进入 ratatui-kit 全屏。
///
/// 返回时已调用 `teardown_app`——hooks 清理、MCP 池关闭、Langfuse flush。
pub async fn run_kit_fullscreen(
    opts: TuiLaunchOptions,
    panic_notify_rx: mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    // 1. 初始化全局 atoms（必须在 element! 之前）
    atoms::init_atoms();

    // 1b. 从磁盘加载输入历史到 INPUT_HISTORY atom（文件不存在则静默跳过）
    input_history::load_history();

    // 2. 构建 App + ACP server/client
    let (mut app, acp_client) = build_app_and_acp(&opts, Some(panic_notify_rx)).await?;

    // 2b. H2: 把 peri_config 共享句柄塞到全局 OnceLock，让 ModelPanel 等组件
    //     在 #[component] 闭包里能直接 write active_alias。ACP server 持同一 Arc。
    let _ = atoms::PERI_CONFIG_HANDLE.set(app.services.peri_config.clone());
    // 2b2. i18n: 根据配置语言初始化 thread_local LcRegistry。
    //     组件通过 crate::i18n::tr() / tr_args() 读取翻译文本。
    {
        let cfg = app.services.peri_config.read();
        crate::i18n::init(cfg.config.language.as_deref());
        // 加载配置中的主题（默认 peri-dark）
        let theme_name = cfg.config.theme.as_deref().unwrap_or("peri-dark");
        match peri_theme::loader::load_theme(theme_name) {
            Ok(theme) => peri_theme::atoms::init_theme_atoms(theme),
            Err(e) => tracing::warn!(
                "failed to load theme '{}': {}, using default",
                theme_name,
                e
            ),
        }
    }
    // 2c. H1a: 把 SharedPermissionMode 句柄塞到全局 OnceLock，让 ConfigPanel
    //     能切换 permission_mode。ServiceRegistry + ACP server 持同一 Arc。
    let _ = atoms::PERMISSION_MODE_HANDLE.set(app.services.permission_mode.clone());
    // 2d. H1g: 把 CronScheduler 共享句柄塞到全局 OnceLock，让 CronPanel
    //     能直接 toggle/remove。service_snapshot 下次 tick 自动派生新列表。
    let _ = atoms::CRON_SCHEDULER_HANDLE.set(app.services.cron.scheduler.clone());

    // 2e. I17-B：检测首次启动未配置 Provider，触发 SetupWizard 渲染。
    //     wizard 即使是引导界面也支持 Esc/q 退出（避免首次启动锁死）。
    {
        let cfg = app.services.peri_config.read();
        if crate::app::setup_wizard::needs_setup(&cfg.config) {
            *atoms::WIZARD_ACTIVE.state().write() = true;
            tracing::info!("kit entry: needs_setup=true，触发 SetupWizard");
        }
    }

    let shutdown = CancellationToken::new();

    // 3. service_snapshot 任务——无论是否配 ACP provider 都要启动：
    //    用户即使离线，也需要看到 CPU/MEM/Cron/Thread 列表。
    let snapshot_src = build_snapshot_source(&app);
    let _snapshot_handle = spawn_service_snapshot(snapshot_src, shutdown.clone());

    // 3b. 渲染心跳任务——每 5 秒写一次 RENDER_HEARTBEAT atom，
    //     确保 ratatui-kit render loop 的 `futures::select` 周期性唤醒。
    //     即使终端窗口切换导致 EventStream 阻塞，心跳也能在 5 秒内恢复渲染。
    {
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                        atoms::RENDER_HEARTBEAT.set(
                            atoms::RENDER_HEARTBEAT.get().wrapping_add(1)
                        );
                    }
                }
            }
        });
    }

    // 4. 接通 kit 四链路（仅当 ACP provider 配置成功——acp_client 为 None 时
    //    走最小可用路径：UI 可显示但无 agent 交互）。
    if let Some((client, notification_rx)) = acp_client {
        // 4a. SUBMIT channel：InputArea → submit_consumer
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<SubmitRequest>();
        let _ = atoms::SUBMIT_TX.set(submit_tx);

        // 4b. REWIND_ACTION channel：RewindPopup → rewind_consumer
        let (rewind_tx, rewind_rx) = mpsc::unbounded_channel();
        let _ = atoms::REWIND_ACTION_TX.set(rewind_tx);

        // 4b1. ASK_USER_RESPONSE channel：AskUserPopup → ask_user_consumer
        let (ask_user_tx, ask_user_rx) = mpsc::unbounded_channel::<AskUserResponseAction>();
        let _ = atoms::ASK_USER_RESPONSE_TX.set(ask_user_tx);

        // 4b3. HITL_RESPONSE channel：HitlPopup → hitl_response_consumer
        let (hitl_tx, hitl_rx) = mpsc::unbounded_channel::<HitlResponseAction>();
        let _ = atoms::HITL_RESPONSE_TX.set(hitl_tx);

        // 4b2. THREAD_LOAD channel：ThreadBrowser → thread_load_consumer（H3）
        let (thread_load_tx, thread_load_rx) = mpsc::unbounded_channel::<String>();
        let _ = atoms::THREAD_LOAD_TX.set(thread_load_tx);

        // 4a2. CANCEL channel：event_handlers Ctrl+C → cancel_consumer
        let (cancel_tx, cancel_rx) = mpsc::unbounded_channel::<()>();
        let _ = atoms::CANCEL_TX.set(cancel_tx);

        // 4c. bridge channel：notifier → acp_bridge
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();
        // LOCAL_EVENT_TX：input_area 本地提交 → acp_bridge
        let (local_event_tx, mut local_event_rx) = mpsc::unbounded_channel::<AcpEventWithEpoch>();
        let _ = atoms::LOCAL_EVENT_TX.set(local_event_tx);
        // Mini bridge task：转发 local event 到 bridge channel
        let bridge_tx_clone = bridge_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = local_event_rx.recv().await {
                let _ = bridge_tx_clone.send(ev);
            }
        });

        // 4d. 启动三链路（render_bridge 已删除）
        let _notifier_handle = spawn_kit_notifier(notification_rx, bridge_tx, shutdown.clone());
        let _bridge_handle = spawn_acp_bridge(bridge_rx, shutdown.clone());
        let cwd = app.services.cwd.clone();
        let cwd_for_init = cwd.clone();
        let _submit_handle =
            spawn_submit_consumer(client.clone(), submit_rx, cwd.clone(), shutdown.clone());
        let _rewind_handle = spawn_rewind_consumer(client.clone(), rewind_rx, shutdown.clone());
        let _ask_user_handle =
            spawn_ask_user_consumer(client.clone(), ask_user_rx, shutdown.clone());
        let _hitl_handle = spawn_hitl_response_consumer(client.clone(), hitl_rx, shutdown.clone());
        let _thread_load_handle =
            spawn_thread_load_consumer(client.clone(), thread_load_rx, cwd, shutdown.clone());
        let _cancel_handle = spawn_cancel_consumer(client.clone(), cancel_rx, shutdown.clone());

        // 4e. 初始化会话——在 notifier/bridge 就绪后立即创建 session，
        //     触发服务器发送 AvailableCommandsUpdate（含 skills），确保
        //     slash 补全弹窗在首次输入前就有数据。
        //     **[TRAP]** 必须将返回的 session_id 写入 ACTIVE_SESSION_ID
        //     + 递增 BRIDGE_RESET_COUNTER。不写 ACTIVE_SESSION_ID 则
        //     acp_bridge 的 state.active_session_id 保持空值，后续全部
        //     携带真实 session_id 的 ACP 事件均被 session filter 丢弃
        //     → 渲染管线断流，消息区显示为空。
        {
            let client = client.clone();
            tokio::spawn(async move {
                match client.new_session(&cwd_for_init, None).await {
                    Ok(session_id) => {
                        tracing::info!(%session_id, "kit: initial session created");
                        atoms::ACTIVE_SESSION_ID.set(session_id);
                        atoms::BRIDGE_RESET_COUNTER
                            .set(atoms::BRIDGE_RESET_COUNTER.get().wrapping_add(1));
                    }
                    Err(e) => tracing::warn!(error = %e, "kit: initial session creation failed"),
                }
            });
        }

        // 4f. I17-A：CLI -c/-r 会话恢复——在 acp_client + THREAD_LOAD_TX 就绪后
        //     通过 channel 触发 load_session。spawn 一次性的延迟任务，让
        //     notifier/bridge 先初始化，再 send（避免 race）。
        if opts.resume_session.is_some() || opts.continue_session {
            let thread_load_tx_clone = atoms::THREAD_LOAD_TX.get().cloned();
            let thread_store = app.services.thread_store.clone();
            let cwd_for_restore = app.services.cwd.clone();
            let resume_id = opts.resume_session.clone();
            tokio::spawn(async move {
                let Some(tx) = thread_load_tx_clone else {
                    tracing::warn!("kit 恢复：THREAD_LOAD_TX 未就绪，跳过");
                    return;
                };
                let thread_id = match resume_id.as_deref() {
                    Some(id) => {
                        tracing::info!(session_id = %id, "-r: 触发 load_session");
                        Some(id.to_string())
                    }
                    None => {
                        // -c: 查 thread_store 找当前 cwd 最近 thread
                        match thread_store.list_threads().await {
                            Ok(threads) => threads
                                .into_iter()
                                .find(|t| t.cwd == cwd_for_restore)
                                .map(|t| {
                                    tracing::info!(thread_id = %t.id, "-c: 触发 load_session");
                                    t.id.to_string()
                                }),
                            Err(e) => {
                                tracing::warn!(error = %e, "-c: list_threads 失败");
                                None
                            }
                        }
                    }
                };
                if let Some(id) = thread_id
                    && let Err(e) = tx.send(id)
                {
                    tracing::warn!(error = %e, "kit 恢复：THREAD_LOAD_TX.send 失败");
                }
            });
        }
    } else {
        tracing::warn!("kit 路径：无 ACP provider，TUI 仅以离线模式运行（无 agent 交互）");
    }

    // 5. 进入 ratatui-kit 全屏 event loop（fullscreen 自管 raw mode + alt screen）。
    // ratatui::init() 默认不启用鼠标捕获；未启用时很多终端会把滚轮转成 Up/Down。
    // 必须显式启用，才能让消息区收到 MouseEventKind::Scroll*，避免和键盘方向键语义混淆。
    let _ = execute!(
        std::io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange
    );

    // 5a. 防御性 SIGINT 拦截——macOS 上部分终端在 raw mode 下仍可能对 Ctrl+C 发送
    // SIGINT（或 ratatui-kit 的 raw mode 未完全生效）。此 handler 吞掉 SIGINT 避免
    // 进程被 kernel 强制终止；Ctrl+C 的事件级处理由 Global handler（event_handlers.rs）
    // 独立完成，两者互不干扰。
    let sigint_shutdown = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = sigint_shutdown.cancelled() => break,
                _ = tokio::signal::ctrl_c() => {
                    tracing::warn!(
                        "SIGINT received at process level — swallowed (handled by TUI event handler)"
                    );
                }
            }
        }
    });

    let result = element!(AppShell).fullscreen().await;
    let _ = execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );

    // 6. 退出前触发 shutdown，让后台任务干净退出
    shutdown.cancel();

    // 7. teardown：hooks / MCP / Langfuse
    teardown_app(&mut app).await;

    result?;
    Ok(())
}

/// 从 `App.services` 提取 Arc 共享字段构造 `SnapshotSource`。
///
/// 关键技巧：`ResourceMonitor` 独立新建（采样进程级数据，多实例不影响正确性），
/// 避免 `ServiceRegistry.resource_monitor: Mutex<ProcessResourceMonitor>`（非 Arc）
/// 的所有权冲突。
fn build_snapshot_source(app: &crate::app::App) -> SnapshotSource {
    use crate::kit::atoms::{HookSummary, PluginSummary, ProviderSummary};

    let s = &app.services;

    // H1b/c: 从 plugin_data 一次性派生 hooks/plugins 静态列表
    let (hooks, plugins) = match &s.plugin_data {
        Some(pd) => {
            let hooks: Vec<HookSummary> = pd
                .all_hooks
                .iter()
                .map(|h| HookSummary {
                    event: format!("{:?}", h.event).to_lowercase(),
                    plugin_name: h.plugin_name.clone(),
                    command: format!("{:?}", h.hook).chars().take(120).collect(),
                    matcher: h.matcher.clone(),
                })
                .collect();
            let plugins: Vec<PluginSummary> = pd
                .plugins
                .iter()
                .map(|p| PluginSummary {
                    name: p.name.clone(),
                    version: p.version.clone(),
                    enabled: true, // 已加载的插件即视为 enabled
                    root: p.install_path.display().to_string(),
                    description: p.manifest.description.clone(),
                })
                .collect();
            (hooks, plugins)
        }
        None => (Vec::new(), Vec::new()),
    };

    // H1f: 从 peri_config 派生 providers
    let providers: Vec<ProviderSummary> = {
        let cfg = s.peri_config.read();
        cfg.config
            .providers
            .iter()
            .map(|p| {
                let env_key = format!("{}_API_KEY", p.provider_type.to_uppercase());
                let has_api_key = !p.api_key.is_empty() || std::env::var(env_key).is_ok();
                let base_url = if p.base_url.is_empty() {
                    None
                } else {
                    Some(p.base_url.clone())
                };
                ProviderSummary {
                    id: p.id.clone(),
                    provider_type: p.provider_type.clone(),
                    is_active: p.id == cfg.config.active_provider_id,
                    has_api_key,
                    base_url,
                }
            })
            .collect()
    };

    SnapshotSource {
        cwd: s.cwd.clone(),
        thread_store: s.thread_store.clone(),
        peri_config: s.peri_config.clone(),
        permission_mode: s.permission_mode.clone(),
        cron_scheduler: s.cron.scheduler.clone(),
        mcp_pool: s.mcp_pool.clone(),
        mcp_init_rx: s.mcp_init_rx.clone(),
        resource_monitor: Arc::new(Mutex::new(ProcessResourceMonitor::new())),
        hooks,
        plugins,
        providers,
    }
}
