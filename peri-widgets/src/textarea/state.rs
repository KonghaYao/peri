use crate::textarea::history;
use crate::textarea::word;

/// Yank 缓冲——记录最近删除的文本，支持 Ctrl+Y 粘贴。
#[derive(Debug, Clone)]
pub enum YankText {
    /// 单次删除的文本
    Piece(String),
    /// 连续同类删除拼接的文本块
    Chunk(String),
}

impl YankText {
    pub fn text(&self) -> &str {
        match self {
            Self::Piece(s) | Self::Chunk(s) => s.as_str(),
        }
    }
}

/// 多行文本编辑状态。光标为字符索引（非字节偏移）。
#[derive(Clone)]
pub struct TextAreaState {
    pub text: String,
    /// 字符索引（保证在字符边界上）
    pub cursor: usize,
    /// 选区起点（与 cursor 组成区间 [min(start,cursor), max(start,cursor)])
    pub selection_start: Option<usize>,
    /// 撤销/重做历史栈
    history: history::History,
    /// 最近被拉出（yank）的文本
    pub yank: Option<YankText>,
    /// 占位符文本（内容为空时显示，空字符串=禁用）
    pub placeholder: String,
}

#[allow(clippy::derivable_impls)]
impl Default for TextAreaState {
    fn default() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            selection_start: None,
            history: history::History::default(),
            yank: None,
            placeholder: String::new(),
        }
    }
}

impl TextAreaState {
    // ── 编辑操作 ──────────────────────────────────────

    /// 在光标位置插入字符，返回新的光标位置
    pub fn insert_char(&mut self, ch: char) {
        self.delete_selection();
        let before = self.snapshot();
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        self.text.insert(byte_idx, ch);
        self.cursor += 1;
        self.yank = None;
        self.record_edit(before);
    }

    /// 在光标位置插入字符串
    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        let before = self.snapshot();
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        self.text.insert_str(byte_idx, s);
        self.cursor += s.chars().count();
        self.yank = None;
        self.record_edit(before);
    }

    /// 删除光标前的字符（行首删除换行符时合并行）。
    pub fn backspace(&mut self) {
        self.delete_selection();
        let before = self.snapshot();
        if self.cursor > 0 {
            let byte_idx = Self::char_to_byte(&self.text, self.cursor);
            let prev_byte = Self::char_to_byte(&self.text, self.cursor - 1);
            let deleted_char = self.text[prev_byte..byte_idx].to_string();
            self.text.drain(prev_byte..byte_idx);
            self.cursor -= 1;
            self.yank = Some(YankText::Piece(deleted_char));
        }
        self.record_edit(before);
    }

    /// 删除光标后的字符（行尾删除换行符时合并行）。
    pub fn delete_forward(&mut self) {
        self.delete_selection();
        let before = self.snapshot();
        if self.cursor < self.len() {
            let byte_idx = Self::char_to_byte(&self.text, self.cursor);
            let next_byte = Self::char_to_byte(&self.text, self.cursor + 1);
            let deleted_char = self.text[byte_idx..next_byte].to_string();
            self.text.drain(byte_idx..next_byte);
            self.yank = Some(YankText::Piece(deleted_char));
        }
        self.record_edit(before);
    }

    // ── 光标移动 ──────────────────────────────────────

    /// 左移光标
    pub fn cursor_left(&mut self) {
        self.cancel_selection();
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// 右移光标
    pub fn cursor_right(&mut self) {
        self.cancel_selection();
        if self.cursor < self.len() {
            self.cursor += 1;
        }
    }

    /// 左移到上一个词边界（使用 Space/Punct/Other 三分法）。
    pub fn cursor_word_left(&mut self) {
        self.cancel_selection();
        self.cursor = word::prev_word_boundary(&self.text, self.cursor);
    }

    /// 右移到下一个词边界（使用 Space/Punct/Other 三分法）。
    pub fn cursor_word_right(&mut self) {
        self.cancel_selection();
        self.cursor = word::next_word_boundary(&self.text, self.cursor);
    }

    /// 光标到文本首
    pub fn cursor_home(&mut self) {
        self.cancel_selection();
        self.cursor = 0;
    }

    /// 光标到文本尾
    pub fn cursor_end(&mut self) {
        self.cancel_selection();
        self.cursor = self.len();
    }

    // ── 词删除 ────────────────────────────────────────

    /// 删除光标前一个词（Ctrl+W）。
    /// 行首时先合并上一行（删除换行符），再继续删词。
    pub fn delete_word_backward(&mut self) {
        self.delete_selection();
        let before = self.snapshot();
        if self.cursor == 0 {
            return;
        }
        let (line, col) = self.cursor_line_col();
        if col == 0 && line > 0 {
            // 行首：先合并上一行（删除换行符）
            let byte_idx = Self::char_to_byte(&self.text, self.cursor);
            let prev_byte = Self::char_to_byte(&self.text, self.cursor - 1);
            self.text.drain(prev_byte..byte_idx);
            self.cursor -= 1;
        }
        let boundary = word::prev_word_boundary(&self.text, self.cursor);
        let start_byte = Self::char_to_byte(&self.text, boundary);
        let end_byte = Self::char_to_byte(&self.text, self.cursor);
        let deleted: String = self.text[start_byte..end_byte].to_string();
        self.text.replace_range(start_byte..end_byte, "");
        self.cursor = boundary;
        self.yank = Some(YankText::Piece(deleted));
        self.record_edit(before);
    }

    /// 删除光标后的一个词（Alt+Delete）。
    pub fn delete_word_forward(&mut self) {
        self.delete_selection();
        let before = self.snapshot();
        if self.cursor >= self.len() {
            return;
        }
        let boundary = word::next_word_boundary(&self.text, self.cursor);
        let start_byte = Self::char_to_byte(&self.text, self.cursor);
        let end_byte = Self::char_to_byte(&self.text, boundary);
        let deleted: String = self.text[start_byte..end_byte].to_string();
        self.text.replace_range(start_byte..end_byte, "");
        self.yank = Some(YankText::Piece(deleted));
        self.record_edit(before);
    }

    // ── 行导航 ────────────────────────────────────────

    /// 当前光标所在的 (line_idx, col_idx)
    pub fn cursor_line_col(&self) -> (usize, usize) {
        Self::cursor_line_col_for(&self.text, self.cursor)
    }

    pub fn cursor_line_col_for(text: &str, cursor: usize) -> (usize, usize) {
        let mut chars_before_line = 0usize;
        for (line_idx, line) in text.split('\n').enumerate() {
            let line_chars = line.chars().count();
            if chars_before_line + line_chars >= cursor {
                return (line_idx, cursor - chars_before_line);
            }
            chars_before_line += line_chars + 1;
        }
        let last_line = text.split('\n').next_back().unwrap_or("");
        (text.matches('\n').count(), last_line.chars().count())
    }

    pub fn line_col_to_cursor(text: &str, target_line: usize, target_col: usize) -> usize {
        let mut cursor = 0usize;
        for (line_idx, line) in text.split('\n').enumerate() {
            let line_chars = line.chars().count();
            if line_idx == target_line {
                return cursor + target_col.min(line_chars);
            }
            cursor += line_chars + 1;
        }
        text.chars().count()
    }

    /// 上移一行，返回是否真的做了多行移动。
    pub fn cursor_line_up(&mut self) -> bool {
        self.cancel_selection();
        let (line, col) = self.cursor_line_col();
        if line == 0 {
            return false;
        }
        self.cursor = Self::line_col_to_cursor(&self.text, line - 1, col);
        true
    }

    /// 下移一行，返回是否真的做了多行移动。
    pub fn cursor_line_down(&mut self) -> bool {
        self.cancel_selection();
        let (line, col) = self.cursor_line_col();
        let last_line = self.text.matches('\n').count();
        if line >= last_line {
            return false;
        }
        self.cursor = Self::line_col_to_cursor(&self.text, line + 1, col);
        true
    }

    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 设置占位符文本（空字符串清空）。
    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }

    /// 把当前字符光标转换为字节偏移。
    pub fn cursor_byte(&self) -> usize {
        Self::char_to_byte(&self.text, self.cursor)
    }

    /// 当前行首。
    pub fn cursor_line_home(&mut self) {
        self.cancel_selection();
        let (line, _) = self.cursor_line_col();
        self.cursor = Self::line_col_to_cursor(&self.text, line, 0);
    }

    /// 当前行尾。
    pub fn cursor_line_end(&mut self) {
        self.cancel_selection();
        let (line, _) = self.cursor_line_col();
        let line_len = self
            .text
            .split('\n')
            .nth(line)
            .unwrap_or("")
            .chars()
            .count();
        self.cursor = Self::line_col_to_cursor(&self.text, line, line_len);
    }

    // ── 文本替换 ──────────────────────────────────────

    /// 替换字符区间，并把光标放在替换文本末尾。
    pub fn replace_char_range(&mut self, start: usize, end: usize, replacement: &str) {
        self.delete_selection();
        let before = self.snapshot();
        let start = start.min(self.len());
        let end = end.min(self.len()).max(start);
        let start_byte = Self::char_to_byte(&self.text, start);
        let end_byte = Self::char_to_byte(&self.text, end);
        self.text.replace_range(start_byte..end_byte, replacement);
        self.cursor = start + replacement.chars().count();
        self.record_edit(before);
    }

    /// 返回当前完整文本的引用。
    pub fn all_text(&self) -> String {
        self.text.clone()
    }

    /// 替换整个文本并把光标放到末尾（历史导航、提交后用）。
    pub fn replace_all(&mut self, text: String) {
        let before = self.snapshot();
        self.text = text;
        self.cursor = self.text.chars().count();
        self.selection_start = None;
        self.yank = None;
        self.record_edit(before);
    }

    // ── 选区操作 ──────────────────────────────────────

    /// 选区是否激活（start != cursor）
    pub fn has_selection(&self) -> bool {
        self.selection_start.is_some_and(|s| s != self.cursor)
    }

    /// 获取规范化的选区范围 [start, end)
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let start = self.selection_start?;
        let end = self.cursor;
        if start == end {
            None
        } else if start < end {
            Some((start, end))
        } else {
            Some((end, start))
        }
    }

    /// 删除当前选区内容，返回被删除的文本。
    /// 无选区时返回 None。
    pub fn delete_selection(&mut self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let deleted = self
            .text
            .chars()
            .skip(start)
            .take(end - start)
            .collect::<String>();
        let start_byte = Self::char_to_byte(&self.text, start);
        let end_byte = Self::char_to_byte(&self.text, end);
        self.text.replace_range(start_byte..end_byte, "");
        self.cursor = start;
        self.selection_start = None;
        // 更新 yank
        self.yank = Some(YankText::Piece(deleted.clone()));
        Some(deleted)
    }

    /// 取消选区（不改变文本）。
    pub fn cancel_selection(&mut self) {
        self.selection_start = None;
    }

    /// 开始选区（记录当前光标位置为选区起点）。
    pub fn start_selection(&mut self) {
        if self.selection_start.is_none() {
            self.selection_start = Some(self.cursor);
        }
    }

    // ── 撤销/重做 ─────────────────────────────────────

    /// 撤销上一次编辑。
    pub fn undo(&mut self) -> bool {
        let Self {
            text,
            cursor,
            selection_start,
            history,
            ..
        } = self;
        history.undo(text, cursor, selection_start)
    }

    /// 重做上一次撤销。
    pub fn redo(&mut self) -> bool {
        let Self {
            text,
            cursor,
            selection_start,
            history,
            ..
        } = self;
        history.redo(text, cursor, selection_start)
    }

    /// 提交后清空 undo/redo 栈。
    pub fn clear_undo_history(&mut self) {
        self.history.clear();
    }

    // ── yank 粘贴 ─────────────────────────────────────

    /// 粘贴最近 yank 的文本。
    pub fn paste_yank(&mut self) {
        if let Some(ref yank) = self.yank.clone() {
            let text = yank.text().to_string();
            if !text.is_empty() {
                self.delete_selection();
                let before = self.snapshot();
                self.insert_str(&text);
                self.record_edit(before);
            }
        }
    }

    // ── 整体操作 ──────────────────────────────────────

    /// 替换整个文本（不记录 undo 栈——历史导航专用）。
    pub fn replace_all_no_undo(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.chars().count();
        self.selection_start = None;
        self.yank = None;
    }

    /// 清除文本（Ctrl+U）。
    pub fn clear(&mut self) {
        let before = self.snapshot();
        self.text.clear();
        self.cursor = 0;
        self.selection_start = None;
        self.yank = None;
        self.record_edit(before);
    }

    /// 取出全部文本并重置状态（提交用）。
    pub fn take_text(&mut self) -> String {
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.selection_start = None;
        self.yank = None;
        self.history.clear();
        text
    }

    // ── 内部辅助 ──────────────────────────────────────

    /// 操作前拍快照（供 record_edit 使用）。
    pub fn snapshot(&self) -> history::Snapshot {
        history::History::snapshot(self)
    }

    /// 记录一次编辑（操作后调用）。外部调用者如需手动记录编辑（如 @mention 替换），
    /// 先 snapshot→操作→record_edit。
    pub fn record_edit(&mut self, before: history::Snapshot) {
        let after = self.snapshot();
        self.history.record(before, after);
    }

    /// 字符索引 → 字节偏移
    pub fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }
}
