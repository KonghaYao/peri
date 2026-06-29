//! Cursor position (row, col_byte). col_byte is byte offset (CJK safety guaranteed by caller).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPos {
    pub row: usize,
    pub col_byte: usize,
}

impl CursorPos {
    pub fn new(row: usize, col_byte: usize) -> Self {
        Self { row, col_byte }
    }

    /// Derive (row, col) from a global buffer byte offset.
    pub fn from_byte_offset(lines: &[String], byte_offset: usize) -> Self {
        let mut remaining = byte_offset;
        for (row, line) in lines.iter().enumerate() {
            let line_len_with_newline = line.len() + 1; // +1 for '\n'
            if remaining <= line.len() {
                return Self {
                    row,
                    col_byte: remaining,
                };
            }
            // Skip past this line + newline.
            remaining = remaining.saturating_sub(line_len_with_newline);
        }
        // Out of bounds: position at end of last line.
        let last_row = lines.len().saturating_sub(1);
        let last_col = lines.last().map(|s| s.len()).unwrap_or(0);
        Self {
            row: last_row,
            col_byte: last_col,
        }
    }

    /// Reverse: convert (row, col) to a global buffer byte offset.
    pub fn to_byte_offset(&self, lines: &[String]) -> usize {
        let mut offset = 0;
        for (i, line) in lines.iter().enumerate() {
            if i == self.row {
                return offset + self.col_byte.min(line.len());
            }
            offset += line.len() + 1; // +1 for '\n'
        }
        offset
    }

    /// Clamp to a valid position within lines.
    pub fn clamped(&self, lines: &[String]) -> Self {
        let row = self.row.min(lines.len().saturating_sub(1));
        let col_byte = self
            .col_byte
            .min(lines.get(row).map(|s| s.len()).unwrap_or(0));
        Self { row, col_byte }
    }
}
