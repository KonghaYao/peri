//! Cursor position (row, col_byte). col_byte is byte offset (CJK safety guaranteed by caller).
//!
//! # Char boundary invariant
//!
//! `col_byte` MUST always be on a UTF-8 character boundary. Methods that move the
//! cursor across lines must snap to the nearest valid boundary on the destination
//! line — see `snap_col_to_char_boundary`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPos {
    pub row: usize,
    pub col_byte: usize,
}

impl CursorPos {
    pub fn new(row: usize, col_byte: usize) -> Self {
        Self { row, col_byte }
    }

    /// Snap a column byte position to the nearest char boundary at or before it.
    ///
    /// When moving the cursor vertically (up/down), the current line's `col_byte`
    /// may fall inside a multi-byte character on the destination line. This snaps
    /// it back to the previous char boundary.
    ///
    /// Example: `col_byte=4` on "你好" (boundaries 0,3,6) → snaps to 3.
    pub fn snap_col_to_char_boundary(line: &str, col_byte: usize) -> usize {
        let pos = col_byte.min(line.len());
        if line.is_char_boundary(pos) {
            return pos;
        }
        line.char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i < pos)
            .last()
            .unwrap_or(0)
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

    /// Clamp to a valid position within lines (char-boundary safe).
    pub fn clamped(&self, lines: &[String]) -> Self {
        let row = self.row.min(lines.len().saturating_sub(1));
        let col_byte = Self::snap_col_to_char_boundary(
            lines.get(row).map(|s| s.as_str()).unwrap_or(""),
            self.col_byte,
        );
        Self { row, col_byte }
    }
}
