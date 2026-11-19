use super::{
    Color, MarkdownRenderCache, MarkdownSegment, Palette, parse_markdown, preprocess_genui_fences,
};

#[test]
fn preprocess_closed_and_unclosed_blocks_preserves_surrounding_markdown() {
    for fence in ["```", "~~~~", "   ````"] {
        let source =
            format!("before\n\n{fence}gen-ui root = Stack([Text(\"你好\")])\n{fence}\n\nafter");
        assert_eq!(
            preprocess_genui_fences(&source),
            "before\n\n```gen-ui\nroot = Stack([Text(\"你好\")])\n```\n\nafter"
        );
    }
    assert_eq!(
        preprocess_genui_fences("```gen-ui root = Stack([Text(\"流式"),
        "```gen-ui-partial\nroot = Stack([Text(\"流式"
    );
    let normal = "```rust\nlet x = 1;\n```\n\n`gen-ui`\n\n> ```gen-ui\n> not a block";
    assert_eq!(preprocess_genui_fences(normal), normal);
}

#[test]
fn inner_backticks_cannot_close_the_markdown_block() {
    let source =
        "before\n\n```gen-ui\nroot = Stack([Text(\"one\n```\ntwo ```` three\")])\n```\n\nafter";
    let preprocessed = preprocess_genui_fences(source);
    assert_eq!(
        preprocessed.matches(super::parser::GENUI_BACKTICKS).count(),
        2
    );
    let segments = parse_markdown(source, 80, Palette::default(), Color::Reset);
    let genui: Vec<_> = segments
        .iter()
        .filter_map(|segment| match segment {
            MarkdownSegment::GenUi(segment) => Some(segment),
            _ => None,
        })
        .collect();
    assert_eq!(genui.len(), 1);
    assert!(genui[0].source.contains("one\n```\ntwo ```` three"));
    assert!(segments.iter().any(|segment| matches!(segment, MarkdownSegment::Text(lines) if lines.iter().any(|line| line.to_string().contains("after")))));
}

#[test]
fn genui_openers_stop_stable_chunks_but_not_ordinary_code() {
    let prefix = "paragraph\n\n";
    for opener in [
        "```gen-ui",
        "~~~gen-ui",
        "   ````GEN-UI root = Stack([])",
        "```gen-ui-partial",
    ] {
        let source =
            format!("{prefix}{opener}\nroot = Stack([Text(\"content\")])\n```\n\nafter\n\n");
        assert_eq!(
            super::stable_chunk_end(&source, 0),
            prefix.len(),
            "{opener}"
        );
    }
    let ordinary = "```text\n```gen-ui\n```gen-ui\n```\n\n";
    assert_eq!(super::stable_chunk_end(ordinary, 0), ordinary.len());
    let conservative = "```text\n```gen-ui\n\n~~~\n```\n\n";
    assert_eq!(super::stable_chunk_end(conservative, 0), 0);
}

#[test]
fn genui_stays_in_tail_and_matches_full_parse_through_streaming() {
    let prefix = "# Normal\n\nparagraph\n\n";
    let mut source = prefix.to_owned();
    let mut cache = MarkdownRenderCache::default();
    for character in "```gen-ui\nroot = Stack([Text(\"内容\")])\n```\n\n**after**".chars() {
        source.push(character);
        let rendered = super::parse_markdown_chunks_cached(
            &source,
            80,
            Palette::default(),
            Color::Reset,
            &mut cache,
        );
        assert!(rendered.stable.iter().all(|chunk| {
            chunk
                .iter()
                .all(|segment| !matches!(segment, MarkdownSegment::GenUi(_)))
        }));
        if source.contains("```gen-ui\n") {
            assert_eq!(cache.stable_source_end, prefix.len());
        }
    }
    let rendered = super::parse_markdown_chunks_cached(
        &source,
        80,
        Palette::default(),
        Color::Reset,
        &mut cache,
    );
    let tail_genui = rendered
        .tail
        .iter()
        .find(|segment| matches!(segment, MarkdownSegment::GenUi(_)))
        .unwrap();
    let full = parse_markdown(&source, 80, Palette::default(), Color::Reset);
    assert_eq!(
        Some(tail_genui),
        full.iter()
            .find(|segment| matches!(segment, MarkdownSegment::GenUi(_)))
    );
}

#[test]
fn unclosed_fence_is_partial_even_when_tree_is_complete() {
    let segments = parse_markdown(
        "```gen-ui\nroot = Stack([Text(\"ok\")])",
        80,
        Palette::default(),
        Color::Reset,
    );
    let MarkdownSegment::GenUi(segment) = &segments[0] else {
        panic!("应生成 GenUi 段");
    };
    assert!(segment.partial);
    assert!(!segment.parsed.incomplete);
    assert!(segment.errors.is_empty());
}
