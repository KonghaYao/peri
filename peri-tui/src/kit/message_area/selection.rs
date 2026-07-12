//! 文本选区 + 折行映射：wrap_map 构建、视觉→逻辑行转换、选区提取、剪贴板复制。

use std::cmp::Ordering;
use std::time::{Duration, Instant};

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL};
use crate::kit::text_selection;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::widgets::{Paragraph, Wrap};

// ── wrap_map 类型 ──────────────────────────────────────────────────────────

/// 折行映射条目：逻辑行索引 + 该逻辑行占据的视觉行范围 [visual_start, visual_end)。
#[derive(Debug, Clone)]
pub(super) struct WrappedLineInfo {
    pub(super) logical_idx: usize,
    pub(super) visual_start: usize,
    pub(super) visual_end: usize,
}

/// 为 all_lines 构建视觉行→逻辑行映射。
/// 返回 (total_visual_rows, wrap_map)。wrap_map 按 visual_start 升序排列，可二分查找。
pub(super) fn build_wrap_map(lines: &[Line<'static>], width: u16) -> (usize, Vec<WrappedLineInfo>) {
    let mut wrap_map = Vec::with_capacity(lines.len());
    let mut visual_row = 0usize;
    for (idx, line) in lines.iter().enumerate() {
        let rows = Paragraph::new(ratatui_kit::ratatui::text::Text::from(line.clone()))
            .wrap(Wrap { trim: false })
            .line_count(width) as usize;
        let rows = rows.max(1);
        wrap_map.push(WrappedLineInfo {
            logical_idx: idx,
            visual_start: visual_row,
            visual_end: visual_row + rows,
        });
        visual_row += rows;
    }
    (visual_row, wrap_map)
}

/// 二分查找：视觉行 → 逻辑行索引。
pub(super) fn visual_to_logical(visual_row: u16, wrap_map: &[WrappedLineInfo]) -> Option<usize> {
    let vr = visual_row as usize;
    match wrap_map.binary_search_by(|entry| {
        if vr < entry.visual_start {
            Ordering::Greater
        } else if vr >= entry.visual_end {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    }) {
        Ok(idx) => Some(wrap_map[idx].logical_idx),
        Err(_) => None,
    }
}

/// 计算视口 [scroll_y, scroll_y + vp_height) 对应的逻辑行范围 + 首行视觉偏移。
///
/// 返回 (start_logical, end_logical, first_line_visual_offset)。
/// first_line_visual_offset 是首行在视口内向下推的视觉行数（Paragraph::scroll 第一参数）。
/// 当 wrap_map 为空或视口在范围外时返回 None。
pub(super) fn viewport_logical_range(
    wrap_map: &[WrappedLineInfo],
    scroll_y: usize,
    vp_height: usize,
) -> Option<(usize, usize, u16)> {
    if wrap_map.is_empty() || vp_height == 0 {
        return None;
    }
    // 视口起始：第一个 visual_end > scroll_y 的 entry
    let start_idx = wrap_map.iter().position(|e| e.visual_end > scroll_y)?;
    let start_logical = wrap_map[start_idx].logical_idx;
    let first_line_offset = scroll_y.saturating_sub(wrap_map[start_idx].visual_start);
    // 视口结束：第一个 visual_start >= scroll_y + vp_height 的 entry 之前
    let vp_visual_end = scroll_y.checked_add(vp_height)?;
    let end_logical = wrap_map
        .iter()
        .take_while(|e| e.visual_start < vp_visual_end)
        .last()
        .map(|e| e.logical_idx)
        .unwrap_or(start_logical);
    Some((start_logical, end_logical, first_line_offset as u16))
}

// ── 剪贴板复制 ────────────────────────────────────────────────────────────

/// 在独立线程中写入系统剪贴板，避免阻塞 tokio worker。
pub(super) fn copy_to_clipboard(text: String) {
    std::thread::spawn(move || {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(&text);
        }
    });
}

pub(super) fn mark_copy_message(char_count: usize) {
    COPY_CHAR_COUNT.set(char_count);
    COPY_MESSAGE_UNTIL.set(Some(Instant::now() + Duration::from_secs(2)));
}

/// 从逻辑行中按视觉坐标精确提取选中文本（字符级精度）。
///
/// 折行偏移公式：column_in_logical = vis_col + (vis_row - visual_start) * width。
/// 用 `visual_col_to_byte_offset` 将列映射到字节偏移再切片。
///
/// [TRAP] 选区可能超出 core 范围（footer 区域无 wrap_map）——clamp 到 wrap_map
/// 末尾，确保 footer 行的 visual_to_logical 不返回 None 导致整个提取失败。
pub(super) fn extract_visual_range(
    lines: &[Line<'static>],
    wrap_map: &[WrappedLineInfo],
    vis_start: (u16, u16),
    vis_end: (u16, u16),
    width: u16,
) -> Option<String> {
    let ((sr, sc), (er, ec)) = if vis_start <= vis_end {
        (vis_start, vis_end)
    } else {
        (vis_end, vis_start)
    };
    // Clamp sr/er 到 wrap_map 视觉范围内（footer 区域无 wrap_map，避免 None）
    let max_visual = wrap_map
        .last()
        .map(|e| (e.visual_end.saturating_sub(1)) as u16)
        .unwrap_or(0);
    let sr = sr.min(max_visual);
    let er = er.min(max_visual);
    let first_logical = visual_to_logical(sr, wrap_map)?;
    let last_logical = visual_to_logical(er, wrap_map)?;
    let first = first_logical.min(last_logical);
    let last = first_logical.max(last_logical);

    let mut parts: Vec<String> = Vec::new();
    for li in first..=last {
        let line = lines.get(li)?;
        let plain = text_selection::line_to_plain_text(line);
        let entry = wrap_map.get(li)?;

        if first == last {
            // 同一逻辑行
            let c_start =
                sc.saturating_add((sr as usize).saturating_sub(entry.visual_start) as u16 * width);
            let c_end =
                ec.saturating_add((er as usize).saturating_sub(entry.visual_start) as u16 * width);
            let b0 = text_selection::visual_col_to_byte_offset(&plain, c_start);
            let b1 = text_selection::visual_col_to_byte_offset(&plain, c_end);
            if b0 >= b1 {
                continue;
            }
            parts.push(plain[b0..b1].to_string());
        } else if li == first {
            let c_start =
                sc.saturating_add((sr as usize).saturating_sub(entry.visual_start) as u16 * width);
            let b0 = text_selection::visual_col_to_byte_offset(&plain, c_start);
            parts.push(plain[b0..].to_string());
        } else if li == last {
            let c_end =
                ec.saturating_add((er as usize).saturating_sub(entry.visual_start) as u16 * width);
            let b1 = text_selection::visual_col_to_byte_offset(&plain, c_end);
            parts.push(plain[..b1].to_string());
        } else {
            parts.push(plain);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
