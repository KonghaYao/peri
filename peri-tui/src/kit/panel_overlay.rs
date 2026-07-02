//! Panel overlay——根据 `ACTIVE_PANEL` atom 渲染当前激活面板。
//!
//! 这是 kit 路径"面板栈"的渲染入口。面板参与 `SessionColumn` 的垂直布局，位于
//! 消息流和输入区之间：打开面板时覆盖消息流底部区域，但不遮挡输入区。
//! - `None`：渲染零高度容器，不占用布局空间。
//! - `Some(kind)`：渲染对应 `#[component]` 面板。
//!
//! ## 渲染语义
//!
//! 面板不再是根级居中浮层，也不清屏；它占用消息区底部的一段高度，输入区仍固定在
//! 最底部。这样面板语义更接近“消息流上的抽屉/覆盖层”，而不是 modal。
//!
//! ## Esc 关闭
//!
//! 全局 Esc 由 `event_handlers::register_root_handlers` 处理：
//! 若 `ACTIVE_PANEL` 为 Some，调用 `close_active_panel`；否则交由子组件。

use crate::app::panel_types::PanelKind;
use crate::kit::atoms;
use crate::kit::panels::{
    agent::AgentPanel, betas::BetasPanel, config::ConfigPanel, cron::CronPanel, hooks::HooksPanel,
    login::LoginPanel, mcp::McpPanel, memory::MemoryPanel, model::ModelPanel, plugin::PluginPanel,
    status::StatusPanel, tasks::TasksPanel, thread_browser::ThreadBrowserPanel,
    workflow::WorkflowPanel,
};
use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction, Flex},
};

/// 面板覆盖层组件。
///
/// 订阅 `ACTIVE_PANEL` atom，渲染当前激活面板。无面板时返回空 View。
#[component]
pub fn PanelOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let active_panel = hooks.use_atom(&atoms::ACTIVE_PANEL);
    let active = *active_panel.read();
    let (_term_w, term_h) = hooks.use_terminal_size();
    match active {
        Some(PanelKind::Model) => render_panel(element!(ModelPanel()).into(), term_h),
        Some(PanelKind::Login) => render_panel(element!(LoginPanel()).into(), term_h),
        Some(PanelKind::Agent) => render_panel(element!(AgentPanel()).into(), term_h),
        Some(PanelKind::Hooks) => render_panel(element!(HooksPanel()).into(), term_h),
        Some(PanelKind::Config) => render_panel(element!(ConfigPanel()).into(), term_h),
        Some(PanelKind::ThreadBrowser) => {
            render_panel(element!(ThreadBrowserPanel()).into(), term_h)
        }
        Some(PanelKind::Mcp) => render_panel(element!(McpPanel()).into(), term_h),
        Some(PanelKind::Plugin) => render_panel(element!(PluginPanel()).into(), term_h),
        Some(PanelKind::Cron) => render_panel(element!(CronPanel()).into(), term_h),
        Some(PanelKind::Status) => render_panel(element!(StatusPanel()).into(), term_h),
        Some(PanelKind::Memory) => render_panel(element!(MemoryPanel()).into(), term_h),
        Some(PanelKind::Tasks) => render_panel(element!(TasksPanel()).into(), term_h),
        Some(PanelKind::Betas) => render_panel(element!(BetasPanel()).into(), term_h),
        Some(PanelKind::Workflow) => render_panel(element!(WorkflowPanel()).into(), term_h),
        None => render_empty(),
    }
}

/// 包裹面板——在消息流和输入区之间占据固定高度，水平居中显示面板内容。
fn render_panel(panel: AnyElement<'static>, term_h: u16) -> AnyElement<'static> {
    let height = term_h.saturating_sub(8).min(28).max(8);

    element!(
        View(
            flex_direction: Direction::Horizontal,
            justify_content: Flex::Center,
            width: Constraint::Fill(1),
            height: Constraint::Length(height),
        ) {
            { panel }
        }
    )
    .into()
}

/// 空面板——无面板激活时不占用主布局高度。
fn render_empty() -> AnyElement<'static> {
    element!(View(width: Constraint::Fill(1), height: Constraint::Length(0))).into()
}
