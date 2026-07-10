//! ratatui-kit 集成模块。
//!
//! 该模块包含所有 ratatui-kit #[component] 组件。
//! Phase 1-3：面板、弹窗、布局组件（编译桩）。
//! Phase 4：事件系统统一——atoms/acp_bridge/event_handlers/mention/slash。
//! Phase 5：Widget 桥接组件——message_area/input_area。

pub mod acp_bridge;
pub mod acp_events;
pub mod acp_notifier;
pub mod acp_types;
pub mod app_shell;
pub mod ask_user_action;
pub mod atoms;
pub mod bg_task_area;
pub mod bubbles;
pub mod entry;
pub mod event_handlers;
pub mod focus_router;
pub mod hitl_response;
pub mod inline_nav;
pub mod input_area;
pub mod input_history;
pub mod layout;
pub mod list_nav;
pub mod markdown;
pub mod mention_popup;
pub mod message_area;
pub mod panel_overlay;
pub mod panel_registry;
pub mod panels;
pub mod popup_overlay;
pub mod popups;
pub mod render_bridge;
pub mod rewind_action;
pub mod service_snapshot;
pub mod setup_wizard;
pub mod slash_completion;
pub mod status_bar;
pub mod stream_data;
pub mod submit_consumer;
pub mod submit_request;
pub mod text_selection;
pub mod theme;
pub mod thread_load_consumer;
pub mod tool_display;
pub mod tui_render_unit;
pub mod view_render;
pub mod welcome;

// Phase 3: 导出布局组件
pub use app_shell::AppShell;
pub use layout::SessionColumn;
pub use status_bar::StatusBar;
