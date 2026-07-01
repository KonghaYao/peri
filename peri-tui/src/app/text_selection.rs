use unicode_segmentation::UnicodeSegmentation;

/// 文本选区状态
#[derive(Debug, Clone)]
pub struct TextSelection {
    /// 选区起始视觉坐标（相对于消息区域左上角）
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

    /// 结束拖拽：标记拖拽结束，selected_text 由外部计算后通过 set_selected_text 设置
    pub fn end_drag(&mut self) {
        self.dragging = false;
    }

    /// 设置提取后的选区文本
    pub fn set_selected_text(&mut self, text: Option<String>) {
        self.selected_text = text;
    }

    /// 清除选区（鼠标点击非拖拽、复制后、resize 后调用）
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
}

/// 面板文字选区状态（用于 thread_browser / agent / cron 等列表面板）
#[derive(Debug, Clone)]
pub struct PanelTextSelection {
    /// 选区起始坐标（内容空间：row 已包含 scroll offset）
    pub start: Option<(u16, u16)>, // (content_row, col)
    /// 选区结束坐标
    pub end: Option<(u16, u16)>,
    /// 是否正在拖拽中
    pub dragging: bool,
    /// 选区对应的纯文本内容
    pub selected_text: Option<String>,
}

impl Default for PanelTextSelection {
    fn default() -> Self {
        Self::new()
    }
}

impl PanelTextSelection {
    pub fn new() -> Self {
        Self {
            start: None,
            end: None,
            dragging: false,
            selected_text: None,
        }
    }

    pub fn start_drag(&mut self, row: u16, col: u16) {
        self.start = Some((row, col));
        self.end = Some((row, col));
        self.dragging = true;
        self.selected_text = None;
    }

    pub fn update_drag(&mut self, row: u16, col: u16) {
        if self.dragging {
            self.end = Some((row, col));
        }
    }

    pub fn end_drag(&mut self) {
        self.dragging = false;
    }

    pub fn set_selected_text(&mut self, text: Option<String>) {
        self.selected_text = text;
    }

    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.dragging = false;
        self.selected_text = None;
    }

    pub fn is_active(&self) -> bool {
        self.dragging || self.selected_text.is_some()
    }
}

/// 从面板纯文本行中提取选区文本（字符级精度）。
/// start/end 为内容空间坐标 (content_row, col)。
/// 自动处理 start > end 的情况。
pub fn extract_panel_text(
    start: (u16, u16),
    end: (u16, u16),
    plain_lines: &[String],
) -> Option<String> {
    let ((sr, sc), (er, ec)) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let sr = sr as usize;
    let er = er as usize;
    if sr >= plain_lines.len() {
        return None;
    }
    let er = er.min(plain_lines.len() - 1);

    let mut parts: Vec<String> = Vec::new();
    for (i, text) in plain_lines.iter().enumerate().take(er + 1).skip(sr) {
        if sr == er {
            // 同一行
            let b_start = grapheme_to_byte_idx(text, sc as usize);
            let b_end = grapheme_to_byte_idx(text, ec as usize);
            if b_start >= b_end {
                return None;
            }
            parts.push(text[b_start..b_end].to_string());
        } else if i == sr {
            // 首行：从 sc 到行尾
            let b_start = grapheme_to_byte_idx(text, sc as usize);
            parts.push(text[b_start..].to_string());
        } else if i == er {
            // 末行：从行首到 ec
            let b_end = grapheme_to_byte_idx(text, ec as usize);
            parts.push(text[..b_end].to_string());
        } else {
            // 中间行：整行
            parts.push(text.clone());
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// 在 char_widths（grapheme 级别）中，模拟 ratatui WordWrapper 的 word-break 算法，
/// 定位到第 row_in_line 个视觉行的起始 grapheme 偏移，然后在该视觉行内累积宽度到 visual_col，
/// 返回 grapheme 偏移量。
///
/// word-break 规则（与 ratatui WordWrapper 一致，trim=false）：
/// - 空白跟随前一个 word 到同一行
/// - 当 line_width + whitespace_width + word_width > usable_width 时换行
/// - 当一个 word 本身超过行宽时，在行宽处硬断
fn char_col_to_offset(
    char_widths: &[u8],
    visual_col: u16,
    row_in_line: usize,
    usable_width: u16,
) -> usize {
    let uw = usable_width as usize;
    if uw == 0 || char_widths.is_empty() {
        return 0;
    }

    let target = visual_col as usize;
    let mut current_row: usize = 0;
    let mut line_width: usize = 0;
    let mut word_width: usize = 0;
    let mut whitespace_width: usize = 0;
    let mut non_ws_previous = false;
    let mut in_target_row = row_in_line == 0;

    for (i, &w) in char_widths.iter().enumerate() {
        let w = w as usize;
        let is_whitespace = w == 0; // 零宽字符视为空白

        // 跳过比行宽还宽的 grapheme
        if w > uw {
            non_ws_previous = false;
            continue;
        }

        let word_found = non_ws_previous && is_whitespace;
        let untrimmed_overflow = line_width == 0 && word_width + whitespace_width + w > uw;

        if word_found || untrimmed_overflow {
            line_width += whitespace_width + word_width;
            whitespace_width = 0;
            word_width = 0;
        }

        let line_full = line_width >= uw;
        let pending_overflow = w > 0 && line_width + whitespace_width + word_width >= uw;

        if line_full || pending_overflow {
            if in_target_row {
                break;
            }
            current_row += 1;
            line_width = 0;
            whitespace_width = 0;
            if current_row == row_in_line {
                in_target_row = true;
            }
            if is_whitespace {
                continue;
            }
        }

        if is_whitespace {
            whitespace_width += w;
        } else {
            word_width += w;
        }

        if in_target_row {
            let effective = line_width + whitespace_width + word_width;
            if effective > target {
                return i;
            }
        }

        non_ws_previous = !is_whitespace;
    }

    char_widths.len()
}

/// 将视觉坐标 (visual_row, visual_col) 通过 wrap_map 映射为 (line_idx, grapheme_offset)。
/// `usable_width` 为消息区域可用宽度（text_area.width）。
pub fn visual_to_logical(
    visual_row: u16,
    visual_col: u16,
    wrap_map: &[crate::app::message_state::WrappedLineInfo],
    usable_width: u16,
) -> Option<(usize, usize)> {
    let idx = wrap_map.partition_point(|info| info.visual_row_end <= visual_row);
    if idx >= wrap_map.len() {
        return None;
    }
    let info = &wrap_map[idx];
    if visual_row < info.visual_row_start {
        return None;
    }
    let row_in_line = (visual_row - info.visual_row_start) as usize;
    let char_offset = char_col_to_offset(&info.char_widths, visual_col, row_in_line, usable_width);
    Some((info.line_idx, char_offset))
}

/// 将 grapheme 索引转换为字节索引，用于安全切割 String。
/// `grapheme_idx` 是 text 中的 grapheme 位置（从 0 开始）。
/// 返回对应的 byte 偏移量。如果 grapheme_idx 超出 grapheme 数，返回 text.len()。
fn grapheme_to_byte_idx(text: &str, grapheme_idx: usize) -> usize {
    text.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// 根据选区起止坐标从 wrap_map 的 plain_text 提取文本（字符级精度）。
/// 自动处理 start > end 的情况（swap）。
/// 首行从 start_col 对应的字符位置截取，末行到 end_col 对应的字符位置截取，中间行整行。
/// 所有 char offset 通过 grapheme_to_byte_idx 转为 byte 索引后切割，保证 unicode 安全。
pub fn extract_selected_text(
    start: (u16, u16),
    end: (u16, u16),
    wrap_map: &[crate::app::message_state::WrappedLineInfo],
    usable_width: u16,
) -> Option<String> {
    let ((start_row, start_col), (end_row, end_col)) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };

    let start_idx = wrap_map.partition_point(|info| info.visual_row_end <= start_row);
    let end_idx = wrap_map.partition_point(|info| info.visual_row_end <= end_row);

    if start_idx >= wrap_map.len() {
        return None;
    }
    let end_idx = end_idx.min(wrap_map.len() - 1);

    let mut parts: Vec<String> = Vec::new();

    for (i, info) in wrap_map
        .iter()
        .enumerate()
        .take(end_idx + 1)
        .skip(start_idx)
    {
        let text = &info.plain_text;

        if start_idx == end_idx {
            // 同一逻辑行：截取 [start_char, end_char)
            let row_in_start = (start_row - info.visual_row_start) as usize;
            let row_in_end = (end_row - info.visual_row_start) as usize;
            let c_start =
                char_col_to_offset(&info.char_widths, start_col, row_in_start, usable_width);
            let c_end = char_col_to_offset(&info.char_widths, end_col, row_in_end, usable_width);
            let b_start = grapheme_to_byte_idx(text, c_start);
            let b_end = grapheme_to_byte_idx(text, c_end);
            if b_start >= b_end {
                return None;
            }
            parts.push(text[b_start..b_end].to_string());
        } else if i == start_idx {
            // 首行：从 start_col 对应的字符位置到行尾
            let row_in_line = (start_row - info.visual_row_start) as usize;
            let c_start =
                char_col_to_offset(&info.char_widths, start_col, row_in_line, usable_width);
            let b_start = grapheme_to_byte_idx(text, c_start);
            parts.push(text[b_start..].to_string());
        } else if i == end_idx {
            // 末行：从行首到 end_col 对应的字符位置
            let row_in_line = (end_row - info.visual_row_start) as usize;
            let c_end = char_col_to_offset(&info.char_widths, end_col, row_in_line, usable_width);
            let b_end = grapheme_to_byte_idx(text, c_end);
            parts.push(text[..b_end].to_string());
        } else {
            // 中间行：整行
            parts.push(text.to_string());
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
