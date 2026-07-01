//! ratatui-kit 入口——替代 main_loop::run 的事件循环和渲染。
//!
//! 由 main.rs 在 `#[cfg(feature = "use-kit")]` 条件下调用。
//!
//! 这是 kit 路径的总线：与 legacy `run_app` 平级但走完全不同的运行栈——
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
use crate::kit::app_shell::AppShell;
use crate::kit::atoms;
use crate::kit::service_snapshot::{SnapshotSource, spawn_service_snapshot};
use crate::kit::submit_consumer::spawn_submit_consumer;
use crate::launch::{TuiLaunchOptions, build_app_and_acp, teardown_app};
use ratatui_kit::prelude::*;

/// 使用 ratatui-kit 的全屏模式启动 TUI。
///
/// 与 legacy `run_app` 对称：接收 CLI 选项 + panic 通知 rx，完成 App+ACP 构建后
/// spawn kit 四链路（notifier / bridge / submit_consumer / service_snapshot），
/// 进入 ratatui-kit 全屏。
///
/// 返回时已调用 `teardown_app`——hooks/hooks 清理、MCP 池关闭、Langfuse flush。
pub async fn run_kit_fullscreen(
    opts: TuiLaunchOptions,
    panic_notify_rx: mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    // 1. 初始化全局 atoms（必须在 element! 之前）
    atoms::init_atoms();

    // 2. 构建 App + ACP server/client（与 legacy 共享同一段构造逻辑）
    let (mut app, acp_client) = build_app_and_acp(&opts, Some(panic_notify_rx)).await?;

    let shutdown = CancellationToken::new();

    // 3. service_snapshot 任务——无论是否配 ACP provider 都要启动：
    //    用户即使离线，也需要看到 CPU/MEM/Cron/Thread 列表。
    let snapshot_src = build_snapshot_source(&app);
    let _snapshot_handle = spawn_service_snapshot(snapshot_src, shutdown.clone());

    // 4. 接通 kit 三链路（仅当 ACP provider 配置成功——acp_client 为 None 时
    //    走最小可用路径：UI 可显示但无 agent 交互）。
    if let Some((client, notification_rx)) = acp_client {
        // 4a. SUBMIT channel：InputArea → submit_consumer
        let (submit_tx, submit_rx) = mpsc::unbounded_channel::<String>();
        let _ = atoms::SUBMIT_TX.set(submit_tx);

        // 4b. bridge channel：notifier → acp_bridge
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel();

        // 4c. 启动三链路
        let _notifier_handle = spawn_kit_notifier(notification_rx, bridge_tx, shutdown.clone());
        let _bridge_handle = spawn_acp_bridge(bridge_rx, shutdown.clone());
        let cwd = app.services.cwd.clone();
        let _submit_handle =
            spawn_submit_consumer(client.clone(), submit_rx, cwd, shutdown.clone());
    } else {
        tracing::warn!("kit 路径：无 ACP provider，TUI 仅以离线模式运行（无 agent 交互）");
    }

    // 5. 进入 ratatui-kit 全屏 event loop（fullscreen 自管 raw mode + alt screen）
    let result = element!(AppShell).fullscreen().await;

    // 6. 退出前触发 shutdown，让后台任务干净退出
    shutdown.cancel();

    // 7. teardown：hooks/MCP/Langfuse（与 legacy run_app 对称）
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
    let s = &app.services;
    SnapshotSource {
        cwd: s.cwd.clone(),
        thread_store: s.thread_store.clone(),
        peri_config: s.peri_config.clone(),
        permission_mode: s.permission_mode.clone(),
        cron_scheduler: s.cron.scheduler.clone(),
        mcp_pool: s.mcp_pool.clone(),
        mcp_init_rx: s.mcp_init_rx.clone(),
        resource_monitor: Arc::new(Mutex::new(ProcessResourceMonitor::new())),
    }
}
