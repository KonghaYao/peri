use std::borrow::Cow;
use std::collections::HashSet;
use std::mem::size_of;
use std::sync::Arc;

use pulldown_cmark_012::Alignment;
use ratatui::text::{Line, Span};
use ratatui_kit_markdown::ParsedBlock;

use super::{ImageSegment, MarkdownRenderCache, MarkdownSegment, MermaidFence, TableData};

pub(super) fn cache_retained_bytes(cache: &MarkdownRenderCache) -> usize {
    let MarkdownRenderCache {
        chunk_source,
        chunk_width: _,
        chunk_palette: _,
        chunk_base_fg: _,
        chunk_theme: _,
        stable_source_end: _,
        stable_chunk_blocks,
        stable_chunk_fences,
        stable_chunks,
        #[cfg(test)]
            stable_text: _,
        #[cfg(test)]
            stable_width: _,
        #[cfg(test)]
            stable_palette: _,
        #[cfg(test)]
            stable_state: _,
    } = cache;
    let mut bytes = chunk_source
        .heap_bytes()
        .saturating_add(stable_chunk_blocks.heap_bytes())
        .saturating_add(stable_chunk_fences.heap_bytes())
        .saturating_add(vec_storage_bytes(stable_chunks));
    let mut seen = HashSet::new();
    for chunk in stable_chunks {
        if seen.insert(Arc::as_ptr(chunk)) {
            bytes = bytes
                .saturating_add(size_of::<Vec<MarkdownSegment>>())
                .saturating_add(size_of::<usize>().saturating_mul(2))
                .saturating_add(chunk.as_ref().heap_bytes());
        }
    }
    bytes
}

// The owned container reference preserves allocation capacity for retained-memory accounting.
fn vec_storage_bytes<Value>(values: &Vec<Value>) -> usize {
    values.capacity().saturating_mul(size_of::<Value>())
}

trait RetainedHeap {
    fn heap_bytes(&self) -> usize;
}

impl<Value: RetainedHeap> RetainedHeap for Vec<Value> {
    fn heap_bytes(&self) -> usize {
        self.iter().fold(vec_storage_bytes(self), |bytes, value| {
            bytes.saturating_add(value.heap_bytes())
        })
    }
}

impl RetainedHeap for String {
    fn heap_bytes(&self) -> usize {
        self.capacity()
    }
}

impl RetainedHeap for Cow<'_, str> {
    fn heap_bytes(&self) -> usize {
        match self {
            Cow::Owned(text) => text.capacity(),
            Cow::Borrowed(_) => 0,
        }
    }
}

impl RetainedHeap for Span<'_> {
    fn heap_bytes(&self) -> usize {
        self.content.heap_bytes()
    }
}

impl RetainedHeap for Line<'_> {
    fn heap_bytes(&self) -> usize {
        self.spans.heap_bytes()
    }
}

impl RetainedHeap for ParsedBlock {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Heading(_, line) => line.heap_bytes(),
            Self::Paragraph(lines) => lines.heap_bytes(),
            Self::CodeBlock(language, lines) => {
                language.heap_bytes().saturating_add(lines.heap_bytes())
            }
            Self::ListItem(item) => item.spans.heap_bytes(),
            Self::Table(headers, rows, alignments) => headers
                .heap_bytes()
                .saturating_add(rows.heap_bytes())
                .saturating_add(vec_storage_bytes::<Alignment>(alignments)),
            Self::Rule => 0,
        }
    }
}

impl RetainedHeap for MarkdownSegment {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Text(lines) => lines.heap_bytes(),
            Self::Table(table) => table.heap_bytes(),
            Self::Image(image) => image.heap_bytes(),
            Self::GenUi(segment) => segment
                .source
                .capacity()
                .saturating_add(size_of::<peri_genui::ParseResult>() + 2 * size_of::<usize>())
                .saturating_add(segment.parsed.retained_bytes())
                .saturating_add(segment.buttons.capacity() * size_of::<crate::genui::GenUiButton>())
                .saturating_add(
                    segment
                        .buttons
                        .iter()
                        .map(|button| {
                            button.label.capacity()
                                + match &button.action {
                                    Ok(action) => action.retained_bytes(),
                                    Err(error) => error.capacity(),
                                }
                        })
                        .sum::<usize>(),
                )
                .saturating_add(segment.errors.heap_bytes())
                .saturating_add(segment.root.as_ref().map_or(0, json_heap_bytes)),
            Self::Mermaid(segment) => segment
                .fence
                .heap_bytes()
                .saturating_add(segment.lines.heap_bytes()),
        }
    }
}

impl RetainedHeap for MermaidFence {
    fn heap_bytes(&self) -> usize {
        self.language
            .capacity()
            .saturating_add(self.info.capacity())
            .saturating_add(self.opening_line.capacity())
            .saturating_add(self.raw.capacity())
            .saturating_add(self.source.capacity())
    }
}

impl RetainedHeap for Option<MermaidFence> {
    fn heap_bytes(&self) -> usize {
        self.as_ref().map_or(0, RetainedHeap::heap_bytes)
    }
}

fn json_heap_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(text) => text.capacity(),
        serde_json::Value::Array(values) => values
            .iter()
            .fold(vec_storage_bytes(values), |bytes, value| {
                bytes.saturating_add(json_heap_bytes(value))
            }),
        serde_json::Value::Object(values) => values.iter().fold(
            values
                .len()
                .saturating_mul(size_of::<(String, serde_json::Value)>() + 3 * size_of::<usize>()),
            |bytes, (key, value)| {
                bytes
                    .saturating_add(key.capacity())
                    .saturating_add(json_heap_bytes(value))
            },
        ),
        _ => 0,
    }
}

impl RetainedHeap for TableData {
    fn heap_bytes(&self) -> usize {
        let Self {
            headers,
            rows,
            alignments,
            col_widths,
        } = self;
        headers
            .heap_bytes()
            .saturating_add(rows.heap_bytes())
            .saturating_add(vec_storage_bytes(alignments))
            .saturating_add(vec_storage_bytes(col_widths))
    }
}

impl RetainedHeap for ImageSegment {
    fn heap_bytes(&self) -> usize {
        let Self {
            alt,
            url,
            title,
            is_remote: _,
            standalone: _,
            byte_start: _,
            byte_end: _,
            lines,
            sources,
        } = self;
        alt.heap_bytes()
            .saturating_add(url.heap_bytes())
            .saturating_add(title.as_ref().map_or(0, String::capacity))
            .saturating_add(lines.heap_bytes())
            .saturating_add(
                sources
                    .iter()
                    .fold(vec_storage_bytes(sources), |bytes, source| {
                        bytes.saturating_add(source.source.heap_bytes())
                    }),
            )
    }
}
