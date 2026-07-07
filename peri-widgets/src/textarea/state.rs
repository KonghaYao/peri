/// 多行文本编辑状态。光标为字符索引（非字节偏移）。
#[derive(Clone, Default)]
pub struct TextAreaState {
    pub text: String,
    /// 字符索引（保证在字符边界上）
    pub cursor: usize,
}

impl TextAreaState {
    /// 在光标位置插入字符，返回新的光标位置
    pub fn insert_char(&mut self, ch: char) {
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        self.text.insert(byte_idx, ch);
        self.cursor += 1;
    }

    /// 在光标位置插入字符串
    pub fn insert_str(&mut self, s: &str) {
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        self.text.insert_str(byte_idx, s);
        self.cursor += s.chars().count();
    }

    /// 删除光标前的字符
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let byte_idx = Self::char_to_byte(&self.text, self.cursor);
            let prev_byte = Self::char_to_byte(&self.text, self.cursor - 1);
            self.text.drain(prev_byte..byte_idx);
            self.cursor -= 1;
        }
    }

    /// 删除光标后的字符
    pub fn delete_forward(&mut self) {
        if self.cursor < self.len() {
            let byte_idx = Self::char_to_byte(&self.text, self.cursor);
            let next_byte = Self::char_to_byte(&self.text, self.cursor + 1);
            self.text.drain(byte_idx..next_byte);
        }
    }

    /// 左移光标
    pub fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// 右移光标
    pub fn cursor_right(&mut self) {
        if self.cursor < self.len() {
            self.cursor += 1;
        }
    }

    /// 左移到上一个词边界
    pub fn cursor_word_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut new_cursor = self.cursor;
        while new_cursor > 0 {
            let prev = self.char_at(new_cursor - 1);
            if !prev.is_whitespace() {
                break;
            }
            new_cursor -= 1;
        }
        while new_cursor > 0 {
            let prev = self.char_at(new_cursor - 1);
            if prev.is_whitespace() {
                break;
            }
            new_cursor -= 1;
        }
        self.cursor = new_cursor;
    }

    /// 右移到下一个词边界
    pub fn cursor_word_right(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let mut new_cursor = self.cursor;
        while new_cursor < self.len() {
            let ch = self.char_at(new_cursor);
            if ch.is_whitespace() {
                break;
            }
            new_cursor += 1;
        }
        while new_cursor < self.len() {
            let ch = self.char_at(new_cursor);
            if !ch.is_whitespace() {
                break;
            }
            new_cursor += 1;
        }
        self.cursor = new_cursor;
    }

    /// 光标到文本首
    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// 光标到文本尾
    pub fn cursor_end(&mut self) {
        self.cursor = self.len();
    }

    /// 删除光标前一个词（Ctrl+W）
    pub fn delete_word_backward(&mut self) {
        let byte_idx = Self::char_to_byte(&self.text, self.cursor);
        // 跳过光标前的空白
        let mut pos = byte_idx;
        while pos > 0 {
            let prev = self.text[..pos].chars().last().unwrap();
            if !prev.is_whitespace() {
                break;
            }
            pos -= prev.len_utf8();
            self.cursor -= 1;
        }
        // 删除到词首
        while pos > 0 {
            let prev = self.text[..pos].chars().last().unwrap();
            if prev.is_whitespace() {
                break;
            }
            pos -= prev.len_utf8();
            self.cursor -= 1;
        }
        self.text.drain(pos..byte_idx);
    }

    /// 删除光标后的一个词（Alt+Delete）
    pub fn delete_word_forward(&mut self) {
        let start_byte = Self::char_to_byte(&self.text, self.cursor);
        let mut pos = start_byte;

        while pos < self.text.len() {
            let ch = self.text[pos..]
                .chars()
                .next()
                .expect("pos must be char boundary");
            if !ch.is_whitespace() {
                break;
            }
            pos += ch.len_utf8();
        }
        while pos < self.text.len() {
            let ch = self.text[pos..]
                .chars()
                .next()
                .expect("pos must be char boundary");
            if ch.is_whitespace() {
                break;
            }
            pos += ch.len_utf8();
        }

        if pos > start_byte {
            self.text.drain(start_byte..pos);
        }
    }

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
        let last_line = text.split('\n').last().unwrap_or("");
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
        let (line, col) = self.cursor_line_col();
        if line == 0 {
            return false;
        }
        self.cursor = Self::line_col_to_cursor(&self.text, line - 1, col);
        true
    }

    /// 下移一行，返回是否真的做了多行移动。
    pub fn cursor_line_down(&mut self) -> bool {
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

    /// 把当前字符光标转换为字节偏移。
    pub fn cursor_byte(&self) -> usize {
        Self::char_to_byte(&self.text, self.cursor)
    }

    /// 当前行首。
    pub fn cursor_line_home(&mut self) {
        let (line, _) = self.cursor_line_col();
        self.cursor = Self::line_col_to_cursor(&self.text, line, 0);
    }

    /// 当前行尾。
    pub fn cursor_line_end(&mut self) {
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

    /// 替换字符区间，并把光标放在替换文本末尾。
    pub fn replace_char_range(&mut self, start: usize, end: usize, replacement: &str) {
        let start = start.min(self.len());
        let end = end.min(self.len()).max(start);
        let start_byte = Self::char_to_byte(&self.text, start);
        let end_byte = Self::char_to_byte(&self.text, end);
        self.text.replace_range(start_byte..end_byte, replacement);
        self.cursor = start + replacement.chars().count();
    }

    /// 返回当前完整文本的引用。
    pub fn all_text(&self) -> String {
        self.text.clone()
    }

    /// 替换整个文本并把光标放到末尾（历史导航用）
    pub fn replace_all(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.chars().count();
    }

    /// 清除文本
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// 字符索引 → 字节偏移
    pub fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }

    fn char_at(&self, char_idx: usize) -> char {
        self.text
            .chars()
            .nth(char_idx)
            .expect("cursor must stay within character bounds")
    }
}
