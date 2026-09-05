//! ACP 流式数据类型——`CurrentTurn` + `ToolCardAccumulator` + `AcpEventData`。
//!
//! S11 起类型定义集中在本模块，不再通过 re-export 分散到其他模块。
//!
//! ## 设计
//!
//! - **纯数据 + 方法**：所有字段为 String/Vec/bool/u32/serde_json::Value，
//!   天然 Send+Sync+'static
//! - **依赖**：仅 `crate::kit::tui_render_unit::TuiRenderUnit` 和
//!   `peri_acp_types::event_data::*`（workspace crate，非 legacy）
//! - **零运行时依赖**：无 terminal / network / IO，可独立测试

mod current_turn;
mod event_data;
mod tool_card;

pub use current_turn::CurrentTurn;
pub use event_data::{
    AcpEventData, AcpEventWithEpoch, BgTaskEntry, CacheUsageSample, FeedbackChannel, FeedbackLevel,
    PendingInteraction, TuiCommandFeedback,
};
pub(crate) use tool_card::build_tool_card;
pub(crate) use tool_card::parse_tool_diff;
pub use tool_card::{SubAgentAccumulator, ToolCardAccumulator};

#[cfg(test)]
use crate::kit::tui_render_unit::{EntryStatus, TuiRenderUnit};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "acp_types_test.rs"]
mod tests;

#[cfg(test)]
#[path = "acp_types/event_data_bg_test.rs"]
mod event_data_bg_test;
