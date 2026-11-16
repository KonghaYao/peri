//! `BG_LIVE_DETAIL` 投影写入。

#[path = "bg_task_live_preview.rs"]
mod preview;

#[cfg(test)]
#[path = "bg_task_live_test.rs"]
mod tests;

#[cfg(test)]
use crate::kit::tui_render_unit::TuiAssistantBubble;
use crate::kit::tui_render_unit::TuiRenderUnit;

mod live_projection;
pub use live_projection::*;

#[cfg(test)]
use crate::kit::atoms::BG_LIVE_DETAIL;

use crate::kit::atoms::BgLiveDetail;

use crate::kit::atoms::BgLiveStatus;

#[cfg(test)]
use crate::kit::stream_data::TuiReasoningChunk;

#[cfg(test)]
use crate::kit::stream_data::TuiTextChunk;

#[cfg(test)]
use crate::kit::stream_data::TuiToolEnded;

#[cfg(test)]
use crate::kit::stream_data::TuiToolStarted;
