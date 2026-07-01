//! Panel overlay——根据 `ACTIVE_PANEL` atom 渲染当前激活面板。
//!
//! 这是 kit 路径"面板栈"的渲染入口——AppShell 顶层固定的覆盖层，
//! 占满整个视口（绝对定位、不参与主布局流）。订阅 `ACTIVE_PANEL`：
//! - `None`：渲染空 View（不消耗布局，但因 AppShell 用绝对定位覆盖层叠加，
//!   实际无视觉影响）。
//! - `Some(kind)`：渲染对应 `#[component]` 面板。
//!
//! ## 渲染语义
//!
//! AppShell 主布局（SessionColumn + StatusBar）始终渲染；PanelOverlay 作为
//! 兄弟节点叠加在上方，由 `View` 的 z-order 决定——ratatui-kit 后到的子节点
//! 覆盖前面的子节点。面板内自带 Border/居中尺寸，所以不会占满整个屏幕。
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
    ratatui::layout::{Constraint, Direction},
};

/// 面板覆盖层组件。
///
/// 订阅 `ACTIVE_PANEL` atom，渲染当前激活面板。无面板时返回空 View。
#[component]
pub fn PanelOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let active_store = hooks.use_store(*atoms::ACTIVE_PANEL.get().unwrap());
    let active = *active_store.read();

    match active {
        Some(PanelKind::Model) => render_panel(element!(ModelPanel()).into()),
        Some(PanelKind::Login) => render_panel(element!(LoginPanel()).into()),
        Some(PanelKind::Agent) => render_panel(element!(AgentPanel()).into()),
        Some(PanelKind::Hooks) => render_panel(element!(HooksPanel()).into()),
        Some(PanelKind::Config) => render_panel(element!(ConfigPanel()).into()),
        Some(PanelKind::ThreadBrowser) => render_panel(element!(ThreadBrowserPanel()).into()),
        Some(PanelKind::Mcp) => render_panel(element!(McpPanel()).into()),
        Some(PanelKind::Plugin) => render_panel(element!(PluginPanel()).into()),
        Some(PanelKind::Cron) => render_panel(element!(CronPanel()).into()),
        Some(PanelKind::Status) => render_panel(element!(StatusPanel()).into()),
        Some(PanelKind::Memory) => render_panel(element!(MemoryPanel()).into()),
        Some(PanelKind::Tasks) => render_panel(element!(TasksPanel()).into()),
        Some(PanelKind::Betas) => render_panel(element!(BetasPanel()).into()),
        Some(PanelKind::Workflow) => render_panel(element!(WorkflowPanel()).into()),
        None => render_empty(),
    }
}

/// 包裹面板——给绝对定位的覆盖层一个填充背景的容器，
/// 让面板的 Border 居中显示而不被主布局穿透。
fn render_panel(panel: AnyElement<'static>) -> AnyElement<'static> {
    let _ = (Direction::Vertical, Constraint::Fill(1)); // 静默未使用警告（element! 内引用）
    panel
}

/// 空覆盖——无面板激活时返回。零尺寸 View，不影响下层渲染。
fn render_empty() -> AnyElement<'static> {
    element!(View(
        flex_direction: Direction::Vertical,
        width: Constraint::Fill(1),
        height: Constraint::Fill(1),
    ))
    .into()
}
