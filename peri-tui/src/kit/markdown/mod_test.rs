//! Tests
use super::convert::wrap_styled_line;
use super::*;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::text::Line;

/// 测试辅助：主题正文色（段落文本默认前景）。
const TEST_BASE_FG: Color = Color::White;

/// 测试辅助：将 parse_markdown 返回的段落展平为 Line 列表。
fn flatten(segments: &[MarkdownSegment]) -> Vec<ratatui::text::Line<'static>> {
    segments
        .iter()
        .flat_map(|s| match s {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => vec![],
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
fn test_code_block() {
    let result = flatten(&parse_markdown(
        "```rust\nlet x = 1;\n```",
        80,
        Palette::default(),
        TEST_BASE_FG,
    ));
    // 单一代码块：至少渲染一行代码
    assert!(!result.is_empty());
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

// ── Phase 2：parse_markdown_cached 续跑测试 ──────────────────────

/// 测试辅助：把 segments 拼成纯文本便于断言。
fn segments_to_text(segments: &[MarkdownSegment]) -> String {
    segments
        .iter()
        .flat_map(|s| match s {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => vec![],
        })
        .flat_map(|l| l.spans)
        .map(|s| s.content.into_owned())
        .collect()
}

#[test]
fn test_cached_parse_matches_full_parse_on_first_call() {
    // 首次调用（cache 空）：输出应与全量 parse_markdown 一致
    let mut cache = MarkdownRenderCache::default();
    let input = "para1\n\npara2";
    let cached = parse_markdown_cached(input, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full = parse_markdown(input, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&cached),
        segments_to_text(&full),
        "首次调用 cached 与 full 输出应一致"
    );
}

#[test]
fn test_cached_parse_reuses_prefix_on_append() {
    // 流式追加：text1 → text1+text2，cache 应命中 stable_text 前缀
    let mut cache = MarkdownRenderCache::default();

    // 第一次：一个完整 paragraph（以 \n\n 结尾，触发持久化）
    let t1 = "para1\n\n";
    let _r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert!(
        cache.stable_text_len() > 0,
        "首次以 \\n\\n 结尾应持久化 stable_text"
    );
    assert_eq!(
        cache.stable_processed_block_count(),
        1,
        "应处理 1 个 block（Paragraph）"
    );

    // 第二次：追加 para2，仍以 t1 为前缀
    let t2 = "para1\n\npara2";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "续跑输出应与全量一致"
    );
    // [新契约] 任意输入都可持久化（persist 前回滚尾部不稳定块保证正确性）：
    // stable_text 扩展为整个 t2，下次续跑可命中更多前缀。
    assert_eq!(
        cache.stable_text_len(),
        t2.len(),
        "t2 持久化后 stable_text 应扩展为整个文本"
    );
}

#[test]
fn test_cached_parse_invalidates_on_width_change() {
    // width 变化 → cache 失效
    let mut cache = MarkdownRenderCache::default();
    let t1 = "para1\n\n";
    let _r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert!(cache.stable_text_len() > 0);

    // width 从 80 改到 60：cache 应失效（can_reuse = false）
    let t2 = "para1\n\npara2";
    let r2 = parse_markdown_cached(t2, 60, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 60, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "width 变化后应全量重跑，输出一致"
    );
}

#[test]
fn test_cached_parse_invalidates_on_palette_change() {
    // palette 变化 → cache 失效
    let mut cache = MarkdownRenderCache::default();
    let t1 = "para1\n\n";
    let p1 = Palette::default();
    let _r1 = parse_markdown_cached(t1, 80, p1, TEST_BASE_FG, &mut cache);
    assert!(cache.stable_text_len() > 0);

    // 修改 palette（替换 fg 颜色）
    let mut p2 = Palette::default();
    p2.fg = ratatui::style::Color::Red;
    let t2 = "para1\n\npara2";
    let r2 = parse_markdown_cached(t2, 80, p2, TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, p2, TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "palette 变化后应全量重跑，输出一致"
    );
}

#[test]
fn test_cached_parse_preserves_spacing_on_append() {
    // [回归测试] spacing 跨 block 边界正确——续跑时新 block 的 spacing 决策
    // 应与全量一致。多 paragraph + list 场景。
    let mut cache = MarkdownRenderCache::default();

    // 第一次：paragraph + list（以 \n\n 结尾）
    let t1 = "intro paragraph\n\n- item 1\n- item 2\n\n";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full1 = parse_markdown(t1, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r1),
        segments_to_text(&full1),
        "首次输出应与全量一致"
    );

    // 第二次：追加新 paragraph
    let t2 = "intro paragraph\n\n- item 1\n- item 2\n\nnew paragraph";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "续跑追加 paragraph 后 spacing 应与全量一致"
    );
}

#[test]
fn test_cached_parse_preserves_table_boundary_on_append() {
    // [回归测试] Table 触发 flush 的 spacing 跨续跑正确
    let mut cache = MarkdownRenderCache::default();

    // 第一次：paragraph + table（以 \n\n 结尾）
    let t1 = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n";
    let _r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);

    // 第二次：table 后追加 paragraph
    let t2 = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter table para";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "Table 后追加 paragraph 续跑输出应一致"
    );
    // 至少 2 个 segment（Text + Table + Text），但 segments_to_text 只统计 Text
    assert!(r2.len() >= 2, "应至少 2 个 segment（含 Table）");
}

#[test]
fn test_cached_parse_multiple_progressive_appends() {
    // [回归测试] 多次渐进追加，cache 累积稳定，每次输出与全量一致
    let mut cache = MarkdownRenderCache::default();
    let steps = [
        "first\n\n",
        "first\n\nsecond\n\n",
        "first\n\nsecond\n\nthird\n\n",
        "first\n\nsecond\n\nthird\n\nfourth",
    ];
    let mut prev_stable_len = 0usize;
    for (i, text) in steps.iter().enumerate() {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} 输出应与全量一致"
        );
        // stable_text 应单调增长（每次新闭合 \n\n 时扩展）
        if text.ends_with("\n\n") {
            assert!(
                cache.stable_text_len() >= prev_stable_len,
                "step {i} stable_text 应单调增长"
            );
            prev_stable_len = cache.stable_text_len();
        }
    }
}

#[test]
fn test_cached_parse_unclosed_code_block_not_persisted_but_still_correct() {
    // [回归测试] text 以未闭合 code block 结尾（fence 数为奇数，sanitized 补闭合）。
    // [新契约] 任意输入都可持久化；但补全的闭合 fence 会破坏下次追加的前缀匹配
    // （追加行进入代码块 → sanitized 与 stable_text 前缀不一致）→ 全量重跑，输出仍正确。
    let mut cache = MarkdownRenderCache::default();

    // 第一次：未闭合 code block（末尾不是 \n）
    let t1 = "```rust\nlet x = 1;";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full1 = parse_markdown(t1, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r1),
        segments_to_text(&full1),
        "未闭合 code block 输出应正确"
    );
    // sanitized = "```rust\nlet x = 1;\n```"，持久化为整个 sanitized
    assert_eq!(
        cache.stable_text_len(),
        "```rust\nlet x = 1;\n```".len(),
        "未闭合 code block 的 sanitized（补闭合后）应持久化"
    );

    // 第二次：代码块内追加一行 → 前缀不匹配 → 全量重跑，输出与全量一致
    let t2 = "```rust\nlet x = 1;\nlet y = 2;";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "代码块内追加行后输出应与全量一致"
    );

    // 第三次：代码块闭合 + 追加文本 → 命中续跑，输出仍一致
    let t3 = "```rust\nlet x = 1;\nlet y = 2;\n```\n\nafter";
    let r3 = parse_markdown_cached(t3, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full3 = parse_markdown(t3, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r3),
        segments_to_text(&full3),
        "闭合代码块后追加文本输出应与全量一致"
    );
}

#[test]
fn test_cached_parse_empty_input_no_cache_pollution() {
    // 空输入不应污染 cache
    let mut cache = MarkdownRenderCache::default();
    let _r = parse_markdown_cached("", 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert_eq!(cache.stable_text_len(), 0, "空输入不应持久化");
}

#[test]
fn test_cached_parse_does_not_persist_unstable_suffix() {
    // [回归测试] 流式期间 text 末尾是不稳定（非 \n 结尾），cache 仍持久化
    // （persist 前回滚尾部不稳定块），下次相同前缀追加仍能命中且输出正确。
    let mut cache = MarkdownRenderCache::default();

    // Step 1：闭合 paragraph
    let t1 = "para1\n\n";
    let _r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let stable_len_after_t1 = cache.stable_text_len();
    assert!(stable_len_after_t1 > 0);

    // Step 2：追加半个 paragraph（不以 \n 结尾）——[新契约] 同样持久化并扩展
    let t2 = "para1\n\npara2 half";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert_eq!(
        cache.stable_text_len(),
        t2.len(),
        "追加非闭合内容后 stable_text 应扩展为整个文本"
    );
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "追加半个 paragraph 输出应与全量一致"
    );

    // Step 3：继续追加（仍以 t2 为前缀）
    let t3 = "para1\n\npara2 half continued";
    let r3 = parse_markdown_cached(t3, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full3 = parse_markdown(t3, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r3),
        segments_to_text(&full3),
        "多次追加后仍应正确"
    );
}

// ── 新契约回归测试：尾部不稳定块回滚（流式散文 / lazy continuation 等）─────

/// [综合回归测试] 逐 token 流式追加：把一段含段落/列表/代码块/表格的混合文本
/// 按 token 边界逐步追加（模拟 agent 流式输出），**每一帧**都断言缓存续跑输出
/// 与全量解析一致。覆盖尾部块回滚、表头翻转失效、表格增长失效、列表哨兵移位
/// 等全部增量路径的组合。
#[test]
fn test_cached_streaming_every_frame_matches_full_parse() {
    let mut cache = MarkdownRenderCache::default();
    let tokens: Vec<&str> = vec![
        "Let me ",
        "explain ",
        "the plan:\n\n",
        "- first ",
        "step\n",
        "- second ",
        "step\n",
        "- third\n\n",
        "```rust\n",
        "let x = 1;\n",
        "```\n\n",
        "| A | B |\n",
        "|---|---|\n",
        "| 1 | 2 |\n",
        "| 3 | 4 |\n\n",
        "done",
        " ...",
        " ... more\n\n",
        "## Summary\n",
        "**bold** and `code`.",
    ];
    let mut text = String::new();
    for (i, tok) in tokens.iter().enumerate() {
        text.push_str(tok);
        let cached = parse_markdown_cached(&text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(&text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "token {i} ('{tok}') 帧输出应与全量一致\n文本: {text:?}"
        );
        assert_eq!(
            cached
                .iter()
                .filter(|s| matches!(s, MarkdownSegment::Table(_)))
                .count(),
            full.iter()
                .filter(|s| matches!(s, MarkdownSegment::Table(_)))
                .count(),
            "token {i} Table segment 数应与全量一致"
        );
    }
}

// ── 表格渲染测试 ─────────────────────────────────────────────────

/// [回归测试] 流式散文最坏情形：单段文本逐 token 同行增长。
/// 旧契约下（仅 \n 结尾持久化）此场景 stable_text 恒空 → 每 token 全量 convert。
/// 新契约：尾部 Paragraph 回滚，续跑重渲最后段落，输出始终与全量一致。
#[test]
fn test_cached_prose_single_paragraph_grows() {
    let mut cache = MarkdownRenderCache::default();
    let steps = [
        "text",
        "text more",
        "text more words",
        "text more words and",
        "text more words and more",
    ];
    let mut prev_stable_len = 0usize;
    for (i, text) in steps.iter().enumerate() {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} 单段散文增长输出应与全量一致"
        );
        // stable_text 应单调扩展（任意输入都持久化）
        assert!(
            cache.stable_text_len() >= prev_stable_len,
            "step {i} stable_text 应单调扩展"
        );
        prev_stable_len = cache.stable_text_len();
    }
}

/// [回归测试] 流式散文跨行增长：单 \n 是 soft-break（合并为同一段落一行）。
/// `para\n` → `para\nmore` 是同一 Paragraph 内容增长（渲染为 "para more"），
/// 续跑必须重渲而非跳过。
#[test]
fn test_cached_paragraph_soft_break_grows() {
    let mut cache = MarkdownRenderCache::default();
    for (i, text) in ["para\n", "para\nmore", "para\nmore words"]
        .iter()
        .enumerate()
    {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} soft-break 段落增长输出应与全量一致"
        );
    }
}

/// [回归测试] 列表项 lazy continuation：`- A\n` 追加无缩进文本行时，
/// 内容并入最后一个列表项（`- A\nmore` → "• A more"）。旧缓存若跳过
/// 最后列表项会永久显示旧内容。
#[test]
fn test_cached_list_lazy_continuation_grows() {
    let mut cache = MarkdownRenderCache::default();
    let t1 = "- A\n- B\n";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert!(segments_to_text(&r1).contains("B"), "t1 应含 B");

    // lazy continuation：追加无缩进行，内容并入最后列表项
    let t2 = "- A\n- B\ncontinued";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    let text2 = segments_to_text(&r2);
    assert!(
        text2.contains("continued"),
        "lazy continuation 内容应可见，实际: {text2:?}"
    );
    assert_eq!(text2, segments_to_text(&full2), "续跑输出应与全量一致");
}

/// [回归测试] 标题同行增长：`# h` → `# h x` 是同一 Heading block 内容变化。
#[test]
fn test_cached_heading_same_line_growth() {
    let mut cache = MarkdownRenderCache::default();
    for (i, text) in ["# h", "# h x", "# h xy"].iter().enumerate() {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} 标题同行增长输出应与全量一致"
        );
    }
}

/// [回归测试] 缩进代码块增长：`    a` → `    a\n    b` 是同一 CodeBlock 行数变化
/// （渲染从单行 inline 变为多行 │ 前缀），续跑必须重渲。
#[test]
fn test_cached_indented_code_block_grows() {
    let mut cache = MarkdownRenderCache::default();
    for (i, text) in ["    a", "    a\n    b", "    a\n    b\n    c"]
        .iter()
        .enumerate()
    {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} 缩进代码块增长输出应与全量一致"
        );
    }
}

/// [回归测试] 规则线类型翻转：`---` 追加字符后不再是 Rule 而是 Paragraph。
/// 旧缓存若持久化 Rule（processed=1）并跳过，会永远显示分割线。
#[test]
fn test_cached_rule_flips_to_paragraph() {
    let mut cache = MarkdownRenderCache::default();
    // Step 1：Rule
    let t1 = "---";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full1 = parse_markdown(t1, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r1),
        segments_to_text(&full1),
        "Rule 输出应与全量一致"
    );
    // Step 2：同行追加 → 翻转为 Paragraph
    let t2 = "---x";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "Rule 翻转后输出应与全量一致"
    );
    // Step 3：追加完整行 → Rule + 段落
    let t3 = "---x\n\npara";
    let r3 = parse_markdown_cached(t3, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full3 = parse_markdown(t3, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r3),
        segments_to_text(&full3),
        "追加段落输出应与全量一致"
    );
}

/// [回归测试] 已闭合代码块 + 同行追加：`\`\`\`\ncode\n\`\`\``（无尾换行）追加文本
/// 会并入代码块内容（fence 破坏），必须重渲而非跳过旧 CodeBlock。
#[test]
fn test_cached_closed_code_block_same_line_append() {
    let mut cache = MarkdownRenderCache::default();
    let t1 = "```rust\ncode\n```";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full1 = parse_markdown(t1, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r1),
        segments_to_text(&full1),
        "闭合代码块输出应与全量一致"
    );
    let t2 = "```rust\ncode\n```more";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "闭合代码块同行追加后输出应与全量一致"
    );
}

/// [回归测试] 已闭合代码块是稳定中间块：代码块后追加文本（换行分隔），
/// 代码块内容不变，续跑只处理新段落。
#[test]
fn test_cached_closed_code_block_then_text_appended() {
    let mut cache = MarkdownRenderCache::default();
    let t1 = "```rust\ncode\n```\n\npara1";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full1 = parse_markdown(t1, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r1),
        segments_to_text(&full1),
        "首次输出应与全量一致"
    );
    let t2 = "```rust\ncode\n```\n\npara1 continued";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "代码块后段落增长输出应与全量一致"
    );
}

// ── 表格渲染测试 ─────────────────────────────────────────────────

/// 辅助：验证表格 header + data rows 在渲染后的 lines 中都可见。
fn assert_table_contains(md: &str, width: usize, headers: &[&str], rows: &[&[&str]], label: &str) {
    use ratatui_kit::components::TableTheme;

    let segments = parse_markdown(md, width, Palette::default(), TEST_BASE_FG);
    let table_data = segments
        .iter()
        .find_map(|s| match s {
            MarkdownSegment::Table(d) => Some(d),
            _ => None,
        })
        .unwrap_or_else(|| panic!("[{label}] 应解析出 Table segment"));

    let expected_header_count = headers.len();
    let expected_row_count = rows.len();
    assert_eq!(
        table_data.headers.len(),
        expected_header_count,
        "[{label}] 表头应有 {expected_header_count} 列，实际 {}",
        table_data.headers.len()
    );
    assert_eq!(
        table_data.rows.len(),
        expected_row_count,
        "[{label}] 应有 {expected_row_count} 行数据，实际 {}",
        table_data.rows.len()
    );

    let theme = TableTheme::from_palette(&Palette::default());
    let lines = table_data_to_lines(table_data, &theme, width);

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect();

    // 验证每个表头
    for h in headers {
        assert!(
            all_text.contains(h),
            "[{label}] 应包含表头 '{h}'，实际文本:\n{all_text}"
        );
    }
    // 验证每个数据单元格
    for (ri, row) in rows.iter().enumerate() {
        for cell in *row {
            assert!(
                all_text.contains(cell),
                "[{label}] 应包含数据行{ri}单元格 '{cell}'，实际文本:\n{all_text}"
            );
        }
    }
    // 验证至少渲染了足够行数（顶边框 + 表头行 + 分隔行 + N行数据 + 底边框）
    let min_expected_lines =
        2 /* top+bottom border */ + 1 /* header */ + 1 /* separator */+ rows.len();
    assert!(
        lines.len() >= min_expected_lines,
        "[{label}] 渲染行数 {} 少于预期最小值 {min_expected_lines}",
        lines.len()
    );
}

#[test]
fn test_table_1_simple_cjk() {
    // 1️⃣ 简单中文表
    assert_table_contains(
        "| 列A | 列B |\n|------|------|\n| 值1 | 值2 |\n| 数据 | 表格 |\n",
        80,
        &["列A", "列B"],
        &[&["值1", "值2"], &["数据", "表格"]],
        "1-simple-cjk",
    );
}

#[test]
fn test_table_2_long_cjk_content() {
    // 2️⃣ 长中文内容
    assert_table_contains(
        "| 文件名 | 说明 |\n|--------|------|\n| 这是一个很长的中文文件名示例 | 核心改动——修改了表格渲染管线 |\n",
        80,
        &["文件名", "说明"],
        &[&[
            "这是一个很长的中文文件名示例",
            "核心改动——修改了表格渲染管线",
        ]],
        "2-long-cjk",
    );
}

#[test]
fn test_table_3_mixed_cjk_ascii() {
    // 3️⃣ 中英混合
    assert_table_contains(
        "| File Path | 改动类型 |\n|-----------|----------|\n| table.rs | 修改核心逻辑 |\n| types.rs | 新增数据结构 |\n",
        80,
        &["File Path", "改动类型"],
        &[&["table.rs", "修改核心逻辑"], &["types.rs", "新增数据结构"]],
        "3-mixed-cjk-ascii",
    );
}

#[test]
fn test_table_4_inline_code_in_cells() {
    // 4️⃣ 单元格含行内代码
    assert_table_contains(
        "| 函数 | 说明 |\n|------|------|\n| `foo()` | 执行某某操作 |\n| `bar()` | 另一操作 |\n",
        80,
        &["函数", "说明"],
        &[&["foo", "执行某某操作"], &["bar", "另一操作"]],
        "4-inline-code",
    );
}

#[test]
fn test_table_5_narrow_width() {
    // 5️⃣ 窄宽度（40列）
    assert_table_contains(
        "| 列A | 列B |\n|------|------|\n| 值1 | 值2 |\n",
        40,
        &["列A", "列B"],
        &[&["值1", "值2"]],
        "5-narrow-40",
    );
}

#[test]
fn test_table_6_three_columns() {
    // 6️⃣ 三列中英表
    assert_table_contains(
        "| 模块 | 状态 | 备注 |\n|------|------|------|\n| peri-tui | 已完成 | 主要前端 |\n| peri-agent | 进行中 | 核心引擎 |\n| peri-acp | 已完成 | 协议层 |\n",
        80,
        &["模块", "状态", "备注"],
        &[
            &["peri-tui", "已完成", "主要前端"],
            &["peri-agent", "进行中", "核心引擎"],
            &["peri-acp", "已完成", "协议层"],
        ],
        "6-three-cols",
    );
}

#[test]
fn test_table_7_many_rows() {
    // 7️⃣ 多行数据
    assert_table_contains(
        "| # | 项目 |\n|---|------|\n| 1 | 第一项 |\n| 2 | 第二项 |\n| 3 | 第三项 |\n| 4 | 第四项 |\n| 5 | 第五项 |\n",
        80,
        &["#", "项目"],
        &[
            &["1", "第一项"],
            &["2", "第二项"],
            &["3", "第三项"],
            &["4", "第四项"],
            &["5", "第五项"],
        ],
        "7-many-rows",
    );
}

#[test]
fn test_table_8_typical_agent_output() {
    // 8️⃣ 类 agent 输出格式
    assert_table_contains(
        "| 文件 | 改动类型 | 说明 |\n|------|----------|------|\n| `peri-tui/src/kit/markdown/table.rs` | 修改 | 核心渲染逻辑 |\n| `peri-tui/src/kit/markdown/types.rs` | 可能修改 | 新增字段 |\n",
        80,
        &["文件", "改动类型", "说明"],
        &[
            &["peri-tui", "修改", "核心渲染逻辑"],
            &["peri-tui", "可能修改", "新增字段"],
        ],
        "8-agent-output",
    );
}

/// [回归测试] 表格增量缓存 bug：table header 被缓存后，追加数据行时
/// 缓存应失效（全量重跑），而非复用旧 TableData（行数不足）。
///
/// 场景：agent 流式输出表格，第一步缓存了 header+separator（rows=[]），
/// 第二步追加数据行——若缓存不被踢掉，第二步仍使用旧 TableData 渲染，
/// 表现为"表头可见，数据行消失"。
#[test]
fn test_cached_table_rows_grow_correctly() {
    let mut cache = MarkdownRenderCache::default();

    // Step 1: 缓存只有 header+separator 的表格
    let t1 = "| # | 测试场景 | 结果 |\n|---|---------|------|\n";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let table1 = r1.iter().find_map(|s| match s {
        MarkdownSegment::Table(d) => Some(d),
        _ => None,
    });
    assert!(table1.is_some(), "t1 应含 Table segment");
    assert!(
        table1.unwrap().rows.is_empty(),
        "t1 Table rows 应为空（仅有 header+separator，无数据行）"
    );

    // Step 2: 追加数据行
    let t2 = "| # | 测试场景 | 结果 |\n|---|---------|------|\n| 1 | 简单中文表 | ✅ |\n| 2 | 长中文内容 | ✅ |\n";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);

    // 提取 Table 数据
    let table_cached = r2.iter().find_map(|s| match s {
        MarkdownSegment::Table(d) => Some(d),
        _ => None,
    });
    let table_full = full2.iter().find_map(|s| match s {
        MarkdownSegment::Table(d) => Some(d),
        _ => None,
    });

    let cached = table_cached.expect("cached 应含 Table segment");
    let full = table_full.expect("full 应含 Table segment");

    assert_eq!(
        cached.rows.len(),
        full.rows.len(),
        "缓存续跑后 rows 数应与全量一致，actual={}, expected={}",
        cached.rows.len(),
        full.rows.len()
    );
    assert_eq!(
        cached.rows.len(),
        2,
        "应有 2 行数据，actual={}",
        cached.rows.len()
    );
}

/// [回归测试] 流式列表项增量渲染时，尾部 EmptyParagraph 移位导致中间项丢失。
///
/// ratatui-kit-markdown 解析器在列表开始/结束时插入 EmptyParagraph 作为分隔符。
/// 当流式文本逐帧到达时（如 "• A\n" → "• A\n• B\n"），缓存的
/// processed_block_count 包含尾部 EmptyParagraph，下一帧解析时 EmptyParagraph
/// 位置后移，导致新列表项被错误跳过——典型表现为 "B 选项消失"。
///
/// 场景：
///   "- A\n" 解析为 [Empty, ListItem(A), Empty]，processed_block_count=3。
///   "- A\n- B\n" 解析为 [Empty, ListItem(A), ListItem(B), Empty]。
///   缓存复用 → 跳过前 3 block → B（block[2]）被跳过！
#[test]
fn test_cached_list_items_no_disappearing() {
    let mut cache = MarkdownRenderCache::default();

    // Step 1: 一个列表项（以 \n 结尾，触发持久化）
    let t1 = "- A\n";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let text1 = segments_to_text(&r1);
    assert!(text1.contains("A"), "t1 应含 A，实际: {text1:?}");
    // cache 已持久化
    assert!(cache.stable_text_len() > 0, "t1 以 \\n 结尾，应触发持久化");

    // Step 2: 追加第二个列表项（最关键的回归断言）
    let t2 = "- A\n- B\n";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    let text2 = segments_to_text(&r2);
    let full_text2 = segments_to_text(&full2);

    assert!(
        text2.contains("B"),
        "[回归] 续跑后应含 B（EmptyParagraph 移位导致 B 被跳过），实际: {text2:?}"
    );
    assert_eq!(text2, full_text2, "续跑输出应与全量一致");
}

/// [回归测试] 流式场景：表头先到达（无分隔符），分隔符+数据后到达。
///
/// 当表头 `| a | b |\n` 先单独到达时，pulldown-cmark 将其识别为 Paragraph
/// （而非 Table，因为没有分隔符行）。增量缓存持久化此状态。
/// 后续分隔符+数据到达后，同一文本前缀的 block 类型从 [Paragraph] 变为 [Table]，
/// 但 cached processed_block_count=1 导致 Table block 被跳过。
/// 结果：表格永远以原始 pipe 格式显示。
#[test]
fn test_cached_table_header_streamed_before_separator() {
    let mut cache = MarkdownRenderCache::default();

    // Step 1: 表头单独到达（无分隔符），pulldown-cmark → Paragraph
    let t1 = "| a | b |\n";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    // 验证：t1 被解析为 Text（Paragraph），不是 Table
    let has_table_t1 = r1.iter().any(|s| matches!(s, MarkdownSegment::Table(_)));
    assert!(
        !has_table_t1,
        "t1（仅表头）不应被解析为 Table，应为 Paragraph"
    );

    // Step 2: 分隔符 + 数据行到达，此时完整表格应被识别为 Table
    let t2 = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);

    // 断言：续跑后应解析出 Table segment
    let has_table_r2 = r2.iter().any(|s| matches!(s, MarkdownSegment::Table(_)));
    assert!(
        has_table_r2,
        "续跑后应解析出 Table segment，但仅得到原始文本"
    );

    // 断言：续跑输出应与全量一致
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "表头先于分隔符到达的续跑输出应与全量一致"
    );
}

// ── wrap_styled_line（§6.2 竖线连续性：convert 阶段折行）──────────────

fn line_text(line: &ratatui::text::Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 超宽 ASCII 行按宽度折为多行，每行 ≤ max_width，不丢内容。
#[test]
fn test_wrap_styled_line_ascii() {
    let line = ratatui::text::Line::from("word ".repeat(40).trim().to_string());
    let rows = wrap_styled_line(&line, 30);
    assert!(rows.len() > 1, "应折为多行");
    for r in &rows {
        assert!(r.width() <= 30, "行宽 {} 超限: {}", r.width(), line_text(r));
    }
    let joined: String = rows.iter().map(line_text).collect::<Vec<_>>().join("");
    assert_eq!(
        joined,
        "word ".repeat(40).trim().to_string(),
        "折行不丢内容"
    );
}

/// CJK 双宽字符按 display width 折行，汉字不被从中间切开。
#[test]
fn test_wrap_styled_line_cjk() {
    let line = ratatui::text::Line::from("测试文本".repeat(20)); // 每个字符宽 2
    let rows = wrap_styled_line(&line, 22); // 奇数宽度边界
    assert!(rows.len() > 1);
    for r in &rows {
        assert!(r.width() <= 22, "行宽 {} 超限", r.width());
    }
    let joined: String = rows.iter().map(line_text).collect::<Vec<_>>().join("");
    assert_eq!(joined, "测试文本".repeat(20), "CJK 折行不丢内容");
}

/// 折行保留 span 样式（行内 `**bold**` 在折行后仍是 bold）。
#[test]
fn test_wrap_styled_line_keeps_styles() {
    use ratatui::text::Span;
    let line = Line::from(vec![
        Span::styled(
            "前缀 ".repeat(10),
            ratatui::style::Style::default().fg(Color::Red),
        ),
        Span::styled(
            "BOLD".repeat(20),
            ratatui::style::Style::default().add_modifier(Modifier::BOLD),
        ),
    ]);
    let rows = wrap_styled_line(&line, 30);
    assert!(rows.len() > 1);
    let has_bold = rows.iter().any(|r| {
        r.spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
    });
    assert!(has_bold, "折行后应保留 bold 样式");
    // 内容不丢（按 grapheme 拼接）
    let joined: String = rows
        .iter()
        .flat_map(|r| r.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(joined.len(), line_text(&line).len());
}

/// max_width == 0（极端窄屏防御）与不超宽行：原样返回。
#[test]
fn test_wrap_styled_line_zero_width_and_short() {
    let short = ratatui::text::Line::from("abc");
    assert_eq!(wrap_styled_line(&short, 10).len(), 1);
    let rows = wrap_styled_line(&short, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(line_text(&rows[0]), "abc");
}

/// 单个 grapheme 即超宽（如极窄屏下的 CJK 字符）：独占一行不丢内容。
#[test]
fn test_wrap_styled_line_single_wide_grapheme() {
    let line = ratatui::text::Line::from("汉");
    let rows = wrap_styled_line(&line, 1);
    assert_eq!(rows.len(), 1, "超宽 grapheme 独占一行");
    assert_eq!(line_text(&rows[0]), "汉");
}
