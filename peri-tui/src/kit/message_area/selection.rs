//! 文本选区 + 折行映射：wrap_map 构建、视觉→逻辑行转换、选区提取、剪贴板复制。

use std::cmp::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL};
use crate::kit::message_area::grid::GridSpec;
use crate::kit::text_selection;
use ratatui_kit::ratatui::text::Line;
use ratatui_kit::ratatui::widgets::{Paragraph, Wrap};

// ── wrap_map 类型 ──────────────────────────────────────────────────────────

/// 折行映射条目：逻辑行索引 + 该逻辑行占据的视觉行范围 [visual_start, visual_end)。
/// [Scheme D] slot_index 标识该逻辑行所属的 VmCacheSlot，替换全局 core_lines_arc 索引。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct WrappedLineInfo {
    pub(super) logical_idx: usize,
    pub(super) visual_start: usize,
    pub(super) visual_end: usize,
    pub(super) slot_index: usize,
}

/// 为 all_lines 构建视觉行→逻辑行映射。
/// 返回 (total_visual_rows, wrap_map)。wrap_map 按 visual_start 升序排列，可二分查找。
pub(super) fn build_wrap_map(lines: &[Line<'static>], width: u16) -> (usize, Vec<WrappedLineInfo>) {
    #[cfg(test)]
    crate::kit::acp_bridge::observe_perf(
        crate::kit::acp_bridge::PerfCounter::WrapRecalculatedLines,
        lines.len() as u64,
    );
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
            slot_index: 0,
        });
        visual_row += rows;
    }
    (visual_row, wrap_map)
}

#[derive(Debug, Clone)]
struct SlotLinePart {
    lines: Arc<Vec<Line<'static>>>,
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SlotLines {
    parts: Vec<SlotLinePart>,
    prefix: Vec<usize>,
}

impl SlotLines {
    pub(super) fn single(lines: Arc<Vec<Line<'static>>>) -> Self {
        let end = lines.len();
        Self::from_parts(vec![SlotLinePart {
            lines,
            start: 0,
            end,
        }])
    }

    pub(super) fn composite(
        mutable: Arc<Vec<Line<'static>>>,
        stable_start: usize,
        stable: Vec<Arc<Vec<Line<'static>>>>,
    ) -> Self {
        let mut parts = Vec::with_capacity(stable.len().saturating_add(2));
        let split = stable_start.min(mutable.len());
        parts.push(SlotLinePart {
            lines: Arc::clone(&mutable),
            start: 0,
            end: split,
        });
        let stable_len = stable.iter().map(|lines| lines.len()).sum::<usize>();
        let suffix_start = split.saturating_add(stable_len).min(mutable.len());
        parts.extend(stable.into_iter().map(|lines| {
            let end = lines.len();
            SlotLinePart {
                lines,
                start: 0,
                end,
            }
        }));
        parts.push(SlotLinePart {
            end: mutable.len(),
            lines: mutable,
            start: suffix_start,
        });
        Self::from_parts(parts)
    }

    fn from_parts(parts: Vec<SlotLinePart>) -> Self {
        let mut prefix = Vec::with_capacity(parts.len().saturating_add(1));
        prefix.push(0usize);
        for part in &parts {
            prefix.push(
                prefix
                    .last()
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(part.end.saturating_sub(part.start)),
            );
        }
        Self { parts, prefix }
    }

    pub(super) fn len(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0)
    }

    pub(super) fn get(&self, logical: usize) -> Option<&Line<'static>> {
        if logical >= self.len() {
            return None;
        }
        let part_index = self
            .prefix
            .partition_point(|&start| start <= logical)
            .saturating_sub(1);
        let part = self.parts.get(part_index)?;
        part.lines
            .get(part.start + logical.saturating_sub(self.prefix[part_index]))
    }

    pub(super) fn line(&self, logical: usize) -> Option<&Line<'static>> {
        self.get(logical)
    }
}

impl From<Arc<Vec<Line<'static>>>> for SlotLines {
    fn from(lines: Arc<Vec<Line<'static>>>) -> Self {
        Self::single(lines)
    }
}

/// 每帧按 VM slot 构建的两级索引。prefix 的最后一个元素是总量；空 slot 也保留，
/// 因而全局坐标查找只需先二分 slot，再二分该 slot 的 local wrap map。
#[derive(Debug, Clone, Default)]
pub(super) struct SlotIndex {
    slots: Vec<SlotLines>,
    wrap_maps: Vec<Arc<Vec<WrappedLineInfo>>>,
    logical_prefix: Vec<usize>,
    visual_prefix: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SlotLookup {
    pub(super) slot_index: usize,
    pub(super) local_logical: usize,
    pub(super) global_logical: usize,
    pub(super) global_visual_start: usize,
    pub(super) global_visual_end: usize,
}

impl SlotIndex {
    #[cfg(test)]
    pub(super) fn new<T>(slots: Vec<T>, wrap_maps: Vec<Arc<Vec<WrappedLineInfo>>>) -> Self
    where
        T: Into<SlotLines>,
    {
        Self::new_with_overlays(slots.into_iter().map(Into::into).collect(), wrap_maps)
    }

    pub(super) fn new_with_overlays(
        slots: Vec<SlotLines>,
        wrap_maps: Vec<Arc<Vec<WrappedLineInfo>>>,
    ) -> Self {
        debug_assert_eq!(slots.len(), wrap_maps.len());
        let mut logical_prefix = Vec::with_capacity(slots.len().saturating_add(1));
        let mut visual_prefix = Vec::with_capacity(slots.len().saturating_add(1));
        logical_prefix.push(0);
        visual_prefix.push(0);
        for (lines, wrap_map) in slots.iter().zip(&wrap_maps) {
            logical_prefix.push(
                logical_prefix
                    .last()
                    .copied()
                    .unwrap_or(0usize)
                    .saturating_add(lines.len()),
            );
            visual_prefix.push(
                visual_prefix
                    .last()
                    .copied()
                    .unwrap_or(0usize)
                    .saturating_add(wrap_map.last().map_or(0, |entry| entry.visual_end)),
            );
        }
        Self {
            slots,
            wrap_maps,
            logical_prefix,
            visual_prefix,
        }
    }

    pub(super) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub(super) fn total_logical(&self) -> usize {
        self.logical_prefix.last().copied().unwrap_or(0)
    }

    pub(super) fn total_visual(&self) -> usize {
        self.visual_prefix.last().copied().unwrap_or(0)
    }

    pub(super) fn slot_visual_start(&self, slot: usize) -> Option<usize> {
        self.visual_prefix.get(slot).copied()
    }

    pub(super) fn slot_visual_range(&self, slot: usize) -> Option<(usize, usize)> {
        let start = *self.visual_prefix.get(slot)?;
        let end = *self.visual_prefix.get(slot.checked_add(1)?)?;
        (start < end).then_some((start, end))
    }

    fn prefix_slot(prefix: &[usize], value: usize) -> Option<usize> {
        let total = *prefix.last()?;
        if value >= total {
            return None;
        }
        Some(
            prefix
                .partition_point(|&start| start <= value)
                .saturating_sub(1),
        )
    }

    pub(super) fn visual_lookup(&self, visual: usize) -> Option<SlotLookup> {
        let slot_index = Self::prefix_slot(&self.visual_prefix, visual)?;
        let local_visual = visual.saturating_sub(self.visual_prefix[slot_index]);
        let wrap_map = self.wrap_maps.get(slot_index)?;
        let entry_index = wrap_map
            .binary_search_by(|entry| {
                if local_visual < entry.visual_start {
                    Ordering::Greater
                } else if local_visual >= entry.visual_end {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            })
            .ok()?;
        let entry = &wrap_map[entry_index];
        let global_logical = self.logical_prefix[slot_index].saturating_add(entry.logical_idx);
        Some(SlotLookup {
            slot_index,
            local_logical: entry.logical_idx,
            global_logical,
            global_visual_start: self.visual_prefix[slot_index].saturating_add(entry.visual_start),
            global_visual_end: self.visual_prefix[slot_index].saturating_add(entry.visual_end),
        })
    }

    pub(super) fn logical_lookup(&self, logical: usize) -> Option<SlotLookup> {
        let slot_index = Self::prefix_slot(&self.logical_prefix, logical)?;
        let local_logical = logical.saturating_sub(self.logical_prefix[slot_index]);
        let entry = self.wrap_maps.get(slot_index)?.get(local_logical)?;
        Some(SlotLookup {
            slot_index,
            local_logical,
            global_logical: logical,
            global_visual_start: self.visual_prefix[slot_index].saturating_add(entry.visual_start),
            global_visual_end: self.visual_prefix[slot_index].saturating_add(entry.visual_end),
        })
    }

    pub(super) fn line(&self, slot: usize, local: usize) -> Option<&Line<'static>> {
        self.slots.get(slot)?.line(local)
    }

    pub(super) fn viewport_logical_range(
        &self,
        scroll_y: usize,
        vp_height: usize,
    ) -> Option<(usize, usize, usize)> {
        if vp_height == 0 {
            return None;
        }
        let start = self.visual_lookup(scroll_y)?;
        let end_visual = scroll_y
            .saturating_add(vp_height)
            .saturating_sub(1)
            .min(self.total_visual().saturating_sub(1));
        let end = self.visual_lookup(end_visual)?;
        Some((
            start.global_logical,
            end.global_logical,
            scroll_y.saturating_sub(start.global_visual_start),
        ))
    }
}

#[cfg(test)]
pub(crate) fn run_synthetic_slot_index(slots: usize) -> (usize, usize) {
    let lines = (0..slots)
        .map(|slot| Arc::new(vec![Line::from(format!("slot-{slot}"))]))
        .collect::<Vec<_>>();
    let maps = (0..slots)
        .map(|_| {
            Arc::new(vec![WrappedLineInfo {
                logical_idx: 0,
                visual_start: 0,
                visual_end: 1,
                slot_index: 0,
            }])
        })
        .collect();
    let index = SlotIndex::new(lines, maps);
    (index.logical_prefix.len(), index.visual_prefix.len())
}

/// 拼接多个 VM 的 wrap_map：仅保留为新索引的测试 reference。
#[cfg(test)]
pub(super) fn concat_wrap_maps(
    slots: &[(&[WrappedLineInfo], usize, usize)],
) -> Vec<WrappedLineInfo> {
    let total: usize = slots.iter().map(|(wm, _, _)| wm.len()).sum();
    #[cfg(test)]
    {
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::AggregateAllocation,
            1,
        );
        crate::kit::acp_bridge::observe_perf(
            crate::kit::acp_bridge::PerfCounter::AggregateCopiedItems,
            total as u64,
        );
    }
    let mut result = Vec::with_capacity(total);
    let mut visual_offset = 0usize;
    for (wm, lines_start, slot_index) in slots {
        for entry in wm.iter() {
            result.push(WrappedLineInfo {
                logical_idx: entry.logical_idx + lines_start,
                visual_start: entry.visual_start + visual_offset,
                visual_end: entry.visual_end + visual_offset,
                slot_index: *slot_index,
            });
        }
        visual_offset += wm.last().map(|e| e.visual_end).unwrap_or(0);
    }
    result
}

/// 二分查找：视觉行 → 逻辑行索引。
#[cfg(test)]
pub(super) fn visual_to_logical(visual_row: usize, wrap_map: &[WrappedLineInfo]) -> Option<usize> {
    let vr = visual_row;
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

    if line_count > u16::MAX as usize {
        return wrap_byte_starts_large_line(plain, usize::from(width));
    }

    let height = u16::try_from(line_count).expect("line count checked against u16::MAX");
    let area = Rect::new(0, 0, width, height);
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

    for row in 0..height.saturating_sub(1) {
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

fn wrap_byte_starts_large_line(plain: &str, width: usize) -> Vec<usize> {
    use unicode_width::UnicodeWidthChar;

    let mut starts = vec![0];
    let mut row_width = 0usize;
    for (byte, ch) in plain.char_indices() {
        let char_width = ch.width().unwrap_or(0);
        if row_width > 0 && row_width.saturating_add(char_width) > width {
            starts.push(byte);
            row_width = 0;
        }
        row_width = row_width.saturating_add(char_width);
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
/// sel_sr/sel_er 为视觉行（usize，内容可超 65535 视觉行），sel_sc/sel_ec 为视觉列（u16）。
#[allow(clippy::too_many_arguments)]
pub(super) fn highlight_line_in_selection(
    line: &Line<'static>,
    entry: &WrappedLineInfo,
    sel_sr: usize,
    sel_er: usize,
    sel_sc: u16,
    sel_ec: u16,
    width: u16,
    sel_bg: ratatui_kit::ratatui::style::Color,
) -> Line<'static> {
    let plain = text_selection::line_to_plain_text(line);
    let row_starts = wrap_byte_starts(line, &plain, width);
    let row_max = row_starts.len().saturating_sub(1);

    let sr_in_line = sel_sr >= entry.visual_start && sel_sr < entry.visual_end;
    let er_in_line = sel_er >= entry.visual_start && sel_er < entry.visual_end;

    // sr_off / er_off 是选区起点/终点视觉行相对该逻辑行 visual_start 的偏移
    let sr_off = sel_sr.saturating_sub(entry.visual_start).min(row_max);
    let er_off = sel_er.saturating_sub(entry.visual_start).min(row_max);

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
/// [Scheme D] 通过 slot_arcs + slot_offsets 按需从 slot 中解析行，不再依赖全量 lines 切片。
/// vis_start/vis_end 为视觉坐标：行 usize（内容可超 65535 视觉行）、列 u16。
///
/// [D3 §9] 语义复制剥离：`view_models`/`grid` 提供时，对每行调用
/// `render::semantic_line_text` 取语义文本（无前缀 chrome/符号/行号）——
/// 完整行直接用语义全文；首/末部分选择行把列范围映射到语义文本上
/// （`plain.find(semantic)` 定位；重建行（tool header）定位失败时回退原始
/// 提取）。`view_models` 为 None 时保持既有行为（列模拟不侵入，剥离只发生
/// 在提取层——wrap_byte_starts / 高亮列模型零改动）。
#[allow(clippy::too_many_arguments)] // 选区提取参数组（与既有调用链签名一致）
pub(super) fn extract_visual_range_index(
    index: &SlotIndex,
    vis_start: (usize, u16),
    vis_end: (usize, u16),
    width: u16,
    view_models: Option<&im::Vector<crate::kit::tui_render_unit::TuiRenderUnit>>,
    grid: Option<GridSpec>,
) -> Option<String> {
    let ((sr, sc), (er, ec)) = if vis_start <= vis_end {
        (vis_start, vis_end)
    } else {
        (vis_end, vis_start)
    };
    // Clamp sr/er 到 core 视觉范围内（footer 区域无映射，避免 None）
    let max_visual = index.total_visual().saturating_sub(1);
    let sr = sr.min(max_visual);
    let er = er.min(max_visual);
    let first_logical = index.visual_lookup(sr)?.global_logical;
    let last_logical = index.visual_lookup(er)?.global_logical;
    let first = first_logical.min(last_logical);
    let last = first_logical.max(last_logical);

    let mut parts: Vec<String> = Vec::new();
    for li in first..=last {
        let lookup = index.logical_lookup(li)?;
        let entry = WrappedLineInfo {
            logical_idx: li,
            visual_start: lookup.global_visual_start,
            visual_end: lookup.global_visual_end,
            slot_index: lookup.slot_index,
        };
        let local_idx = lookup.local_logical;
        let line = index.line(lookup.slot_index, local_idx)?;
        let plain = text_selection::line_to_plain_text(line);
        // [D3 §9] 语义文本（无 UI chrome）——仅提取层替换，列模拟仍基于 plain。
        // [Fix §15] 传入已渲染行而非重渲染 VM：slot 行与渲染缓存同源，
        // 避免 N 行选区 × N 次全量 markdown 解析（旧实现每行新建缓存重渲染）。
        let semantic: Option<(String, usize)> = view_models
            .and_then(|vms| vms.get(entry.slot_index))
            .and_then(|vm| {
                grid.and_then(|g| super::render::semantic_line_text(vm, local_idx, line, &g))
            })
            .map(|sem| {
                // 语义文本在 plain 中的定位（重建行如 tool header 可能不是
                // 连续子串——find 失败时映射回退原始提取）。
                let p = plain.find(sem.as_str()).unwrap_or(0);
                (sem, p)
            });
        // 每个视觉行在该逻辑行 plain text 中的 byte 起始偏移
        let row_starts = wrap_byte_starts(line, &plain, width);
        // 把视觉行号 clamp 到 row_starts 索引范围内（防御：footer 区域等异常 sr/er）
        let row_max = row_starts.len().saturating_sub(1);
        let sr_off = sr.saturating_sub(entry.visual_start).min(row_max);
        let er_off = er.saturating_sub(entry.visual_start).min(row_max);

        if first == last {
            // 同一逻辑行：起点行 sr_off 内的列 sc → 终点行 er_off 内的列 ec
            let s_row_byte = row_starts[sr_off];
            let e_row_byte = row_starts[er_off];
            let b0 = row_start_byte(&plain, s_row_byte, sc);
            let b1 = row_end_byte(&plain, e_row_byte, ec);
            if b0 >= b1 {
                continue;
            }
            parts.push(map_slice_to_semantic(&plain, b0, b1, &semantic));
        } else if li == first {
            // 首行：从 sr_off 行内的列 sc 到逻辑行末尾
            let s_row_byte = row_starts[sr_off];
            let b0 = row_start_byte(&plain, s_row_byte, sc);
            parts.push(map_slice_to_semantic(&plain, b0, plain.len(), &semantic));
        } else if li == last {
            // 末行：从逻辑行开头到 er_off 行内的列 ec
            let e_row_byte = row_starts[er_off];
            let b1 = row_end_byte(&plain, e_row_byte, ec);
            parts.push(map_slice_to_semantic(&plain, 0, b1, &semantic));
        } else {
            // 中间行（完整选择）：直接用语义全文
            match &semantic {
                Some((sem, _)) => parts.push(sem.clone()),
                None => parts.push(plain),
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) fn extract_visual_range(
    slots: &[Arc<Vec<Line<'static>>>],
    slot_offsets: &[usize],
    wrap_map: &[WrappedLineInfo],
    vis_start: (usize, u16),
    vis_end: (usize, u16),
    width: u16,
    view_models: Option<&im::Vector<crate::kit::tui_render_unit::TuiRenderUnit>>,
    grid: Option<GridSpec>,
) -> Option<String> {
    let mut visual_start = 0usize;
    let local_maps = slots
        .iter()
        .enumerate()
        .map(|(slot, _)| {
            let logical_start = slot_offsets.get(slot).copied().unwrap_or(0);
            let first_visual = visual_start;
            let entries: Vec<_> = wrap_map
                .iter()
                .filter(|entry| entry.slot_index == slot)
                .map(|entry| WrappedLineInfo {
                    logical_idx: entry.logical_idx.saturating_sub(logical_start),
                    visual_start: entry.visual_start.saturating_sub(first_visual),
                    visual_end: entry.visual_end.saturating_sub(first_visual),
                    slot_index: 0,
                })
                .collect();
            visual_start = entries.last().map_or(first_visual, |entry| {
                first_visual.saturating_add(entry.visual_end)
            });
            Arc::new(entries)
        })
        .collect();
    let index = SlotIndex::new(slots.to_vec(), local_maps);
    extract_visual_range_index(&index, vis_start, vis_end, width, view_models, grid)
}

/// [D3 §9] 把 plain 的 byte 片段 [b0..b1) 映射到语义文本上。
///
/// 语义文本是 plain 去掉前缀 chrome 的结果（连续子串）时偏移直接换算；
/// 重建行（tool header）或定位失败时回退原始片段（部分选择边界场景）。
fn map_slice_to_semantic(
    plain: &str,
    b0: usize,
    b1: usize,
    semantic: &Option<(String, usize)>,
) -> String {
    let Some((sem, p)) = semantic else {
        return plain[b0..b1].to_string();
    };
    if *p == 0 && !plain.starts_with(sem.as_str()) && !sem.is_empty() {
        // find 定位失败（semantic 非 plain 子串）——回退原始提取。
        return plain[b0..b1].to_string();
    }
    let s0 = b0.saturating_sub(*p).min(sem.len());
    let e0 = b1.saturating_sub(*p).min(sem.len());
    if s0 >= e0 {
        String::new()
    } else {
        sem[s0..e0].to_string()
    }
}

// ── 测试 ─────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "selection_test.rs"]
mod tests;
