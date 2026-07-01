//! ratatui-kit 集成模块。
//!
//! 该模块包含所有 ratatui-kit #[component] 组件。
//! Phase 1-3：面板、弹窗、布局组件（编译桩）。
//! Phase 4：事件系统统一——atoms/acp_bridge/event_handlers/mention/slash。
//! Phase 5：Widget 桥接组件——message_area/input_area。

pub mod acp_bridge;
pub mod acp_events;
pub mod app_shell;
pub mod atoms;
pub mod entry;
pub mod event_handlers;
pub mod input_area;
pub mod layout;
pub mod mention_popup;
pub mod message_area;
pub mod panels;
pub mod popups;
pub mod setup_wizard;
pub mod slash_completion;
pub mod status_bar;

// Phase 3: 导出布局组件
pub use app_shell::AppShell;
pub use layout::SessionColumn;
pub use status_bar::StatusBar;
