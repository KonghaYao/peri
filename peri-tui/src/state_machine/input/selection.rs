//! Selection range type. All coordinates are (row, col_byte).

/// Text selection. anchor is where the user pressed, cursor is the current position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub anchor_row: usize,
    pub anchor_col: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

/// Normalized range (start <= end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionRange {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

impl Selection {
    pub fn normal(
        anchor_row: usize,
        anchor_col: usize,
        cursor_row: usize,
        cursor_col: usize,
    ) -> Self {
        Self {
            anchor_row,
            anchor_col,
            cursor_row,
            cursor_col,
        }
    }

    /// Returns normalized range (start <= end).
    pub fn range(&self) -> SelectionRange {
        let (start_row, start_col, end_row, end_col) = if self.anchor_row < self.cursor_row
            || (self.anchor_row == self.cursor_row && self.anchor_col <= self.cursor_col)
        {
            (
                self.anchor_row,
                self.anchor_col,
                self.cursor_row,
                self.cursor_col,
            )
        } else {
            (
                self.cursor_row,
                self.cursor_col,
                self.anchor_row,
                self.anchor_col,
            )
        };
        SelectionRange {
            start_row,
            start_col,
            end_row,
            end_col,
        }
    }

    pub fn start(&self) -> (usize, usize) {
        let r = self.range();
        (r.start_row, r.start_col)
    }

    pub fn end(&self) -> (usize, usize) {
        let r = self.range();
        (r.end_row, r.end_col)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor_row == self.cursor_row && self.anchor_col == self.cursor_col
    }
}

impl SelectionRange {
    pub fn contains_row(&self, row: usize) -> bool {
        row >= self.start_row && row <= self.end_row
    }
}
