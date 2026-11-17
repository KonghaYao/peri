//! ACP 事件类型定义和 Atom 写入辅助函数。
//!
//! 将 AcpEventData 映射为全局 Atom 写入，供 kit 组件通过 use_store 订阅。
//! Phase 2 桥接层——ACP 事件 → Atom 写入。

mod agent;
mod compact;
mod display_retention;
mod fold;
mod history_page;
pub(crate) mod history_v2;
pub(crate) mod history_window;
mod live_seal;
pub(crate) mod live_tool_links;
mod memory_probe;
pub(crate) mod render;
mod streaming;
mod subagent;
mod subagent_preview;
mod system;
mod todo_cache;
mod tool;
mod turn;

#[cfg(test)]
#[path = "../acp_events_test.rs"]
pub(crate) mod acp_events_test;

#[cfg(test)]
use crate::kit::acp_types::AcpEventData;
#[cfg(test)]
use crate::kit::acp_types::CacheUsageSample;
#[cfg(test)]
use crate::kit::acp_types::CurrentTurn;

#[cfg(test)]
use crate::kit::tui_render_unit::TuiNoteLevel;

mod bridge_state;
pub use bridge_state::*;

#[cfg(test)]
pub(crate) use self::render::drain_input_buffer;
pub use self::render::handle_plan_update;
pub(crate) use self::render::push_view_models;
pub use self::render::push_view_models_for_reset;
pub(crate) use self::system::request_bg_task_snapshot;
pub use crate::kit::submit_request::SubmitRequest;
pub use crate::kit::tui_render_unit::{TuiAssistantBubble, TuiRenderUnit, TuiUserBubble};
