use std::mem::size_of;

use ratatui::text::{Line, Span};

use super::*;

fn cache_for_source(source: &str) -> MarkdownRenderCache {
    let (segments, blocks) = parse_markdown_piece(source, 80, Palette::default(), Color::Reset);
    MarkdownRenderCache {
        chunk_source: source.to_owned(),
        stable_chunk_blocks: vec![blocks],
        stable_chunks: vec![Arc::new(segments)],
        ..Default::default()
    }
}

#[test]
fn empty_and_reset_cache_have_no_retained_heap() {
    let mut cache = cache_for_source(&"paragraph\n\n".repeat(128));
    assert!(cache.retained_bytes() > 0);
    cache = MarkdownRenderCache::default();
    assert_eq!(cache.retained_bytes(), 0);
}

#[test]
fn duplicate_arc_is_charged_once_per_cache() {
    let chunk = Arc::new(vec![MarkdownSegment::Text(vec![Line::from(
        "payload".repeat(4096),
    )])]);
    let mut chunks = Vec::with_capacity(2);
    chunks.push(Arc::clone(&chunk));
    let mut cache = MarkdownRenderCache {
        stable_chunks: chunks,
        ..Default::default()
    };
    let once = cache.retained_bytes();
    cache.stable_chunks.push(Arc::clone(&chunk));
    assert_eq!(cache.retained_bytes(), once);
}

#[test]
fn equal_but_distinct_arcs_are_charged_separately() {
    let chunk = vec![MarkdownSegment::Text(vec![Line::from(
        "payload".repeat(4096),
    )])];
    let mut cache = MarkdownRenderCache {
        stable_chunks: Vec::with_capacity(2),
        ..Default::default()
    };
    cache.stable_chunks.push(Arc::new(chunk.clone()));
    let once = cache.retained_bytes();
    cache.stable_chunks.push(Arc::new(chunk));
    assert!(cache.retained_bytes() > once + 4096);
}

#[test]
fn clear_releases_payload_but_still_charges_spare_capacity() {
    let mut cache = cache_for_source(&"paragraph\n\n".repeat(128));
    let before = cache.retained_bytes();
    cache.chunk_source.clear();
    cache.stable_chunk_blocks.clear();
    cache.stable_chunks.clear();
    let remaining = cache.chunk_source.capacity()
        + cache.stable_chunk_blocks.capacity() * size_of::<Vec<ParsedBlock>>()
        + cache.stable_chunks.capacity() * size_of::<Arc<Vec<MarkdownSegment>>>();
    assert_eq!(cache.retained_bytes(), remaining);
    assert!(remaining < before);
}

#[test]
fn owned_cow_capacity_is_charged_but_borrowed_text_is_not() {
    let mut owned = String::with_capacity(8192);
    owned.push_str("short");
    let owned_capacity = owned.capacity();
    let mut spans = Vec::with_capacity(2);
    spans.push(Span::raw("borrowed"));
    let mut cache = MarkdownRenderCache {
        stable_chunk_blocks: vec![vec![ParsedBlock::Paragraph(vec![Line::from(spans)])]],
        ..Default::default()
    };
    let before = cache.retained_bytes();
    let ParsedBlock::Paragraph(lines) = &mut cache.stable_chunk_blocks[0][0] else {
        panic!("expected paragraph");
    };
    lines[0].spans.push(Span::raw(owned));
    assert_eq!(cache.retained_bytes() - before, owned_capacity);
}

#[test]
fn legacy_test_only_state_is_excluded_from_production_budget() {
    let mut cache = MarkdownRenderCache {
        stable_text: "legacy".repeat(4096),
        ..Default::default()
    };
    cache.stable_state.current_text = vec![Line::from("legacy".repeat(4096))];
    cache.stable_state.segments = vec![MarkdownSegment::Text(vec![Line::from(
        "legacy".repeat(4096),
    )])];
    cache.stable_state.block_line_ends = vec![42; 4096];
    assert_eq!(cache.retained_bytes(), 0);
}

#[test]
fn long_code_retains_parser_and_render_payloads() {
    let small = cache_for_source("```text\nshort\n```\n");
    let large = cache_for_source(&format!("```text\n{}\n```\n", "code line\n".repeat(512)));
    assert!(large.retained_bytes() > small.retained_bytes() + 4096);
    assert!(large.retained_bytes() > large.chunk_source.capacity());
}

#[test]
fn long_table_retains_nested_cells_and_render_payloads() {
    let small = cache_for_source("| h |\n| - |\n| short |\n");
    let large = cache_for_source(&format!(
        "| h |\n| - |\n{}",
        "| cell content |\n".repeat(512)
    ));
    assert!(large.retained_bytes() > small.retained_bytes() + 4096);
    assert!(large.retained_bytes() > large.chunk_source.capacity());
}

#[test]
fn table_spare_capacity_at_every_level_is_charged() {
    let mut cell_text = String::with_capacity(4096);
    cell_text.push_str("cell");
    let text_bytes = cell_text.capacity();
    let mut cell = Vec::with_capacity(3);
    cell.push(Span::raw(cell_text));
    let cell_bytes = cell.capacity() * size_of::<Span<'static>>() + text_bytes;
    let mut row = Vec::with_capacity(5);
    row.push(cell);
    let row_bytes = row.capacity() * size_of::<Vec<Span<'static>>>() + cell_bytes;
    let mut rows = Vec::with_capacity(7);
    rows.push(row);
    let rows_bytes = rows.capacity() * size_of::<Vec<Vec<Span<'static>>>>() + row_bytes;
    let table = TableData {
        headers: Vec::with_capacity(11),
        rows,
        alignments: Vec::with_capacity(13),
        col_widths: Vec::with_capacity(17),
    };
    let table_bytes = rows_bytes
        + table.headers.capacity() * size_of::<Vec<Span<'static>>>()
        + table.alignments.capacity() * size_of::<pulldown_cmark_012::Alignment>()
        + table.col_widths.capacity() * size_of::<usize>();
    let segments = vec![MarkdownSegment::Table(table)];
    let segment_bytes = segments.capacity() * size_of::<MarkdownSegment>() + table_bytes;
    let cache = MarkdownRenderCache {
        stable_chunks: vec![Arc::new(segments)],
        ..Default::default()
    };
    let expected = cache.stable_chunks.capacity() * size_of::<Arc<Vec<MarkdownSegment>>>()
        + size_of::<Vec<MarkdownSegment>>()
        + 2 * size_of::<usize>()
        + segment_bytes;
    assert_eq!(cache.retained_bytes(), expected);
}

#[test]
fn long_list_retains_parser_spans_and_render_lines() {
    let small = cache_for_source("- short\n");
    let large = cache_for_source(&"- list item content\n".repeat(512));
    assert!(large.retained_bytes() > small.retained_bytes() + 4096);
    assert!(large.retained_bytes() > large.chunk_source.capacity());
}

#[test]
fn image_fields_title_and_fallback_lines_are_charged() {
    let mut cache = cache_for_source("![alt](https://example.com/image.png \"title\")\n");
    let before = cache.retained_bytes();
    let segments = Arc::make_mut(&mut cache.stable_chunks[0]);
    let image = segments
        .iter_mut()
        .find_map(|segment| match segment {
            MarkdownSegment::Image(image) => Some(image),
            _ => None,
        })
        .expect("image segment");
    let previous = image.alt.capacity()
        + image.url.capacity()
        + image.title.as_ref().map_or(0, String::capacity);
    image.alt = String::with_capacity(4096);
    image.url = String::with_capacity(8192);
    image.title = Some(String::with_capacity(16384));
    let replacement = image.alt.capacity()
        + image.url.capacity()
        + image.title.as_ref().map_or(0, String::capacity);
    assert_eq!(cache.retained_bytes(), before - previous + replacement);
}

#[test]
fn image_fallback_line_owned_text_capacity_is_charged() {
    let mut cache = cache_for_source("![alt](https://example.com/image.png)\n");
    let before = cache.retained_bytes();
    let segments = Arc::make_mut(&mut cache.stable_chunks[0]);
    let image = segments
        .iter_mut()
        .find_map(|segment| match segment {
            MarkdownSegment::Image(image) => Some(image),
            _ => None,
        })
        .expect("image segment");
    let span = &mut image.lines[0].spans[0];
    let previous = match &span.content {
        Cow::Owned(text) => text.capacity(),
        Cow::Borrowed(_) => 0,
    };
    let replacement = String::with_capacity(8192);
    let replacement_bytes = replacement.capacity();
    span.content = Cow::Owned(replacement);
    assert_eq!(
        cache.retained_bytes(),
        before - previous + replacement_bytes
    );
}
