//! V2 TuiRenderUnit → ratatui Line 转换器。
//!
//! 纯函数 `render_v2_vm(vm, width) -> Vec<Line<'static>>`，
//! 处理全部 7 种 `crate::kit::tui_render_unit::TuiRenderUnit` 变体。
//! 零副作用，不持有缓存——markdown 每帧重新解析。

mod ask_user;
mod assistant;
mod misc;
mod probe;
mod subagent;
mod system_note;
#[cfg(test)]
mod tests;
mod tool_card;
mod user;

pub(crate) use probe::RENDER_CALL_COUNT;
pub use probe::{SubAgentRenderInfo, SubAgentStatusProbe, with_status_probe};

use std::sync::atomic::Ordering;

use ratatui::text::Line;

use crate::kit::markdown::MarkdownSegment;
use crate::kit::tui_render_unit::TuiRenderUnit;

// ── 公开入口 ──────────────────────────────────────────────────────────────

/// 将单个 V2 TuiRenderUnit 转换为段落序列（文本行 / 表格数据分离）。
pub fn render_v2_vm(vm: &TuiRenderUnit, width: usize) -> Vec<MarkdownSegment> {
    RENDER_CALL_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
    match vm {
        TuiRenderUnit::TuiUserBubble(data) => {
            user::render_user_bubble(&data.text, width, data.reminder.as_ref())
        }
        TuiRenderUnit::TuiAssistantBubble(data) => assistant::render_assistant_bubble(data, width),
        TuiRenderUnit::TuiToolCard(data) => {
            vec![MarkdownSegment::Text(tool_card::render_tool_card(data))]
        }
        TuiRenderUnit::TuiSystemNote(data) => {
            vec![MarkdownSegment::Text(system_note::render_system_note(data))]
        }
        TuiRenderUnit::TuiSubAgentGroup(data) => {
            let lines = subagent::render_subagent_group(data, width);
            vec![MarkdownSegment::Text(lines)]
        }
        TuiRenderUnit::TuiCollapsedGroup(data) => {
            vec![MarkdownSegment::Text(misc::render_collapsed_group(data))]
        }
        TuiRenderUnit::TuiDivider(data) => {
            vec![MarkdownSegment::Text(misc::render_divider(data))]
        }
        TuiRenderUnit::TuiAskUserBlock(data) => {
            vec![MarkdownSegment::Text(ask_user::render_ask_user_block(data))]
        }
    }
}

/// 便利函数：将段落展平为 Line 列表（表格跳过）。
pub fn segments_to_lines(segments: &[MarkdownSegment]) -> Vec<Line<'static>> {
    segments
        .iter()
        .flat_map(|s| match s {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => vec![],
        })
        .collect()
}
