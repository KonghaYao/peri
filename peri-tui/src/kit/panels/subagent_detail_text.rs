//! Sparse row index for a bounded, read-only history page or body chunk.
use std::ops::Range;
use std::sync::Arc;

use ratatui_kit::ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const CHECKPOINT_ROWS: usize = 64;

pub(super) struct TextLayout {
    text: Arc<str>,
    width: usize,
    height: usize,
    checkpoints: Vec<usize>,
}

impl TextLayout {
    pub(super) fn new(text: Arc<str>, width: usize) -> Self {
        let width = width.max(2);
        let mut height = 0usize;
        let mut checkpoints = Vec::new();
        for range in Rows::new(&text, width, 0) {
            if height.is_multiple_of(CHECKPOINT_ROWS) {
                checkpoints.push(range.start);
            }
            height += 1;
        }
        Self {
            text,
            width,
            height,
            checkpoints,
        }
    }

    pub(super) fn matches(&self, text: &Arc<str>, width: usize) -> bool {
        Arc::ptr_eq(&self.text, text) && self.width == width.max(2)
    }

    pub(super) fn height(&self) -> usize {
        self.height
    }

    pub(super) fn lines(&self, rows: Range<usize>) -> Vec<Line<'static>> {
        let top = rows.start.min(self.height);
        let count = rows.end.min(self.height).saturating_sub(top);
        if count == 0 {
            return Vec::new();
        }
        let checkpoint = top / CHECKPOINT_ROWS;
        Rows::new(&self.text, self.width, self.checkpoints[checkpoint])
            .skip(top % CHECKPOINT_ROWS)
            .take(count)
            .map(|range| {
                Line::from(
                    self.text[range]
                        .chars()
                        .filter(|ch| !ch.is_control())
                        .collect::<String>(),
                )
            })
            .collect()
    }
}

struct Rows<'a> {
    text: &'a str,
    width: usize,
    next: Option<usize>,
}

impl<'a> Rows<'a> {
    fn new(text: &'a str, width: usize, start: usize) -> Self {
        Self {
            text,
            width,
            next: Some(start),
        }
    }
}

impl Iterator for Rows<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let start = self.next.take()?;
        let mut columns = 0usize;
        for (relative, grapheme) in self.text[start..].grapheme_indices(true) {
            let offset = start + relative;
            if grapheme.ends_with('\n') {
                self.next = Some(offset + grapheme.len());
                return Some(start..offset);
            }
            let width = grapheme.width();
            if columns.saturating_add(width) > self.width && columns != 0 {
                self.next = Some(offset);
                return Some(start..offset);
            }
            columns = columns.saturating_add(width);
        }
        Some(start..self.text.len())
    }
}

#[cfg(test)]
#[path = "subagent_detail_text_test.rs"]
mod tests;
