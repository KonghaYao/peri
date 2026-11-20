use super::Color;
use super::Palette;
use super::fence::FenceClose;
use super::fence::FenceKind;
use super::fence::MermaidFence;
use super::fence::MermaidMessageRef;
use super::fence::scan_mermaid_fences;
use super::parse_markdown;
use super::types::MarkdownSegment;

fn mermaid_segments(source: &str) -> Vec<super::types::MermaidSegment> {
    parse_markdown(source, 80, Palette::default(), Color::Reset)
        .into_iter()
        .filter_map(|segment| match segment {
            MarkdownSegment::Mermaid(segment) => Some(segment),
            _ => None,
        })
        .collect()
}

fn line_text(line: &ratatui::text::Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn all_text(segment: &super::types::MermaidSegment) -> String {
    segment
        .lines
        .iter()
        .map(line_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn scans_first_mermaid_token_and_both_fence_kinds() {
    let source = "```mermaid\ngraph TD\nA-->B\n```\n\n~~~Mermaid title\nsequenceDiagram\n~~~\n\n```mermaidx\nnot mermaid\n```\n\n```rust\nlet x = 1;\n```\n";
    let fences = scan_mermaid_fences(source, true);
    assert_eq!(fences.len(), 2);
    assert_eq!(fences[0].kind, FenceKind::Backtick);
    assert_eq!(fences[0].marker_len, 3);
    assert_eq!(fences[0].language, "mermaid");
    assert_eq!(fences[0].close, FenceClose::Closed);
    assert_eq!(fences[0].source, "graph TD\nA-->B\n");
    assert_eq!(fences[0].raw, "graph TD\nA-->B\n");
    assert_eq!(fences[0].open_start, source.find("```mermaid").unwrap());
    assert_eq!(
        fences[0].body_start,
        fences[0].open_start + "```mermaid\n".len()
    );
    assert!(fences[0].opener_confirmed);

    assert_eq!(fences[1].kind, FenceKind::Tilde);
    assert_eq!(fences[1].language, "Mermaid");
    assert_eq!(fences[1].info, "Mermaid title");
    assert_eq!(fences[1].close, FenceClose::Closed);
}

#[test]
fn raw_bytes_are_exact_after_unicode_image_and_genui_blocks() {
    let source = "# 中文标题\n\n中文段落，含图片：![示意图](https://example.com/图.png)\n\n```gen-ui\nroot = Stack([Text(\"你好\")])\n```\n\n```mermaid\ngraph TD\n  用户-->系统\n```";
    let segments = mermaid_segments(source);
    assert_eq!(segments.len(), 1);
    let fence = &segments[0].fence;
    assert_eq!(fence.source, "graph TD\n  用户-->系统\n");
    assert_eq!(fence.raw, "graph TD\n  用户-->系统\n");
    assert_eq!(fence.open_start, source.find("```mermaid").unwrap());
    assert!(all_text(&segments[0]).contains("用户-->系统"));
}

#[test]
fn blockquote_and_list_fences_keep_raw_bytes_and_strip_container_prefixes() {
    let quoted = "> ```mermaid\n> graph TD\n> A-->B\n> ```\n";
    let fences = scan_mermaid_fences(quoted, true);
    assert_eq!(fences.len(), 1);
    assert_eq!(fences[0].raw, "> graph TD\n> A-->B\n");
    assert_eq!(fences[0].source, "graph TD\nA-->B\n");
    assert_eq!(fences[0].opening_line, "> ```mermaid");

    let listed = "- item\n\n  ```mermaid\n  graph TD\n  A-->B\n  ```\n";
    let fences = scan_mermaid_fences(listed, true);
    assert_eq!(fences.len(), 1);
    assert_eq!(fences[0].raw, "  graph TD\n  A-->B\n");
    assert_eq!(fences[0].source, "graph TD\nA-->B\n");
}

#[test]
fn closing_fence_is_confirmed_only_after_newline_or_terminal_eof() {
    let without_newline = "```mermaid\ngraph TD\n```";
    let streamed = scan_mermaid_fences(without_newline, false);
    assert_eq!(streamed[0].close, FenceClose::Provisional);
    assert!(!streamed[0].closed());
    assert_eq!(streamed[0].source, "graph TD\n");
    assert_eq!(streamed[0].raw, "graph TD\n");
    assert_eq!(streamed[0].end, without_newline.len());

    let terminal = scan_mermaid_fences(without_newline, true);
    assert_eq!(terminal[0].close, FenceClose::Closed);
    assert!(terminal[0].closed());

    let with_newline = format!("{without_newline}\n");
    assert_eq!(
        scan_mermaid_fences(&with_newline, false)[0].close,
        FenceClose::Closed
    );

    let open = scan_mermaid_fences("```mermaid\ngraph TD\nA-->B", false);
    assert_eq!(open[0].close, FenceClose::Open);
    assert_eq!(open[0].source, "graph TD\nA-->B");
    assert_eq!(open[0].raw, "graph TD\nA-->B");
}

#[test]
fn longer_openers_require_matching_closers_and_support_tilde_fences() {
    let four = "````mermaid\n```\n````\n";
    let fences = scan_mermaid_fences(four, true);
    assert_eq!(fences[0].marker_len, 4);
    assert_eq!(fences[0].close, FenceClose::Closed);
    assert_eq!(fences[0].source, "```\n");

    let three_closes_four = "````mermaid\n```\n";
    let streamed = scan_mermaid_fences(three_closes_four, false);
    assert_eq!(streamed[0].close, FenceClose::Open);
    assert_eq!(streamed[0].source, "```\n");

    let tilde = "~~~mermaid\ngraph TD\n~~~\n";
    let fences = scan_mermaid_fences(tilde, false);
    assert_eq!(fences[0].kind, FenceKind::Tilde);
    assert_eq!(fences[0].close, FenceClose::Closed);
}

#[test]
fn unconfirmed_opener_info_stays_provisional_while_streaming() {
    let partial = "```mermaid";
    let streamed = scan_mermaid_fences(partial, false);
    assert_eq!(streamed.len(), 1);
    assert!(!streamed[0].opener_confirmed);
    assert_eq!(streamed[0].close, FenceClose::Open);

    let terminal = scan_mermaid_fences(partial, true);
    assert!(terminal[0].opener_confirmed);
}

#[test]
fn duplicate_diagrams_get_distinct_position_keys() {
    let source = "```mermaid\ngraph TD\nA-->B\n```\n\n```mermaid\ngraph TD\nA-->B\n```\n";
    let segments = mermaid_segments(source);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].fence.source, segments[1].fence.source);
    let first = source.find("```mermaid").unwrap();
    let second = source.rfind("```mermaid").unwrap();
    assert_eq!(segments[0].fence.open_start, first);
    assert_eq!(segments[1].fence.open_start, second);

    let message = MermaidMessageRef::Persistent("message-1".to_string());
    let first_key = segments[0].fence.key("session-1", message.clone());
    let second_key = segments[1].fence.key("session-1", message.clone());
    assert_ne!(first_key, second_key);
    assert_ne!(
        first_key,
        segments[0]
            .fence
            .key("session-1", MermaidMessageRef::Local(7))
    );
    assert_ne!(
        first_key,
        segments[0].fence.key("session-2", message.clone())
    );
    assert_eq!(first_key, segments[0].fence.key("session-1", message));
}

#[test]
fn position_reuse_requires_the_same_opening_fence() {
    let original = scan_mermaid_fences("```mermaid\ngraph TD\nA-->B\n```\n", true);
    let grown = scan_mermaid_fences("```mermaid\ngraph TD\nA-->B\nA-->C\n```\n", true);
    assert!(grown[0].reuses_position(&original[0]));

    let tilde = scan_mermaid_fences("~~~mermaid\ngraph TD\nA-->B\n~~~\n", true);
    assert!(!tilde[0].reuses_position(&original[0]));

    let info_changed = scan_mermaid_fences("```mermaid title\ngraph TD\nA-->B\n```\n", true);
    assert!(!info_changed[0].reuses_position(&original[0]));
}

#[test]
fn ordinary_code_blocks_keep_their_existing_text_segments() {
    let source = "```rust\nlet x = 1;\n```";
    let segments = parse_markdown(source, 80, Palette::default(), Color::Reset);
    assert!(
        !segments
            .iter()
            .any(|segment| matches!(segment, MarkdownSegment::Mermaid(_)))
    );
    let text: String = segments
        .iter()
        .flat_map(|segment| match segment {
            MarkdownSegment::Text(lines) => lines.clone(),
            _ => Vec::new(),
        })
        .map(|line| line_text(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("let x = 1;"));
}

#[test]
#[serial_test::serial]
fn stable_chunks_and_tail_carry_global_fence_coordinates() {
    let source = "intro\n\n```mermaid\ngraph TD\nA-->B\n```\n\nafter";
    let expected_start = source.find("```mermaid").unwrap();
    let mut cache = super::MarkdownRenderCache::default();
    let first = super::parse_markdown_chunks_cached(
        source,
        80,
        Palette::default(),
        Color::Reset,
        &mut cache,
    );
    let stable = first
        .stable
        .iter()
        .flat_map(|chunk| chunk.iter())
        .find_map(|segment| match segment {
            MarkdownSegment::Mermaid(segment) => Some(segment),
            _ => None,
        })
        .expect("闭合围栏应进入稳定块");
    assert_eq!(stable.fence.open_start, expected_start);
    assert_eq!(stable.fence.source, "graph TD\nA-->B\n");
    let full = mermaid_segments(source);
    assert_eq!(
        stable.fence, full[0].fence,
        "稳定块元数据必须与完整解析逐字段一致"
    );
    let stable_arc = first.stable[0].clone();

    let appended = format!("{source} more");
    let second = super::parse_markdown_chunks_cached(
        &appended,
        80,
        Palette::default(),
        Color::Reset,
        &mut cache,
    );
    assert!(super::Arc::ptr_eq(&stable_arc, &second.stable[0]));
    let stable_again = second
        .stable
        .iter()
        .flat_map(|chunk| chunk.iter())
        .find_map(|segment| match segment {
            MarkdownSegment::Mermaid(segment) => Some(segment),
            _ => None,
        })
        .expect("稳定块复用后图元数据不能丢失");
    assert_eq!(stable_again.fence.open_start, expected_start);

    let rematerialized = super::parse_markdown_chunks_cached(
        &appended,
        40,
        Palette::default(),
        Color::Reset,
        &mut cache,
    );
    let narrow = rematerialized
        .stable
        .iter()
        .flat_map(|chunk| chunk.iter())
        .find_map(|segment| match segment {
            MarkdownSegment::Mermaid(segment) => Some(segment),
            _ => None,
        })
        .expect("宽度变化重转稳定块时必须携带围栏元数据");
    assert_eq!(narrow.fence.open_start, expected_start);
    assert_eq!(narrow.fence.source, "graph TD\nA-->B\n");

    let unfinished = "intro\n\n```mermaid\ngraph TD\nA-->B";
    let rendered = super::parse_markdown_chunks_cached(
        unfinished,
        80,
        Palette::default(),
        Color::Reset,
        &mut super::MarkdownRenderCache::default(),
    );
    let tail = rendered
        .tail
        .iter()
        .find_map(|segment| match segment {
            MarkdownSegment::Mermaid(segment) => Some(segment),
            _ => None,
        })
        .expect("流式尾部必须保留未闭合 Mermaid 段");
    assert_eq!(tail.fence.close, FenceClose::Open);
    assert_eq!(
        tail.fence.open_start,
        unfinished.find("```mermaid").unwrap()
    );
    assert_eq!(tail.fence.source, "graph TD\nA-->B");

    let terminal = super::parse_markdown_terminal(
        "intro\n\n```mermaid\ngraph TD\n```",
        80,
        Palette::default(),
        Color::Reset,
        &mut super::MarkdownRenderCache::default(),
    );
    let terminal_mermaid = terminal
        .tail
        .iter()
        .find_map(|segment| match segment {
            MarkdownSegment::Mermaid(segment) => Some(segment),
            _ => None,
        })
        .expect("完整解析在 EOF 处确认关闭行");
    assert_eq!(terminal_mermaid.fence.close, FenceClose::Closed);
}

#[test]
fn fence_shift_origin_is_used_for_fragment_coordinates() {
    let fragment = "```mermaid\ngraph TD\nA-->B\n```";
    let mut fence = scan_mermaid_fences(fragment, true).remove(0);
    fence.shift_origin(100);
    assert_eq!(fence.open_start, 100);
    assert_eq!(fence.body_start, 100 + "```mermaid\n".len());
    assert_eq!(fence.raw, "graph TD\nA-->B\n");
}

#[test]
fn fence_type_is_reachable_from_segment_metadata() {
    let source = "```mermaid\ngraph TD\nA-->B\n```\n";
    let segments = mermaid_segments(source);
    let MermaidFence { language, .. } = &segments[0].fence;
    assert_eq!(language, "mermaid");
}
