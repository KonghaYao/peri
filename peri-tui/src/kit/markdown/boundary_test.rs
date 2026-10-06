use super::*;
use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};

fn semantic_segments<'a>(
    segments: impl Iterator<Item = &'a MarkdownSegment>,
) -> Vec<MarkdownSegment> {
    segments
        .flat_map(|segment| match segment {
            MarkdownSegment::Text(lines) => lines
                .iter()
                .filter(|line| !line.spans.is_empty())
                .map(|line| MarkdownSegment::Text(vec![line.clone()]))
                .collect(),
            other => vec![other.clone()],
        })
        .collect()
}

#[test]
#[serial_test::serial]
fn test_closed_fence_with_blank_lines_shares_body_after_publication() {
    let body = "let value = 42;\n\n".repeat(128);
    let prefix = format!("```rust\n{body}```\n\n");
    let mut cache = MarkdownRenderCache::default();
    let first =
        parse_markdown_chunks_cached(&prefix, 80, Palette::default(), Color::Reset, &mut cache);
    assert_eq!(cache.stable_source_end, prefix.len());
    assert_eq!(first.stable.len(), 1);
    reset_perf_counters();
    boundary::SCANNED_BYTES.with(|count| count.set(0));
    let mut source = prefix.clone();
    let mut expected_bytes = 0;
    for length in 1..=32 {
        source.push('x');
        expected_bytes += length;
        let rendered =
            parse_markdown_chunks_cached(&source, 80, Palette::default(), Color::Reset, &mut cache);
        assert!(Arc::ptr_eq(&first.stable[0], &rendered.stable[0]));
    }
    let counters = perf_counters();
    assert_eq!(counters.full_parses, 0);
    assert_eq!(counters.tail_parses, 32);
    assert_eq!(counters.tail_parsed_bytes, expected_bytes);
    assert_eq!(counters.materialized_lines, 32);
    boundary::SCANNED_BYTES.with(|count| assert_eq!(count.get(), expected_bytes as usize));
    let terminal =
        parse_markdown_terminal(&source, 80, Palette::default(), Color::Reset, &mut cache);
    assert_eq!(perf_counters().full_parses, 1);
    assert_eq!(
        terminal.tail,
        parse_markdown(&source, 80, Palette::default(), Color::Reset)
    );
}

#[test]
#[serial_test::serial]
fn test_fence_boundaries_handle_markers_lengths_and_false_closers() {
    for source in [
        "```rust\nfirst\n\nsecond\n```\n\n",
        "~~~text\nfirst\n\nsecond\n~~~\n\n",
        "````\n```\n\n```\n````\n\n",
        "```text\n[ref]\n\n![image](url)\n- list\n| a |\n```\n\n",
        "   ```text\nfirst\n\nsecond\n   ```\n\n",
    ] {
        assert_eq!(stable_chunk_end(source, 0), source.len(), "{source:?}");
    }
    for source in [
        "```\nfirst\n\nsecond\n",
        "````\nfirst\n\n```\n\n",
        "```\nfirst\n\n``` invalid\n\n",
        "~~~\nfirst\n\n```\n\n",
        "```\nfirst\n\n    ```\n\n",
        "```\nfirst\n\n \t```\n\n",
    ] {
        assert_eq!(stable_chunk_end(source, 0), 0, "{source:?}");
    }
}

#[test]
#[serial_test::serial]
fn test_streaming_fence_and_sensitive_tail_match_full_reference() {
    let prefix = "```text\n第一行\n\n第二行\n```\n\n";
    for suffix in [
        "正文 **加粗** 与 `code`\n\n后文",
        "[later]\n\n[later]: https://example.com",
        "![图片][later]\n\n[later]: https://example.com/a.png",
        "[later]: https://example.com\n\n[later]",
        "- first\n\n  continuation\n- second",
        "-\tfirst\n\n  continuation\n- second",
        "-\n\n  continuation",
        "+\tfirst\n\n  continuation",
        "*\n\n  continuation",
        "1.\n\n   continuation",
        "1) first\n\n   continuation\n2) second",
        "a | b\n--- | ---\n1 | 2\n\n末尾",
        "![图片](https://example.com/a.png \"标题\")",
    ] {
        for width in [24, 80] {
            let mut source = prefix.to_owned();
            let mut cache = MarkdownRenderCache::default();
            for character in suffix.chars() {
                source.push(character);
                let rendered = parse_markdown_chunks_cached(
                    &source,
                    width,
                    Palette::default(),
                    Color::Reset,
                    &mut cache,
                );
                let reference = parse_markdown(&source, width, Palette::default(), Color::Reset);
                assert_eq!(
                    semantic_segments(rendered.segments()),
                    semantic_segments(reference.iter()),
                    "{source:?} width={width}",
                );
            }
            let terminal = parse_markdown_terminal(
                &source,
                width,
                Palette::default(),
                Color::Reset,
                &mut cache,
            );
            assert_eq!(
                terminal.tail,
                parse_markdown(&source, width, Palette::default(), Color::Reset)
            );
        }
    }
}

#[test]
#[serial_test::serial]
fn test_many_blank_lines_scan_once_per_boundary_search() {
    let source = format!("```text\n{}```\n\n", "body\n\n".repeat(1024));
    boundary::SCANNED_BYTES.with(|count| count.set(0));
    assert_eq!(stable_chunk_end(&source, 0), source.len());
    boundary::SCANNED_BYTES.with(|count| assert_eq!(count.get(), source.len()));
}

#[test]
#[serial_test::serial]
fn test_fence_closure_promotes_previously_mutable_body() {
    let prefix = "开始\n\n";
    let mut source = format!("{prefix}```text\n第一行\n\n第二行\n");
    let mut cache = MarkdownRenderCache::default();
    let open =
        parse_markdown_chunks_cached(&source, 80, Palette::default(), Color::Reset, &mut cache);
    assert_eq!(cache.stable_source_end, prefix.len());
    source.push_str("```\n\n后文");
    let closed =
        parse_markdown_chunks_cached(&source, 80, Palette::default(), Color::Reset, &mut cache);
    assert_eq!(closed.stable.len(), 2);
    assert!(Arc::ptr_eq(&open.stable[0], &closed.stable[0]));
    assert_eq!(cache.stable_source_end, source.len() - "后文".len());
    let reference = parse_markdown(&source, 80, Palette::default(), Color::Reset);
    assert_eq!(
        semantic_segments(closed.segments()),
        semantic_segments(reference.iter())
    );
    let resized =
        parse_markdown_chunks_cached(&source, 24, Palette::default(), Color::Reset, &mut cache);
    assert!(!Arc::ptr_eq(&closed.stable[1], &resized.stable[1]));
    let reference = parse_markdown(&source, 24, Palette::default(), Color::Reset);
    assert_eq!(
        semantic_segments(resized.segments()),
        semantic_segments(reference.iter())
    );
    let rewritten = source.replace("第一行", "改写");
    let rendered =
        parse_markdown_chunks_cached(&rewritten, 24, Palette::default(), Color::Reset, &mut cache);
    let reference = parse_markdown(&rewritten, 24, Palette::default(), Color::Reset);
    assert_eq!(
        semantic_segments(rendered.segments()),
        semantic_segments(reference.iter())
    );
}

#[test]
#[serial_test::serial]
fn test_stable_chunks_refresh_text_and_code_background_without_reparse() {
    struct RestoreTheme(Arc<peri_theme::theme::ThemeDefinition>);
    impl Drop for RestoreTheme {
        fn drop(&mut self) {
            peri_theme::atoms::THEME_ATOM.state().set(self.0.clone());
        }
    }

    let original = peri_theme::atoms::THEME_ATOM.state().read().clone();
    let _restore = RestoreTheme(original.clone());
    let source = "paragraph\n\n```text\nfirst\n\nsecond\n```\n\n";
    let mut cache = MarkdownRenderCache::default();
    let first =
        parse_markdown_chunks_cached(source, 80, Palette::default(), Color::Red, &mut cache);
    reset_perf_counters();
    let recolored =
        parse_markdown_chunks_cached(source, 80, Palette::default(), Color::Green, &mut cache);
    assert!(!Arc::ptr_eq(&first.stable[0], &recolored.stable[0]));
    assert_eq!(perf_counters().tail_parses, 0);
    assert_eq!(
        semantic_segments(recolored.segments()),
        semantic_segments(parse_markdown(source, 80, Palette::default(), Color::Green).iter()),
    );

    let mut updated = (*original).clone();
    updated.semantic.surface.sunken = if updated.semantic.surface.sunken == Color::Magenta {
        Color::Blue
    } else {
        Color::Magenta
    };
    peri_theme::atoms::THEME_ATOM.state().set(Arc::new(updated));
    reset_perf_counters();
    let background_changed =
        parse_markdown_chunks_cached(source, 80, Palette::default(), Color::Green, &mut cache);
    assert!(!Arc::ptr_eq(
        &recolored.stable[0],
        &background_changed.stable[0]
    ));
    assert_eq!(perf_counters().tail_parses, 0);
    assert_eq!(
        semantic_segments(background_changed.segments()),
        semantic_segments(parse_markdown(source, 80, Palette::default(), Color::Green).iter()),
    );
    let unchanged =
        parse_markdown_chunks_cached(source, 80, Palette::default(), Color::Green, &mut cache);
    assert!(Arc::ptr_eq(
        &background_changed.stable[0],
        &unchanged.stable[0]
    ));
}
