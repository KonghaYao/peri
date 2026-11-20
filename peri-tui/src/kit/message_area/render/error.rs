use super::CopyButtonInfo;
use super::group::SUBAGENT_TOOL_LINES;
use crate::kit::message_area::grid::GridSpec;
use crate::kit::tui_render_unit::{TuiNoteLevel, TuiRenderUnit};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn preview_lines(text: &str, width: usize, limit: usize) -> Vec<String> {
    if width == 0 || limit == 0 {
        return Vec::new();
    }
    let mut lines = vec![String::new()];
    let mut columns = 0;
    let mut graphemes = text.graphemes(true).peekable();
    while let Some(grapheme) = graphemes.next() {
        let newline = matches!(grapheme, "\n" | "\r\n" | "\r");
        if newline && graphemes.peek().is_none() {
            break;
        }
        if newline || (columns > 0 && columns + grapheme.width() > width) {
            if lines.len() == limit {
                let last = lines
                    .last_mut()
                    .expect("error preview starts with one line and never removes it");
                *last = format!(
                    "{}…",
                    // 省略号由这里追加；用前缀 helper 让正文占满 width - 1 列，
                    // 不能再让截断函数从预算里扣掉省略号。
                    peri_tui_components::text::prefix_by_width(last, width.saturating_sub(1))
                );
                return lines;
            }
            lines.push(String::new());
            columns = 0;
        }
        if !newline {
            let visible = crate::truncate::truncate_to_width(grapheme, width);
            columns += visible.width();
            lines
                .last_mut()
                .expect("error preview starts with one line and never removes it")
                .push_str(&visible);
        }
    }
    lines
}

/// 首行错误符号的点击列区间（§6.4 工具 / §6.6 系统错误共用）。
///
/// 过程区首行的符号落在标记列 [`GridSpec::process_marker_col`]（§3），命中列随
/// `indent` 右移；首行与区内行前缀等宽（§5），转场台阶不改变符号列。
pub(super) fn icon_button(grid: &GridSpec, logical_idx: usize) -> CopyButtonInfo {
    // `indent == 0`（Compact/Narrow）回退形态里符号仍占 accent 列（§9）。
    let x_start = if grid.indent == 0 {
        grid.outer
    } else {
        grid.process_marker_col()
    };
    CopyButtonInfo {
        logical_idx,
        x_start,
        x_end: x_start.saturating_add(grid.accent),
    }
}

/// 过程区续行内容起始列（组内工具行 / 失败原因行的符号列）——`process_prefix_width()`
/// 已按断点覆盖回退形态（`indent == 0` 取基准前缀宽，§9）。
fn process_content_col(grid: &GridSpec) -> u16 {
    grid.process_prefix_width() as u16
}

pub(crate) fn subagent_error_buttons(vm: &TuiRenderUnit, grid: &GridSpec) -> Vec<CopyButtonInfo> {
    let TuiRenderUnit::TuiSubAgentGroup(group) = vm else {
        return Vec::new();
    };
    let recent = group
        .view_models
        .iter()
        .rev()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(card) => Some(card),
            _ => None,
        })
        .take(SUBAGENT_TOOL_LINES)
        .collect::<Vec<_>>();
    let symbol_column = process_content_col(grid);
    let mut buttons = recent
        .iter()
        .enumerate()
        .filter(|(_, card)| card.is_error)
        .map(|(logical_idx, _)| CopyButtonInfo {
            logical_idx,
            x_start: symbol_column,
            x_end: symbol_column.saturating_add(1),
        })
        .collect::<Vec<_>>();
    if super::group::subagent_error_reason(group).is_some() {
        buttons.push(CopyButtonInfo {
            logical_idx: recent.len(),
            x_start: symbol_column,
            x_end: symbol_column.saturating_add(1),
        });
    }
    buttons
}

pub(crate) fn copy_text_at(vm: &TuiRenderUnit, logical_idx: usize) -> Option<String> {
    match vm {
        TuiRenderUnit::TuiAssistantBubble(data) => Some(data.text.clone()),
        TuiRenderUnit::TuiSystemNote(data) if data.level == TuiNoteLevel::Error => {
            Some(data.text.clone())
        }
        TuiRenderUnit::TuiToolCard(data) if data.is_error => Some(data.output_summary.clone()),
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            let recent = group
                .view_models
                .iter()
                .rev()
                .filter_map(|vm| match vm {
                    TuiRenderUnit::TuiToolCard(card) => Some(card),
                    _ => None,
                })
                .take(SUBAGENT_TOOL_LINES)
                .collect::<Vec<_>>();
            if let Some(card) = recent.get(logical_idx) {
                card.is_error.then(|| card.output_summary.clone())
            } else if logical_idx == recent.len() {
                super::group::subagent_error_reason(group).map(str::to_owned)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
