use super::*;

const TEST_BASE_FG: Color = Color::White;

use ratatui::style::Modifier;

fn flatten(segments: &[MarkdownSegment]) -> Vec<ratatui::text::Line<'static>> {
    segments
        .iter()
        .flat_map(|s| match s {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => vec![],
            MarkdownSegment::Image(img) => img.lines.clone(),
        })
        .collect()
}

#[test]
fn test_empty_input() {
    let result = flatten(&parse_markdown("", 80, Palette::default(), TEST_BASE_FG));
    assert!(result.is_empty());
}

#[test]
fn test_heading() {
    let result = flatten(&parse_markdown(
        "# Hello",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    assert_eq!(result.len(), 1);
    let line = &result[0];
    // 不渲染 # 前缀，标题文本当普通段落
    assert_eq!(line.spans.len(), 1);
    assert_eq!(line.spans[0].content, "Hello");
}

#[test]
fn test_paragraph() {
    let result = flatten(&parse_markdown(
        "hello world",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].spans[0].content, "hello world");
    // 普通段落文本应有 base_fg 色
    assert_eq!(
        result[0].spans[0].style.fg,
        Some(Color::White),
        "paragraph text should have base_fg = White"
    );
}

#[test]
fn test_adjacent_paragraphs() {
    let result = flatten(&parse_markdown(
        "a\n\nb",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].spans[0].content, "a");
    assert!(result[1].spans.is_empty());
    assert_eq!(result[2].spans[0].content, "b");
}

#[test]
fn test_inline_code() {
    let result = flatten(&parse_markdown(
        "use `code` here",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    let line = &result[0];
    // backtick 已剥离，span 内容为纯代码文本
    let code_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "code")
        .expect("inline code span content should be 'code' (backticks stripped)");
    // Palette::default().info = Blue
    assert_eq!(
        code_span.style.fg,
        Some(Color::Blue),
        "inline code should have fg = palette.info (Blue)"
    );
    // 行内代码无背景色
    assert_eq!(
        code_span.style.bg, None,
        "inline code should not have background"
    );
    // 普通文本 span 应有 base_fg
    let plain_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "use ")
        .expect("should have plain text span");
    assert_eq!(
        plain_span.style.fg,
        Some(Color::White),
        "plain text should have base_fg"
    );
}

#[test]
fn test_unordered_list() {
    let result = flatten(&parse_markdown(
        "- item 1\n- item 2",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    let non_empty: Vec<_> = result.iter().filter(|l| !l.spans.is_empty()).collect();
    assert_eq!(non_empty.len(), 2, "expected 2 non-empty list item lines");
    assert!(
        non_empty[0]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "• ")
    );
    assert!(
        non_empty[1]
            .spans
            .iter()
            .any(|s| s.content.as_ref() == "• ")
    );
}

#[test]
fn test_code_block_background_fills_each_visual_line() {
    let width = 12;
    let result = flatten(&parse_markdown(
        "```text\nshort\nthis line is much longer than the width\n```",
        width,
        Palette::default(),
        TEST_BASE_FG,
    ));

    assert!(result.len() > 2, "超长代码应折为多个视觉行");
    for line in result {
        assert_eq!(line.width(), width, "每个代码视觉行都应铺满 content 宽度");
        let background = line
            .spans
            .first()
            .and_then(|span| span.style.bg)
            .expect("代码行应使用主题控制的背景色");
        assert!(
            line.spans
                .iter()
                .all(|span| span.style.bg == Some(background)),
            "代码字符、前缀和右侧填充应使用同一背景色"
        );
    }
}

#[test]
fn test_code_block_spacing() {
    let result = flatten(&parse_markdown(
        "text\n\n```rust\nlet x = 1;\n```",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    // 段落 + 空行分隔 + 代码行
    assert!(result.len() >= 3);
    // 第一行是段落文本
    assert_eq!(result[0].spans[0].content, "text");
    // 第二行是空行（分隔）
    assert!(result[1].spans.is_empty());
}

#[test]
fn test_rule() {
    let result = flatten(&parse_markdown("---", 80, Palette::default(), TEST_BASE_FG));
    assert_eq!(result.len(), 1);
    let content: String = result[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(content.contains('─'));
}

#[test]
fn test_bold_text() {
    let result = flatten(&parse_markdown(
        "**bold**",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    let line = &result[0];
    assert!(
        line.spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD)),
        "bold text should have BOLD modifier"
    );
}

// ── 未闭合 code block 修复（步骤 1）──

#[test]
fn test_unclosed_code_block_content_visible() {
    // [回归测试] 流式输入末尾若 ``` 未闭合，内容不应被丢弃
    let input = "```rust\nlet x = 1;\nlet y = 2;";
    let result = flatten(&parse_markdown(input, 80, Palette::default(), TEST_BASE_FG));
    let content: String = result
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref().to_string())
        .collect();
    assert!(
        content.contains("let x = 1;"),
        "未闭合代码块首行内容应可见，实际：{content:?}"
    );
    assert!(
        content.contains("let y = 2;"),
        "未闭合代码块次行内容应可见，实际：{content:?}"
    );
}

#[test]
fn test_closed_code_block_unchanged_after_fix() {
    // 闭合的代码块渲染结果稳定（验证修复不引入回归）
    let input = "```rust\nlet x = 1;\n```";
    let result = flatten(&parse_markdown(input, 80, Palette::default(), TEST_BASE_FG));
    let content: String = result
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref().to_string())
        .collect();
    assert!(
        content.contains("let x = 1;"),
        "闭合代码块内容应可见，实际：{content:?}"
    );
}

// ── ensure_closed_code_fences ──

#[test]
fn test_ensure_closed_code_fences_even_count_unchanged() {
    // 已闭合：fence 数为偶数，不补
    let input = "```rust\ncode\n```";
    assert_eq!(ensure_closed_code_fences(input), input);
}

#[test]
fn test_ensure_closed_code_fences_zero_count_unchanged() {
    // 无 fence：不补
    let input = "普通段落文本";
    assert_eq!(ensure_closed_code_fences(input), input);
}

#[test]
fn test_ensure_closed_code_fences_odd_count_appends_closer() {
    // 未闭合：fence 数为奇数，补一个闭合
    let input = "```rust\nlet x = 1;";
    let result = ensure_closed_code_fences(input);
    assert_eq!(result, "```rust\nlet x = 1;\n```");
    // 验证补完后变成偶数（递归调用应不再追加）
    assert_eq!(ensure_closed_code_fences(&result), result);
}

#[test]
fn test_ensure_closed_code_fences_multiple_blocks() {
    // 多个代码块 + 末尾未闭合
    let input = "```rust\na\n```\n\ntext\n\n```python\nb";
    let result = ensure_closed_code_fences(input);
    assert!(result.ends_with("```"), "应在末尾补闭合 fence");
    assert_eq!(
        result.matches("```").count() % 2,
        0,
        "补完后 fence 数应为偶数"
    );
}
