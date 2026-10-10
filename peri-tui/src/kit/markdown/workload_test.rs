use super::*;

fn assert_streaming_matches_terminal(source: &str) {
    let palette = Palette::default();
    let mut cache = MarkdownRenderCache::default();
    IMAGE_SCANNED_BYTES.with(|count| count.set(0));
    for end in source
        .match_indices('\n')
        .map(|(offset, _)| offset + 1)
        .step_by(128)
        .chain(std::iter::once(source.len()))
    {
        let input = &source[..end];
        let streaming = parse_markdown_chunks_cached(input, 80, palette, Color::Reset, &mut cache);
        let reference = parse_markdown(input, 80, palette, Color::Reset);
        assert_eq!(streaming.segments().cloned().collect::<Vec<_>>(), reference);
    }
    let terminal = parse_markdown_terminal(source, 80, palette, Color::Reset, &mut cache);
    assert_eq!(
        terminal.tail,
        parse_markdown(source, 80, palette, Color::Reset)
    );
    assert!(cache.chunk_source.is_empty());
    IMAGE_SCANNED_BYTES.with(|count| assert_eq!(count.get(), 0));
}

#[test]
fn lat09_large_list_tail_skips_image_scan_and_matches_terminal() {
    let source = "- 工作项 **明确边界** 与 `代码`\n".repeat(1024);
    assert_streaming_matches_terminal(&source);
}

#[test]
fn lat09_large_table_tail_skips_image_scan_and_matches_terminal() {
    let source = format!(
        "| 名称 | 状态 |\n| --- | --- |\n{}",
        "| 工作项 | 完成 |\n".repeat(1024)
    );
    assert_streaming_matches_terminal(&source);
}

#[test]
fn lat09_unclosed_fence_tail_skips_image_scan_and_matches_terminal() {
    let source = format!("```rust\n{}", "let value = 42;\n".repeat(1024));
    assert_streaming_matches_terminal(&source);
}

#[test]
fn lat09_long_code_line_preserves_streaming_and_terminal_text() {
    let source = format!("```rust\n{}", "let 名称 = 42;".repeat(4096));
    assert_streaming_matches_terminal(&source);
}

#[test]
fn lat09_image_free_preprocessing_borrows_original_source() {
    for source in [
        "- list\n".repeat(1024),
        "| table | cell |\n".repeat(1024),
        format!("```rust\n{}", "fn main() {}\n".repeat(1024)),
    ] {
        IMAGE_SCANNED_BYTES.with(|count| count.set(0));
        let (placeholder, images) = preprocess_images(&source);
        assert!(matches!(placeholder, Cow::Borrowed(_)));
        assert_eq!(placeholder.as_ptr(), source.as_ptr());
        assert!(images.is_empty());
        IMAGE_SCANNED_BYTES.with(|count| assert_eq!(count.get(), 0));
    }
}

#[test]
fn lat09_image_candidates_keep_reference_and_code_fence_semantics() {
    for source in [
        "before ![图片](https://example.com/a.png) after",
        "![图片][ref]\n\n[ref]: https://example.com/a.png",
        "\\![escaped](https://example.com/a.png)",
        "```text\n![code](https://example.com/a.png)\n```",
        "unfinished ![alt](",
    ] {
        let sanitized = ensure_closed_code_fences(source);
        let (expected_source, expected_images) =
            scan::replace_images(&sanitized, &scan::scan_images(&sanitized));
        let (placeholder, images) = preprocess_images(&sanitized);
        assert_eq!(placeholder, expected_source);
        assert_eq!(images, expected_images);
        if images.is_empty() {
            assert!(matches!(placeholder, Cow::Borrowed(_)));
        } else {
            assert!(matches!(placeholder, Cow::Owned(_)));
        }
    }
}
