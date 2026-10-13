use ratatui::{
    style::Style,
    text::{Line, Span},
};
use ratatui_kit_markdown::{ListItemData, MarkdownTheme};

use super::span_style::apply_span_styles;

/// 连续列表项之间插入空行的范围（生效策略；调整范围只改这一个常量）。
///
/// - `Some(0)`：仅顶层列表项之间插空行，嵌套项保持紧凑（**当前**）
/// - `Some(n)`：嵌套深度 ≤ n 的连续列表项之间插空行
/// - `Some(u32::MAX)`：所有层级
/// - `None`：列表项之间不插空行（旧行为）
///
/// [Why 顶层] 顶层项是正文扫描的主目标，逐项分隔的辨识收益最大；嵌套项的层级
/// 已由缩进表达，再加空行会把「父项 — 子项」割裂成视觉上并列的两块，且行数随
/// 层级倍增（三层列表接近翻倍），与「提高辨识度」相悖。
pub(crate) const LIST_ITEM_GAP_MAX_DEPTH: Option<u32> = Some(0);

/// 连续列表项之间是否插入空行（`depth` 为当前项的嵌套深度，顶层为 0）。
pub(crate) fn list_item_gap(max_depth: Option<u32>, depth: u32) -> bool {
    max_depth.is_some_and(|max| depth <= max)
}

pub(crate) fn list_item_line(
    item: &ListItemData,
    theme: &MarkdownTheme,
    base_style: Style,
) -> Line<'static> {
    let indent = "  ".repeat(item.depth as usize);
    let prefix = if item.ordered {
        format!("{}{}. ", indent, item.number.unwrap_or(1))
    } else {
        format!("{indent}• ")
    };
    let mut spans = vec![Span::styled(prefix, theme.list_marker_style)];
    spans.extend(apply_span_styles(&item.spans, theme, Some(base_style)));
    Line::from(spans)
}
