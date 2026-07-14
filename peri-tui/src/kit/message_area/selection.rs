//! 文本选区 + 折行映射：wrap_map 构建、视觉→逻辑行转换、选区提取、剪贴板复制。

use std::cmp::Ordering;
use std::time::{Duration, Instant};

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL};
use crate::kit::text_selection;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::widgets::{Paragraph, Wrap};

// ── wrap_map 类型 ──────────────────────────────────────────────────────────

/// 折行映射条目：逻辑行索引 + 该逻辑行占据的视觉行范围 [visual_start, visual_end)。
#[derive(Debug, Clone, PartialEq)]
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
            .line_count(width);
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

/// 拼接多个 VM 的 wrap_map：每个分片内部 visual_row 从 0 起、logical_idx 从 0 起，
/// 拼接时累加 visual_offset 和 lines_start（合并后 lines 中该分片的起始 logical_idx）。
///
/// 输入：每个分片的 (wrap_map, lines_start_offset)。
/// 输出：扁平化 wrap_map，所有 entry 的 visual_start/end 是全量坐标，
/// logical_idx 是合并 lines 中的索引，可直接传给 visual_to_logical / viewport_logical_range。
pub(super) fn concat_wrap_maps(slots: &[(&[WrappedLineInfo], usize)]) -> Vec<WrappedLineInfo> {
    let total: usize = slots.iter().map(|(wm, _)| wm.len()).sum();
    let mut result = Vec::with_capacity(total);
    let mut visual_offset = 0usize;
    for (wm, lines_start) in slots {
        for entry in wm.iter() {
            result.push(WrappedLineInfo {
                logical_idx: entry.logical_idx + lines_start,
                visual_start: entry.visual_start + visual_offset,
                visual_end: entry.visual_end + visual_offset,
            });
        }
        visual_offset += wm.last().map(|e| e.visual_end).unwrap_or(0);
    }
    result
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

// ── 折行宽度模拟 ──────────────────────────────────────────────────────────

/// 用 ratatui `Paragraph::wrap` 渲染 `Line` 到 offscreen `Buffer`，按 cell 流匹配
/// plain text 字符，确定每个视觉行在该 plain text 中的 byte 起始偏移。
///
/// [Why] 旧实现用 `c = vis_col + (vis_row - visual_start) * width` 推算逻辑列，
/// 假设每个视觉行恰好占满 `width` 列。但 ratatui 用 `WordWrapper` 做 word-level
/// wrap：CJK 文本（无空格）被当成单个 word，超宽也不拆分；ASCII 文本在空格处
/// 优先换行；行尾字符宽度不一致（每行占列数不固定）。按 width×k 推算会在所有
/// 这些情况下累积偏移，导致复制结果与终端显示的高亮范围不一致。
///
/// 直接渲染到 Buffer 是唯一能 100% 复刻 ratatui wrap 行为的方法——和实际显示
/// 完全一致，鼠标点击的视觉行号自然对齐。
///
/// 返回值长度 = 视觉行数 + 1，元素依次是 row 0 起始 byte、row 1 起始 byte、...
/// 末行结束 byte（= `plain.len()`）。
fn wrap_byte_starts(line: &Line<'_>, plain: &str, width: u16) -> Vec<usize> {
    use ratatui_kit::ratatui::buffer::Buffer;
    use ratatui_kit::ratatui::layout::Rect;
    use ratatui_kit::ratatui::widgets::Widget;

    let width = width.max(1);
    if plain.is_empty() {
        return vec![0];
    }
    let line_count = Paragraph::new(ratatui_kit::ratatui::text::Text::from(line.clone()))
        .wrap(Wrap { trim: false })
        .line_count(width);
    if line_count <= 1 {
        return vec![0, plain.len()];
    }

    let area = Rect::new(0, 0, width, line_count as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(ratatui_kit::ratatui::text::Text::from(line.clone()))
        .wrap(Wrap { trim: false })
        .render(area, &mut buf);

    // 预计算 plain 的 char_index → byte_offset 表
    let mut char_byte: Vec<usize> = Vec::with_capacity(plain.chars().count() + 1);
    char_byte.push(0);
    let mut byte_off = 0usize;
    for ch in plain.chars() {
        byte_off += ch.len_utf8();
        char_byte.push(byte_off);
    }

    let mut starts: Vec<usize> = Vec::with_capacity(line_count + 1);
    starts.push(0);
    let mut chars_consumed = 0usize;
    let total_chars = plain.chars().count();
    let mut chars = plain.chars();

    for row in 0..(line_count as u16 - 1) {
        for col in 0..width {
            if chars_consumed >= total_chars {
                break;
            }
            let Some(cell) = buf.cell((col, row)) else {
                continue;
            };
            let sym = cell.symbol();
            if sym.is_empty() {
                continue; // 双宽字符的 continuation cell
            }
            let sym_first = sym.chars().next();
            let next_plain = chars.clone().next();
            if sym_first == next_plain {
                chars.next();
                chars_consumed += 1;
            } else if sym == " " && next_plain == Some(' ') {
                // trim:false 保留的 leading whitespace
                chars.next();
                chars_consumed += 1;
            }
            // 否则是 trailing padding 空格，跳过
        }
        starts.push(*char_byte.get(chars_consumed).unwrap_or(&plain.len()));
    }
    starts.push(plain.len());
    starts
}

/// 选区**起点** byte 偏移：target_col 落在字符占的列范围 [col, col+cw) 内时，
/// 总是返回该字符起点（含字符本身）。
fn row_start_byte(plain: &str, row_start_byte: usize, col_in_row: u16) -> usize {
    let head = plain.split_at(row_start_byte).1;
    let mut col = 0u16;
    for (i, ch) in head.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if cw > 0 && col_in_row < col + cw {
            return row_start_byte + i;
        }
        col += cw;
    }
    row_start_byte + head.len()
}

/// 选区**终点** byte 偏移：target_col 落在字符左半 → 返回字符起点（不含）；
/// 落在右半 → 返回字符终点（含）。语义见 `text_selection::visual_col_to_byte_offset`。
fn row_end_byte(plain: &str, row_start_byte: usize, col_in_row: u16) -> usize {
    let head = plain.split_at(row_start_byte).1;
    row_start_byte + text_selection::visual_col_to_byte_offset(head, col_in_row)
}

// ── 字符级高亮 ────────────────────────────────────────────────────────────

/// 根据选区高亮单条逻辑行（字符级精度）。
///
/// 调用方已经确保该逻辑行 `li` 在选区 `[first_logical, last_logical]` 内，
/// 通过 `wrap_map.get(li)` 取该行的 `WrappedLineInfo`（含 visual_start/visual_end）。
/// 函数计算该行被选中的 byte 范围 `[b_start, b_end)`，按 byte 范围拆分 spans，
/// 对落在范围内的字符 span 加 `sel_bg` 背景。
///
/// - **首行**（sel_sr 落在该行视觉范围内）：起始 byte = sr_off 行内 col sc
/// - **末行**（sel_er 落在该行视觉范围内）：结束 byte = er_off 行内 col ec
/// - **中间行**（既不是首行也不是末行）：整行高亮 byte = [0, plain.len())
/// - **首+末同行**：byte 范围 = [sr_off 行内 sc, er_off 行内 ec]
///
/// [Why] 旧实现是整逻辑行高亮（粗粒度），与字符级复制提取不一致——用户看到的高亮
/// 范围比实际复制内容大。改为字符级后高亮范围 = 复制范围，与终端鼠标拖拽选择一致。
#[allow(clippy::too_many_arguments)]
pub(super) fn highlight_line_in_selection(
    line: &Line<'static>,
    entry: &WrappedLineInfo,
    sel_sr: u16,
    sel_er: u16,
    sel_sc: u16,
    sel_ec: u16,
    width: u16,
    sel_bg: ratatui_kit::ratatui::style::Color,
) -> Line<'static> {
    let plain = text_selection::line_to_plain_text(line);
    let row_starts = wrap_byte_starts(line, &plain, width);
    let row_max = row_starts.len().saturating_sub(1);

    let sr_in_line =
        (sel_sr as usize) >= entry.visual_start && (sel_sr as usize) < entry.visual_end;
    let er_in_line =
        (sel_er as usize) >= entry.visual_start && (sel_er as usize) < entry.visual_end;

    // sr_off / er_off 是选区起点/终点视觉行相对该逻辑行 visual_start 的偏移
    let sr_off = (sel_sr as usize)
        .saturating_sub(entry.visual_start)
        .min(row_max);
    let er_off = (sel_er as usize)
        .saturating_sub(entry.visual_start)
        .min(row_max);

    let (b_start, b_end) = if sr_in_line && er_in_line {
        // 同一行：byte 范围 = [sr_off 行内 sc, er_off 行内 ec]
        let s = row_start_byte(&plain, row_starts[sr_off], sel_sc);
        let e = row_end_byte(&plain, row_starts[er_off], sel_ec);
        (s, e)
    } else if sr_in_line {
        // 仅首行：起始 sc，结束 = plain 末尾
        let s = row_start_byte(&plain, row_starts[sr_off], sel_sc);
        (s, plain.len())
    } else if er_in_line {
        // 仅末行：起始 = 0，结束 ec
        let e = row_end_byte(&plain, row_starts[er_off], sel_ec);
        (0, e)
    } else {
        // 中间行：整行高亮
        (0, plain.len())
    };

    if b_start >= b_end {
        return line.clone();
    }
    split_line_spans_by_byte_range(line, b_start, b_end, sel_bg)
}

/// 按字节范围 `[b_start, b_end)` 拆分 line 的 spans，对落在范围内的字符追加 `sel_bg` 背景。
///
/// 该函数处理三种情况：
/// - span 完全在范围外：原样保留
/// - span 完全在范围内：整个 span 加 sel_bg
/// - span 部分重叠：拆成前/中/后三段，仅中间段加 sel_bg
fn split_line_spans_by_byte_range(
    line: &Line<'static>,
    b_start: usize,
    b_end: usize,
    sel_bg: ratatui_kit::ratatui::style::Color,
) -> Line<'static> {
    use ratatui_kit::ratatui::text::Span;

    let mut cur = 0usize;
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .flat_map(|s| {
            let len = s.content.len();
            let end = cur + len;
            let overlap_start = cur.max(b_start);
            let overlap_end = end.min(b_end);
            let base = cur;
            cur = end;

            if overlap_start >= overlap_end {
                vec![Span::styled(s.content.clone(), s.style)]
            } else if overlap_start == base && overlap_end == end {
                vec![Span::styled(s.content.clone(), s.style.bg(sel_bg))]
            } else {
                let mut parts: Vec<Span<'static>> = Vec::new();
                let raw: &str = s.content.as_ref();
                if overlap_start > base {
                    parts.push(Span::styled(
                        raw[..overlap_start - base].to_string(),
                        s.style,
                    ));
                }
                let mid_start = overlap_start - base;
                let mid_end = overlap_end - base;
                parts.push(Span::styled(
                    raw[mid_start..mid_end].to_string(),
                    s.style.bg(sel_bg),
                ));
                if overlap_end < end {
                    parts.push(Span::styled(raw[mid_end..].to_string(), s.style));
                }
                parts
            }
        })
        .collect();
    Line::from(spans)
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
/// 每个视觉行的起始 byte 偏移由 `wrap_byte_starts` 用 `unicode_width` 模拟
/// ratatui `Paragraph::wrap` 得到，避免"width×k"假设在 CJK 文本上累积偏移。
/// 行内列号再由 `visual_col_to_byte_offset` 转为 byte 偏移，二者共享同一套
/// 宽度语义，保证选区高亮范围与复制结果一致。
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
        // 每个视觉行在该逻辑行 plain text 中的 byte 起始偏移
        let row_starts = wrap_byte_starts(line, &plain, width);
        // 把视觉行号 clamp 到 row_starts 索引范围内（防御：footer 区域等异常 sr/er）
        let row_max = row_starts.len().saturating_sub(1);
        let sr_off = (sr as usize)
            .saturating_sub(entry.visual_start)
            .min(row_max);
        let er_off = (er as usize)
            .saturating_sub(entry.visual_start)
            .min(row_max);

        if first == last {
            // 同一逻辑行：起点行 sr_off 内的列 sc → 终点行 er_off 内的列 ec
            let s_row_byte = row_starts[sr_off];
            let e_row_byte = row_starts[er_off];
            let b0 = row_start_byte(&plain, s_row_byte, sc);
            let b1 = row_end_byte(&plain, e_row_byte, ec);
            if b0 >= b1 {
                continue;
            }
            parts.push(plain[b0..b1].to_string());
        } else if li == first {
            // 首行：从 sr_off 行内的列 sc 到逻辑行末尾
            let s_row_byte = row_starts[sr_off];
            let b0 = row_start_byte(&plain, s_row_byte, sc);
            parts.push(plain[b0..].to_string());
        } else if li == last {
            // 末行：从逻辑行开头到 er_off 行内的列 ec
            let e_row_byte = row_starts[er_off];
            let b1 = row_end_byte(&plain, e_row_byte, ec);
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

// ── 测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui_kit::ratatui::text::Span;

    fn make_line(s: &str) -> Line<'static> {
        Line::from(vec![Span::raw(s.to_string())])
    }

    // ── concat_wrap_maps ──

    fn make_wrap_entry(logical: usize, start: usize, end: usize) -> WrappedLineInfo {
        WrappedLineInfo {
            logical_idx: logical,
            visual_start: start,
            visual_end: end,
        }
    }

    #[test]
    fn test_concat_wrap_maps_empty_input_returns_empty() {
        let result = concat_wrap_maps(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_concat_wrap_maps_single_slot_preserves_entries() {
        // 单个分片：偏移为 0，logical_idx 不变
        let slot = vec![
            make_wrap_entry(0, 0, 1),
            make_wrap_entry(1, 1, 3),
            make_wrap_entry(2, 3, 4),
        ];
        let result = concat_wrap_maps(&[(&slot, 0)]);
        assert_eq!(
            result,
            vec![
                make_wrap_entry(0, 0, 1),
                make_wrap_entry(1, 1, 3),
                make_wrap_entry(2, 3, 4),
            ]
        );
    }

    #[test]
    fn test_concat_wrap_maps_multi_slots_accumulates_offsets() {
        // 3 个分片，各自内部 visual_row 从 0 起；拼接后累加 visual_offset 和 logical_idx
        // slot0: 2 行（visual 0-2），lines_start=0
        // slot1: 1 行（visual 0-1），lines_start=2（slot0 占用 2 个 logical line）
        // slot2: 1 行（visual 0-1），lines_start=3
        let slot0 = vec![make_wrap_entry(0, 0, 1), make_wrap_entry(1, 1, 2)];
        let slot1 = vec![make_wrap_entry(0, 0, 1)];
        let slot2 = vec![make_wrap_entry(0, 0, 1)];
        let result = concat_wrap_maps(&[(&slot0, 0), (&slot1, 2), (&slot2, 3)]);
        assert_eq!(
            result,
            vec![
                // slot0 原样
                make_wrap_entry(0, 0, 1),
                make_wrap_entry(1, 1, 2),
                // slot1: visual += 2, logical += 2
                make_wrap_entry(2, 2, 3),
                // slot2: visual += 3, logical += 3
                make_wrap_entry(3, 3, 4),
            ]
        );
    }

    #[test]
    fn test_concat_wrap_maps_supports_multi_visual_rows_per_line() {
        // 单条逻辑行 wrap 成多视觉行：分片 1 一条 line 占 visual 0-3
        let slot0 = vec![make_wrap_entry(0, 0, 3)];
        let slot1 = vec![make_wrap_entry(0, 0, 1), make_wrap_entry(1, 1, 2)];
        let result = concat_wrap_maps(&[(&slot0, 0), (&slot1, 1)]);
        assert_eq!(
            result,
            vec![
                make_wrap_entry(0, 0, 3),
                // slot1 第一行 visual_start += 3, logical_idx += 1
                make_wrap_entry(1, 3, 4),
                // slot1 第二行 visual_start/end += 3, logical_idx += 1
                make_wrap_entry(2, 4, 5),
            ]
        );
    }

    // ── wrap_byte_starts ──

    #[test]
    fn test_wrap_byte_starts_ascii_no_wrap() {
        let line = make_line("abcdef");
        assert_eq!(wrap_byte_starts(&line, "abcdef", 10), vec![0, 6]);
    }

    #[test]
    fn test_wrap_byte_starts_ascii_wrap() {
        let line = make_line("abcdef");
        // width=3: 行 0="abc", 行 1="def"
        assert_eq!(wrap_byte_starts(&line, "abcdef", 3), vec![0, 3, 6]);
    }

    #[test]
    fn test_wrap_byte_starts_cjk_no_wrap() {
        let line = make_line("你好");
        assert_eq!(wrap_byte_starts(&line, "你好", 10), vec![0, 6]);
    }

    #[test]
    fn test_wrap_byte_starts_mixed_cjk_word_wrap() {
        // ratatui WordWrapper 行为：CJK 整段算一个 word，超宽不拆分
        // plain="abc你好你好" w=5 → 行 0="abc你"(byte 0..6), 行 1="好你好"(byte 6..15, 溢出但保留)
        let line = make_line("abc你好你好");
        assert_eq!(wrap_byte_starts(&line, "abc你好你好", 5), vec![0, 6, 15]);
    }

    #[test]
    fn test_wrap_byte_starts_mixed_cjk_wider() {
        // w=4: 行 0="abc", 行 1="你好"(trim:false 下 WordWrapper 在 word 边界处拆，但 CJK 无空格
        // 整段 word "abc你好你好" 超过 4 列。观察：abc 你好 / 你好
        let line = make_line("abc你好你好");
        let starts = wrap_byte_starts(&line, "abc你好你好", 4);
        assert_eq!(starts, vec![0, 3, 9, 15]);
    }

    #[test]
    fn test_wrap_byte_starts_empty() {
        let line = make_line("");
        assert_eq!(wrap_byte_starts(&line, "", 10), vec![0]);
    }

    #[test]
    fn test_wrap_byte_starts_single_cjk() {
        let line = make_line("你");
        assert_eq!(wrap_byte_starts(&line, "你", 10), vec![0, 3]);
    }

    #[test]
    fn test_wrap_byte_starts_row_count_matches_ratatui() {
        // 关键不变量：wrap_byte_starts 推算的视觉行数必须与 ratatui Paragraph::line_count
        // 一致——否则鼠标点击的视觉行号与提取算法的视觉行号错位
        for (text, width) in [
            ("abc你好你好", 5u16),
            ("abc你好你好", 4),
            ("abc你好", 4),
            ("你好世界", 3),
            ("你好世界", 5),
            ("abcdef", 3),
            ("hello world", 5),
        ] {
            let line = make_line(text);
            let line_count = Paragraph::new(ratatui_kit::ratatui::text::Text::from(line.clone()))
                .wrap(Wrap { trim: false })
                .line_count(width);
            let our_rows = wrap_byte_starts(&line, text, width)
                .len()
                .saturating_sub(1)
                .max(1);
            assert_eq!(
                our_rows, line_count,
                "wrap_byte_starts 行数 {} 与 ratatui line_count {} 不一致（text={:?}, width={}）",
                our_rows, line_count, text, width
            );
        }
    }

    // ── extract_visual_range ──

    #[test]
    fn test_extract_cjk_same_visual_row_double_width_right_half() {
        // ratatui w=5: 行 0="abc你", 行 1="好你好"（WordWrapper 不拆 word，溢出保留）
        // 用户拖 col 0 到 col 3（'你' 右半，含 '你'）→ 期望 "好你"
        let lines = vec![make_line("abc你好你好")];
        let (_, wrap_map) = build_wrap_map(&lines, 5);
        let result = extract_visual_range(&lines, &wrap_map, (1, 0), (1, 3), 5);
        assert_eq!(result.as_deref(), Some("好你"));
    }

    #[test]
    fn test_extract_cjk_same_visual_row_left_half_excludes_char() {
        // 同 row 1，拖 col 0 到 col 2（'你' 左半，不含 '你'）→ "好"
        let lines = vec![make_line("abc你好你好")];
        let (_, wrap_map) = build_wrap_map(&lines, 5);
        let result = extract_visual_range(&lines, &wrap_map, (1, 0), (1, 2), 5);
        assert_eq!(result.as_deref(), Some("好"));
    }

    #[test]
    fn test_extract_cjk_first_row_partial() {
        // 行 0="abc你"：col 0 到 col 4（'你' 右半，含）→ "abc你"
        let lines = vec![make_line("abc你好你好")];
        let (_, wrap_map) = build_wrap_map(&lines, 5);
        let result = extract_visual_range(&lines, &wrap_map, (0, 0), (0, 4), 5);
        assert_eq!(result.as_deref(), Some("abc你"));
    }

    #[test]
    fn test_extract_cjk_cross_visual_row() {
        // 跨视觉行：行 0 col 4（'你' 右半，含）→ 行 1 col 1（'好' 右半，含）
        // 行 0 含 '你'，行 1 含 '好'（行 1 第 1 个字符）
        // 期望 '你' + '好' = "你好"
        let lines = vec![make_line("abc你好你好")];
        let (_, wrap_map) = build_wrap_map(&lines, 5);
        let result = extract_visual_range(&lines, &wrap_map, (0, 4), (1, 1), 5);
        assert_eq!(result.as_deref(), Some("你好"));
    }

    #[test]
    fn test_extract_ascii_same_row() {
        let lines = vec![make_line("abcdef")];
        let (_, wrap_map) = build_wrap_map(&lines, 10);
        let result = extract_visual_range(&lines, &wrap_map, (0, 1), (0, 3), 10);
        assert_eq!(result.as_deref(), Some("bc"));
    }

    #[test]
    fn test_extract_ascii_cross_visual_row() {
        // 视觉行 0="abc" col 2，视觉行 1="def" col 1 → "cd"
        let lines = vec![make_line("abcdef")];
        let (_, wrap_map) = build_wrap_map(&lines, 3);
        let result = extract_visual_range(&lines, &wrap_map, (0, 2), (1, 1), 3);
        assert_eq!(result.as_deref(), Some("cd"));
    }

    #[test]
    fn test_extract_cross_logical_row() {
        // 跨逻辑行：(0,1) → (1,2) = "bc" + "de"（用 \n 连接）
        let lines = vec![make_line("abc"), make_line("def")];
        let (_, wrap_map) = build_wrap_map(&lines, 10);
        let result = extract_visual_range(&lines, &wrap_map, (0, 1), (1, 2), 10);
        assert_eq!(result.as_deref(), Some("bc\nde"));
    }

    #[test]
    fn test_extract_swapped_start_end_normalizes() {
        // 反向拖拽：vis_start > vis_end 应规范化
        let lines = vec![make_line("abcdef")];
        let (_, wrap_map) = build_wrap_map(&lines, 10);
        let result = extract_visual_range(&lines, &wrap_map, (0, 3), (0, 1), 10);
        assert_eq!(result.as_deref(), Some("bc"));
    }

    // ── highlight_line_in_selection（字符级高亮）──

    use ratatui_kit::ratatui::style::{Color, Style};

    const TEST_BG: Color = Color::Rgb(1, 2, 3);

    /// 提取 line 中带 `bg` 背景的 span 内容拼接，用于断言"实际被高亮的字符"。
    fn highlighted_text(line: &Line<'_>, bg: Color) -> String {
        line.spans
            .iter()
            .filter(|s| s.style.bg == Some(bg))
            .map(|s| s.content.as_ref())
            .collect::<Vec<&str>>()
            .concat()
    }

    #[test]
    fn test_highlight_ascii_same_row_partial() {
        // line="abcdef" w=10：单视觉行，sel=(0,1)→(0,3) → byte 范围 [1,3)，高亮 "bc"
        let line = make_line("abcdef");
        let entry = WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 1,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 1, 3, 10, TEST_BG);
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "bc");
    }

    #[test]
    fn test_highlight_cjk_same_visual_row_double_width_right_half() {
        // line="abc你好你好" w=5：行 0="abc你", 行 1="好你好"
        // sel=(1,0)→(1,3) → '好' 占 col 0-1（右半 col 1 含），'你' 占 col 2-3（右半 col 3 含）
        // 期望高亮 "好你"（byte 6..12）
        let line = make_line("abc你好你好");
        let entry = WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 2,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 1, 1, 0, 3, 5, TEST_BG);
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "好你");
    }

    #[test]
    fn test_highlight_cjk_cross_visual_row_first_and_last_same_logical() {
        // 同一逻辑行的跨视觉行选区：行 0 col 4（'你' 右半含）→ 行 1 col 1（'好' 右半含）
        // sr_in_line=true (sr=0)，er_in_line=true (er=1)
        // byte 范围 [3, 9)：高亮 "你好"
        let line = make_line("abc你好你好");
        let entry = WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 2,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 1, 4, 1, 5, TEST_BG);
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "你好");
    }

    #[test]
    fn test_highlight_first_logical_only_partial_to_end() {
        // lines=["abc","def"]：首行 "abc" sel=(0,1)→(1,2)
        // 首行 is_first=true, is_last=false → byte 范围 [1,3)，高亮 "bc"
        let line = make_line("abc");
        let entry = WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 1,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 1, 1, 2, 10, TEST_BG);
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "bc");
    }

    #[test]
    fn test_highlight_last_logical_only_start_to_col() {
        // lines=["abc","def"]：末行 "def" sel=(0,1)→(1,2)
        // 末行 is_first=false, is_last=true → byte 范围 [0,2)，高亮 "de"
        let line = make_line("def");
        // 末行的 visual_start=1（第二逻辑行），sel_sr=0（不在该行），sel_er=1（在该行）
        let entry = WrappedLineInfo {
            logical_idx: 1,
            visual_start: 1,
            visual_end: 2,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 1, 1, 2, 10, TEST_BG);
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "de");
    }

    #[test]
    fn test_highlight_middle_logical_full_line() {
        // lines=["abc","def","ghi"]：中间行 "def" sel=(0,0)→(2,0)
        // 中间行 is_first=false, is_last=false → 整行 [0,3)，高亮 "def"
        let line = make_line("def");
        let entry = WrappedLineInfo {
            logical_idx: 1,
            visual_start: 1,
            visual_end: 2,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 2, 0, 0, 10, TEST_BG);
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "def");
    }

    #[test]
    fn test_highlight_multi_span_partial_overlap_splits_span() {
        // line = [Span("hello"), Span("world")]（两个独立 span，每个 byte=5）
        // sel 覆盖 byte [3,7) → "lo" + "wo"，应拆分两个 span 各成 前缀未高亮 + 中段高亮 + 后缀
        let line = Line::from(vec![
            Span::raw("hello".to_string()),
            Span::raw("world".to_string()),
        ]);
        let entry = WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 1,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 3, 7, 20, TEST_BG);
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "lowo");
        // 完整文本保留原顺序
        let full: String = highlighted
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<&str>>()
            .concat();
        assert_eq!(full, "helloworld");
    }

    #[test]
    fn test_highlight_empty_byte_range_returns_original() {
        // line="abcdef" w=10，sel=(0,1)→(0,1)（同点）→ byte 范围空 → 返回 clone 原行
        let line = make_line("abcdef");
        let entry = WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 1,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 1, 1, 10, TEST_BG);
        // 无 span 高亮
        assert_eq!(highlighted_text(&highlighted, TEST_BG), "");
        // 完整文本仍为 "abcdef"
        let full: String = highlighted
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<&str>>()
            .concat();
        assert_eq!(full, "abcdef");
    }

    #[test]
    fn test_highlight_preserves_existing_span_style() {
        // line = Span::styled("abc", Style::fg(red))——验证高亮叠加 bg 但保留 fg
        use ratatui_kit::ratatui::style::Color as C;
        let line = Line::from(vec![Span::styled(
            "abc".to_string(),
            Style::default().fg(C::Red),
        )]);
        let entry = WrappedLineInfo {
            logical_idx: 0,
            visual_start: 0,
            visual_end: 1,
        };
        let highlighted = highlight_line_in_selection(&line, &entry, 0, 0, 0, 2, 10, TEST_BG);
        // 高亮部分 "ab" 应同时保留 fg=Red + bg=TEST_BG
        let highlighted_span = highlighted
            .spans
            .iter()
            .find(|s| s.style.bg == Some(TEST_BG))
            .expect("应有被高亮的 span");
        assert_eq!(highlighted_span.style.fg, Some(C::Red));
        assert_eq!(highlighted_span.content.as_ref(), "ab");
    }
}
