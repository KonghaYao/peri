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

#[test]
fn replacing_same_length_source_discards_stable_chunks() {
    let mut cache = MarkdownRenderCache::default();
    let first = parse_markdown_chunks_cached(
        "old paragraph\n\ntail",
        80,
        Palette::default(),
        Color::White,
        &mut cache,
    );
    assert!(!first.stable.is_empty());
    let replacement = "new paragraph\n\ntail";
    let replaced = parse_markdown_chunks_cached(
        replacement,
        80,
        Palette::default(),
        Color::White,
        &mut cache,
    );
    assert_ne!(first.stable_identities(), replaced.stable_identities());
    let cold = parse_markdown_chunks_cached(
        replacement,
        80,
        Palette::default(),
        Color::White,
        &mut MarkdownRenderCache::default(),
    );
    assert_eq!(
        replaced.segments().cloned().collect::<Vec<_>>(),
        cold.segments().cloned().collect::<Vec<_>>()
    );
}

#[test]
fn shortening_source_discards_removed_chunks() {
    let mut cache = MarkdownRenderCache::default();
    let first = parse_markdown_chunks_cached(
        "first\n\nsecond\n\ntail",
        80,
        Palette::default(),
        Color::White,
        &mut cache,
    );
    assert!(!first.stable.is_empty());
    let shortened =
        parse_markdown_chunks_cached("first", 80, Palette::default(), Color::White, &mut cache);
    assert!(shortened.stable.is_empty());
    assert_eq!(
        shortened.tail,
        parse_markdown("first", 80, Palette::default(), Color::White)
    );
    assert_eq!(cache.stable_source_end, 0);
}

#[test]
fn changing_body_foreground_rematerializes_stable_chunks() {
    let input = "stable paragraph\n\nmutable";
    let mut cache = MarkdownRenderCache::default();
    let first =
        parse_markdown_chunks_cached(input, 80, Palette::default(), Color::White, &mut cache);
    assert!(!first.stable.is_empty());
    let recolored =
        parse_markdown_chunks_cached(input, 80, Palette::default(), Color::Red, &mut cache);
    assert_ne!(first.stable_identities(), recolored.stable_identities());
    let cold = parse_markdown_chunks_cached(
        input,
        80,
        Palette::default(),
        Color::Red,
        &mut MarkdownRenderCache::default(),
    );
    assert_eq!(
        recolored.segments().cloned().collect::<Vec<_>>(),
        cold.segments().cloned().collect::<Vec<_>>()
    );
    assert_ne!(
        first.segments().cloned().collect::<Vec<_>>(),
        recolored.segments().cloned().collect::<Vec<_>>()
    );
}
