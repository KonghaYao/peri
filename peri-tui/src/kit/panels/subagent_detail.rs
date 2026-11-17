//! ratatui-kit SubAgentDetailPanel component.
//!
//! §6.7 subagent 详情 pane：Enter 打开 nested transcript 或详情 pane，不把
//! 完整嵌套消息铺入主时间轴。本面板从 VIEW_MODELS 扫描 `TuiSubAgentGroup`
//! （按 `SELECTED_SUBAGENT_ID` 匹配，由消息区焦点分派在 Enter 时写入），
//! 用真实面板宽度构建 `GridSpec`，共享 EntryRenderCache 嵌套渲染子消息。
//! 缓存保留完整内容高度，只为可见范围访问行，不渲染 md 复制按钮。
//!
//! canonical 内容只读；面板内可选择、展开和滚动（滚轮复用面板仲裁）；Esc 单层关闭
//! （栈顶弹栈，不触及其他面板/焦点仲裁）。

#[path = "child_history_detail.rs"]
mod child_history;
#[path = "subagent_detail_cache.rs"]
mod render_cache;

#[cfg(test)]
#[path = "subagent_detail_test.rs"]
mod tests;

#[cfg(test)]
use crate::kit::message_area::grid::GridSpec;
#[cfg(test)]
use crate::kit::tui_render_unit::TuiRenderUnit;
#[cfg(test)]
use ratatui_kit::ratatui::layout::Rect;

mod sub_agent_detail_panel;
use self::sub_agent_detail_panel as detail_panel;
pub use detail_panel::*;

#[cfg(test)]
use crate::kit::atoms::BgDisplayEntry;
