//! ratatui-kit 入口——替代 main_loop::run 的事件循环和渲染。
//!
//! 由 main.rs 在 #[cfg(feature = "use-kit")] 条件下调用。

use ratatui_kit::prelude::*;
use crate::kit::app_shell::AppShell;
use crate::kit::atoms;

/// 使用 ratatui-kit 的全屏模式启动 TUI。
///
/// 前置条件：ACP server 已启动，atoms 已初始化。
pub async fn run_kit_fullscreen() -> anyhow::Result<()> {
    // 初始化全局 atoms（必须在 element! 之前）
    atoms::init_atoms();

    // 启动 ratatui-kit 的全屏 event loop
    element!(AppShell).fullscreen().await?;

    Ok(())
}
