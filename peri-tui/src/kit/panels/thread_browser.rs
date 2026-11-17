//! Thread Browser 面板（spec/global/domains/tui/tui-panels.md §6.6）
//!
//! S6c：thread 列表从 `THREAD_LIST` atom 读取（由 `service_snapshot` 后台任务
//! 周期性经 ACP 查询项目 / 工作区会话）。Enter 切换 thread 操作 S11
//! 解耦后通过 AcpClient 触发。
//!
//! 仿 Login 面板模式：Vec<Line> → Paragraph → ScrollView(Text)。手动键盘
//! 导航 + 选中高亮，不使用 VirtualList。

#[path = "thread_browser/history_preview.rs"]
mod history_preview;
#[path = "thread_browser/selection.rs"]
mod selection;

#[cfg(test)]
#[path = "thread_browser/ui_test.rs"]
mod ui_test;

#[cfg(test)]
use crate::i18n;

#[cfg(test)]
use ratatui_kit::ratatui::style::Style;
#[cfg(test)]
use std::time::Duration;

mod thread_browser_panel;
pub use thread_browser_panel::*;

#[cfg(test)]
use crate::kit::atoms::ACTIVE_SESSION_ID;

#[cfg(test)]
use crate::kit::atoms::BRIDGE_RESET_COUNTER;

#[cfg(test)]
use crate::kit::atoms::NOTIFICATION;

#[cfg(test)]
use crate::kit::atoms::Notification;

#[cfg(test)]
use crate::kit::atoms::THREAD_BROWSER_ARCHIVED;

#[cfg(test)]
use crate::kit::atoms::THREAD_BROWSER_SCOPE;

#[cfg(test)]
use crate::kit::atoms::THREAD_LIST;

#[cfg(test)]
use crate::kit::atoms::THREAD_LIST_ERROR;

#[cfg(test)]
use crate::kit::atoms::ThreadBrowserScope;

#[cfg(test)]
use crate::kit::atoms::ThreadSummary;

mod relative_time;
#[cfg(test)]
use relative_time::format_relative_time;
#[cfg(test)]
use relative_time::relative_time_label;
