use super::*;

#[test]
fn terminal_cache_does_not_retain_source_or_streaming_blocks() {
    let source = "stable paragraph\n\n".repeat(256);
    let mut cache = MarkdownRenderCache::default();
    parse_markdown_chunks_cached(&source, 80, Palette::default(), Color::Reset, &mut cache);
    assert!(!cache.chunk_source.is_empty());
    let terminal =
        parse_markdown_terminal(&source, 80, Palette::default(), Color::Reset, &mut cache);
    assert!(!terminal.tail.is_empty());
    assert!(cache.chunk_source.is_empty());
    assert_eq!(cache.chunk_source.capacity(), 0);
    assert!(cache.stable_chunks.is_empty());
    assert!(cache.stable_chunk_blocks.is_empty());
}

#[test]
fn clearing_source_resets_streaming_cache() {
    let mut cache = MarkdownRenderCache::default();
    parse_markdown_chunks_cached(
        "first\n\nsecond",
        80,
        Palette::default(),
        Color::Reset,
        &mut cache,
    );
    let empty = parse_markdown_chunks_cached("", 80, Palette::default(), Color::Reset, &mut cache);
    assert!(empty.stable.is_empty());
    assert!(empty.tail.is_empty());
    assert!(cache.chunk_source.is_empty());
    assert_eq!(cache.stable_source_end, 0);
}
