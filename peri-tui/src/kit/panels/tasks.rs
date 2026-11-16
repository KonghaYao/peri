//! ratatui-kit TasksPanel component.
//!
//! H1g（Iteration 14）：聚合多源任务视图——
//! - 后台任务（从 BG_TASKS atom）
//! - Cron 任务（从 CRON_JOBS atom）
//! - SubAgent 运行时（从 VIEW_MODELS 扫描 TuiSubAgentGroup）
//!
//! 三段拼在同一个连续选中下标空间里（后台段在最前），提供跨调度源的"任务总览"。
//! Cron 任务的 enable/disable/delete 在 Cron 面板、SubAgent 详情在 Agent 面板，
//! 故本面板的行内动作只有「取消后台任务」（选中该行按 Enter，或左键点击该行）。
//!
//! 行结构、可见窗口与鼠标命中由 `viewport` 模块的行表统一推导（绘制、命中、取消目标
//! 解析读同一份结果）。

#[cfg(test)]
mod tests;
mod viewport;

#[cfg(test)]
use crate::app::panel_types::PanelKind;
#[cfg(test)]
use crate::application::operations::OperationTicket;

#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use self::viewport::CancelTarget;
#[cfg(test)]
use self::viewport::SegmentCounts;
#[cfg(test)]
use self::viewport::TaskSegment;
#[cfg(test)]
use self::viewport::TasksRow;
#[cfg(test)]
use self::viewport::TasksViewport;
#[cfg(test)]
use crate::kit::tui_render_unit::TuiRenderUnit;
#[cfg(test)]
use crate::kit::tui_render_unit::TuiSubAgentGroup;
#[cfg(test)]
use ratatui_kit::crossterm::event::Event;
#[cfg(test)]
use ratatui_kit::crossterm::event::KeyCode;
#[cfg(test)]
use ratatui_kit::crossterm::event::MouseButton;
#[cfg(test)]
use ratatui_kit::crossterm::event::MouseEventKind;
#[cfg(test)]
use ratatui_kit::ratatui::layout::Rect;

mod tasks_panel;
pub use tasks_panel::*;

#[cfg(test)]
use crate::kit::atoms::BG_TASKS;

#[cfg(test)]
use crate::kit::atoms::BRIDGE_RESET_COUNTER;

#[cfg(test)]
use crate::kit::atoms::BgTaskEntry;

#[cfg(test)]
use crate::kit::atoms::CRON_JOBS;

#[cfg(test)]
use crate::kit::atoms::CronJobSummary;

#[cfg(test)]
use crate::kit::atoms::NOTIFICATION;

#[cfg(test)]
use crate::kit::atoms::Notification;

#[cfg(test)]
use crate::kit::atoms::VIEW_MODELS;
