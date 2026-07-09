//! 消息区文本拖拽选中——鼠标拖拽选中文本并自动复制到剪贴板。
//!
//! 移植自旧 v1 架构 `peri-tui/src/app/text_selection.rs`，简化为仅消息区用。
//!
//! 注意：迁移 ratatui-kit-markdown 后，RENDER_CACHE 已删除，文本选区功能暂时失效。
//! 保留代码待后续基于 Markdown 组件重新实现选区逻辑。

#![allow(dead_code)]
//! 新 kit 架构无 wrap_map，直接基于已换行的 `Line<'static>` 做字符级文本提取。

use ratatui::text::{Line, Span};

/// 文本选区状态（消息区用）
#[derive(Debug, Clone)]
pub struct TextSelection {
    /// 选区起始视觉坐标（相对于消息内容左上角，已含 scroll offset）
    pub start: Option<(u16, u16)>, // (visual_row, visual_col)
    /// 选区结束视觉坐标
    pub end: Option<(u16, u16)>,
    /// 是否正在拖拽中
    pub dragging: bool,
    /// 选区对应的纯文本内容（松开鼠标后计算）
    pub selected_text: Option<String>,
}

impl Default for TextSelection {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSelection {
    pub fn new() -> Self {
        Self {
            start: None,
            end: None,
            dragging: false,
            selected_text: None,
        }
    }

    /// 开始拖拽：记录起始坐标，清除旧选区
    pub fn start_drag(&mut self, row: u16, col: u16) {
        self.start = Some((row, col));
        self.end = Some((row, col));
        self.dragging = true;
        self.selected_text = None;
    }

    /// 更新拖拽：更新结束坐标
    pub fn update_drag(&mut self, row: u16, col: u16) {
        if self.dragging {
            self.end = Some((row, col));
        }
    }

    /// 结束拖拽：标记拖拽结束
    pub fn end_drag(&mut self) {
        self.dragging = false;
    }

    /// 设置提取后的选区文本
    pub fn set_selected_text(&mut self, text: Option<String>) {
        self.selected_text = text;
    }

    /// 清除选区
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.dragging = false;
        self.selected_text = None;
    }

    /// 是否有活跃的选区（正在拖拽或已选中文字）
    pub fn is_active(&self) -> bool {
        self.dragging || self.selected_text.is_some()
    }

    /// 返回规范化的选区范围（start ≤ end）
    pub fn normalized_bounds(&self) -> Option<((u16, u16), (u16, u16))> {
        let start = self.start?;
        let end = self.end?;
        if start <= end {
            Some((start, end))
        } else {
            Some((end, start))
        }
    }
}

// ── 文本提取（基于已换行的 Line<'static>）─────────────────────────────────

/// 将 Line 的所有 span 内容拼接为纯文本。
pub fn line_to_plain_text(line: &Line) -> String {
    line.spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<&str>>()
        .concat()
}

/// 在纯文本 text 中，将 visual_col 转换为 byte 偏移量。
/// visual_col 是 Unicode 显示宽度列号。CJK 字符占 2 列。
pub fn visual_col_to_byte_offset(text: &str, target_col: u16) -> usize {
    let mut col = 0u16;
    for (i, ch) in text.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as u16;
        if col + cw > target_col {
            return i;
        }
        col += cw;
    }
    text.len()
}

/// 从消息区已换行的 lines 中提取选区文本（字符级精度）。
///
/// start/end 为内容空间坐标 (visual_row, visual_col)，直接索引 lines。
/// 自动处理 start > end（swap）。首行从 start_col 截取，末行到 end_col 截取，中间行整行。
pub fn extract_selected_text(
    start: (u16, u16),
    end: (u16, u16),
    lines: &[Line<'static>],
) -> Option<String> {
    let ((sr, sc), (er, ec)) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };

    let sr = sr as usize;
    let er = er as usize;

    if sr >= lines.len() {
        return None;
    }
    let er = er.min(lines.len() - 1);

    let mut parts: Vec<String> = Vec::new();

    for i in sr..=er {
        let text = line_to_plain_text(&lines[i]);

        if sr == er {
            // 同一行：截取 [sc, ec)
            let b_start = visual_col_to_byte_offset(&text, sc);
            let b_end = visual_col_to_byte_offset(&text, ec);
            if b_start >= b_end {
                return None;
            }
            parts.push(text[b_start..b_end].to_string());
        } else if i == sr {
            // 首行：从 sc 到行尾
            let b_start = visual_col_to_byte_offset(&text, sc);
            parts.push(text[b_start..].to_string());
        } else if i == er {
            // 末行：从行首到 ec
            let b_end = visual_col_to_byte_offset(&text, ec);
            parts.push(text[..b_end].to_string());
        } else {
            // 中间行：整行
            parts.push(text);
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

// ── 选区高亮 ─────────────────────────────────────────────────────────────

/// 选区高亮背景色。
const SELECTION_BG: ratatui::style::Color = ratatui::style::Color::Rgb(38, 79, 120);

/// 为选区内的行做字符级高亮——首/末行仅 highlight 被选中的列范围，中间行全行。
///
/// 返回新的 `Vec<Line>`，对每个字符 span 按选区位置决定是否追加背景色。
pub fn highlight_selected_lines(
    lines: &[Line<'static>],
    start_row: u16,
    start_col: u16,
    end_row: u16,
    end_col: u16,
) -> Vec<Line<'static>> {
    let ((sr, sc), (er, ec)) = if (start_row, start_col) <= (end_row, end_col) {
        ((start_row, start_col), (end_row, end_col))
    } else {
        ((end_row, end_col), (start_row, start_col))
    };

    let sr = sr as usize;
    let er = (er as usize).min(lines.len().saturating_sub(1));

    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if i < sr || i > er {
                return line.clone();
            }
            let text = line_to_plain_text(line);
            // 计算该行被选中的字符范围 [highlight_start_byte, highlight_end_byte)
            let (h_byte_start, h_byte_end) = if sr == er {
                // 同行：仅 [sc, ec) 范围
                let b0 = visual_col_to_byte_offset(&text, sc);
                let b1 = visual_col_to_byte_offset(&text, ec);
                (b0, b1)
            } else if i == sr {
                // 首行：从 sc 到行尾
                let b0 = visual_col_to_byte_offset(&text, sc);
                (b0, text.len())
            } else if i == er {
                // 末行：从行首到 ec
                let b1 = visual_col_to_byte_offset(&text, ec);
                (0, b1)
            } else {
                // 中间行：整行
                (0, text.len())
            };
            // 按 span 重建，对落在 [h_byte_start, h_byte_end) 内的部分追加背景色
            let mut cur = 0usize;
            let spans: Vec<Span<'static>> = line
                .spans
                .iter()
                .flat_map(|s| {
                    let len = s.content.len();
                    let end = cur + len;
                    let overlap_start = cur.max(h_byte_start);
                    let overlap_end = end.min(h_byte_end);
                    cur = end;

                    if overlap_start >= overlap_end {
                        // 该 span 完全在选区外
                        vec![Span::styled(s.content.clone(), s.style)]
                    } else if overlap_start == cur.saturating_sub(len) && overlap_end == end {
                        // 该 span 完全在选区内
                        vec![Span::styled(s.content.clone(), s.style.bg(SELECTION_BG))]
                    } else {
                        // 部分重叠：拆成三段（前/中/后），只用选中的中间段加背景色
                        let mut parts: Vec<Span<'static>> = Vec::new();
                        let raw: &str = s.content.as_ref();
                        let base = cur - len;
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
                            s.style.bg(SELECTION_BG),
                        ));
                        if overlap_end < end {
                            parts.push(Span::styled(raw[mid_end..].to_string(), s.style));
                        }
                        parts
                    }
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}
