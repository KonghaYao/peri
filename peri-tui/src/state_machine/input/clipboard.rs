//! Clipboard operations — copy / cut / paste through the system clipboard.
//!
//! Uses `arboard` for cross-platform clipboard access. If clipboard is unavailable
//! (e.g., headless environment), operations silently no-op.

use super::InputState;

/// Clipboard operations on the input buffer.
pub trait InputClipboard {
    /// Copy current selection (if any) to the system clipboard.
    fn copy(&self);

    /// Cut current selection — copy to clipboard + delete from buffer.
    fn cut(&mut self);

    /// Paste clipboard content at cursor position.
    fn paste(&mut self);
}

impl InputClipboard for InputState {
    fn copy(&self) {
        let text = match &self.selection {
            Some(sel) => {
                let range = sel.range();
                if range.start_row == range.end_row {
                    self.lines[range.start_row][range.start_col..range.end_col].to_string()
                } else {
                    let mut parts =
                        vec![self.lines[range.start_row][range.start_col..].to_string()];
                    for r in (range.start_row + 1)..range.end_row {
                        parts.push(self.lines[r].clone());
                    }
                    parts.push(self.lines[range.end_row][..range.end_col].to_string());
                    parts.join("\n")
                }
            }
            None => return,
        };

        let _ = try_set_clipboard(&text);
    }

    fn cut(&mut self) {
        self.copy();
        self.delete_selection();
    }

    fn paste(&mut self) {
        let Ok(text) = try_get_clipboard() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.delete_selection();
        self.insert_str(&text);
    }
}

impl InputState {
    /// Delete the current selection from the buffer.
    fn delete_selection(&mut self) {
        let Some(sel) = self.selection.take() else {
            return;
        };
        if sel.is_empty() {
            return;
        }
        let range = sel.range();

        if range.start_row == range.end_row {
            self.lines[range.start_row].replace_range(range.start_col..range.end_col, "");
            self.cursor = super::CursorPos::new(range.start_row, range.start_col);
            return;
        }

        // Multi-line deletion.
        self.lines[range.start_row].truncate(range.start_col);
        self.lines[range.end_row].replace_range(..range.end_col, "");
        let merged = format!(
            "{}{}",
            self.lines[range.start_row], self.lines[range.end_row]
        );
        self.lines[range.start_row] = merged;
        // Remove the middle lines + end line.
        for _ in 0..(range.end_row - range.start_row) {
            self.lines.remove(range.start_row + 1);
        }
        self.cursor = super::CursorPos::new(range.start_row, range.start_col);
    }
}

// ---------------------------------------------------------------------------
// Clipboard backend
// ---------------------------------------------------------------------------

/// Try to read text from the system clipboard.
fn try_get_clipboard() -> Result<String, ()> {
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => clipboard.get_text().map_err(|_| ()),
        Err(_) => Err(()),
    }
}

/// Try to write text to the system clipboard.
fn try_set_clipboard(text: &str) -> Result<(), ()> {
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => clipboard.set_text(text).map_err(|_| ()),
        Err(_) => Err(()),
    }
}
