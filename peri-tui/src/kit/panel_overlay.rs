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
use ratatui_kit::{prelude::*, ratatui::layout::Constraint};

/// 面板覆盖层组件。
///
/// 订阅 `ACTIVE_PANEL` atom，渲染当前激活面板。无面板时返回空 View。
#[component]
pub fn PanelOverlay(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let active_panel = hooks.use_atom(&atoms::ACTIVE_PANEL);
    let active = *active_panel.read();
    let (term_w, term_h) = hooks.use_terminal_size();
    match active {
        Some(PanelKind::Model) => render_panel(element!(ModelPanel()).into(), term_w, term_h),
        Some(PanelKind::Login) => render_panel(element!(LoginPanel()).into(), term_w, term_h),
        Some(PanelKind::Agent) => render_panel(element!(AgentPanel()).into(), term_w, term_h),
        Some(PanelKind::Hooks) => render_panel(element!(HooksPanel()).into(), term_w, term_h),
        Some(PanelKind::Config) => render_panel(element!(ConfigPanel()).into(), term_w, term_h),
        Some(PanelKind::ThreadBrowser) => {
            render_panel(element!(ThreadBrowserPanel()).into(), term_w, term_h)
        }
        Some(PanelKind::Mcp) => render_panel(element!(McpPanel()).into(), term_w, term_h),
        Some(PanelKind::Plugin) => render_panel(element!(PluginPanel()).into(), term_w, term_h),
        Some(PanelKind::Cron) => render_panel(element!(CronPanel()).into(), term_w, term_h),
        Some(PanelKind::Status) => render_panel(element!(StatusPanel()).into(), term_w, term_h),
        Some(PanelKind::Memory) => render_panel(element!(MemoryPanel()).into(), term_w, term_h),
        Some(PanelKind::Tasks) => render_panel(element!(TasksPanel()).into(), term_w, term_h),
        Some(PanelKind::Betas) => render_panel(element!(BetasPanel()).into(), term_w, term_h),
        Some(PanelKind::Workflow) => render_panel(element!(WorkflowPanel()).into(), term_w, term_h),
        None => render_empty(),
    }
}

/// 包裹面板——只定位和清除面板矩形，避免 Modal 整屏背景绘制导致白屏。
fn render_panel(panel: AnyElement<'static>, term_w: u16, term_h: u16) -> AnyElement<'static> {
    let width = term_w.saturating_sub(4).min(120).max(1);
    let height = term_h.saturating_sub(4).min(36).max(1);
    let x = term_w.saturating_sub(width) / 2;
    let y = term_h.saturating_sub(height) / 2;

    element!(
        Positioned(x: x, y: y, width: width, height: height, clear: true) {
            Center(width: Constraint::Fill(1), height: Constraint::Fill(1)) {
                { panel }
            }
        }
    )
    .into()
}

/// 空覆盖——无面板激活时返回零尺寸 Positioned，避免默认 View/Fragment 布局参与父级 flex。
fn render_empty() -> AnyElement<'static> {
    element!(Positioned(x: 0u16, y: 0u16, width: 0u16, height: 0u16, clear: false)).into()
}
