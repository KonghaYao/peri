use super::*;

const TEST_BASE_FG: Color = Color::White;

fn segments_to_text(segments: &[MarkdownSegment]) -> String {
    segments
        .iter()
        .flat_map(|s| match s {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => vec![],
            MarkdownSegment::Image(img) => img.lines.clone(),
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
    let mut cache = MarkdownRenderCache::default();
    let input = "long paragraph with multiple words that must wrap at a narrower width\n\n";
    let wide = parse_markdown_cached(input, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert!(cache.stable_text_len() > 0);
    let narrow = parse_markdown_cached(input, 12, Palette::default(), TEST_BASE_FG, &mut cache);
    assert_ne!(narrow, wide);
    assert_eq!(
        narrow,
        parse_markdown(input, 12, Palette::default(), TEST_BASE_FG)
    );
}

#[test]
fn test_cached_parse_invalidates_on_palette_change() {
    let mut cache = MarkdownRenderCache::default();
    let input = "[link](https://example.invalid)\n\n";
    let palette = Palette::default();
    let initial = parse_markdown_cached(input, 80, palette, TEST_BASE_FG, &mut cache);
    assert!(cache.stable_text_len() > 0);
    let mut changed = palette;
    changed.info = Color::Red;
    let recolored = parse_markdown_cached(input, 80, changed, TEST_BASE_FG, &mut cache);
    assert_ne!(recolored, initial);
    assert_eq!(recolored, parse_markdown(input, 80, changed, TEST_BASE_FG));
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

#[test]
fn test_incremental_reference_definition_keeps_reference_region_mutable() {
    let mut cache = MarkdownRenderCache::default();
    let initial = "[foo]\n\n";
    let first =
        parse_markdown_chunks_cached(initial, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert!(first.stable.is_empty());

    let completed = "[foo]\n\n[foo]: https://example.invalid\n";
    let incremental =
        parse_markdown_chunks_cached(completed, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full = parse_markdown(completed, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&incremental.tail),
        segments_to_text(&full),
        "后置 shortcut definition 必须重解析既有 reference region"
    );
}
#[test]
#[serial_test::serial]
fn test_rendered_chunks_reuse_stable_identity_and_only_parse_suffix() {
    use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};

    let mut cache = MarkdownRenderCache::default();
    let first = parse_markdown_chunks_cached(
        "alpha paragraph\n\nmutable",
        80,
        Palette::default(),
        TEST_BASE_FG,
        &mut cache,
    );
    assert_eq!(cache.stable_chunk_count(), 1);
    assert!(cache.stable_parsed_blocks() > 0);
    let stable = first.stable_identities();

    reset_perf_counters();
    let second = parse_markdown_chunks_cached(
        "alpha paragraph\n\nmutable tail grows",
        80,
        Palette::default(),
        TEST_BASE_FG,
        &mut cache,
    );
    let counters = perf_counters();
    assert_eq!(second.stable_identities(), stable);
    assert_eq!(counters.full_parses, 0);
    assert_eq!(
        counters.tail_parsed_bytes,
        "mutable tail grows".len() as u64
    );
}

#[test]
#[serial_test::serial]
fn test_rendered_chunks_fixed_length_more_publications_do_not_reparse_prefix() {
    use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};

    let stable = "stable words\n\n".repeat(128);
    let suffix = "mutable suffix split across publications";
    let mut cache = MarkdownRenderCache::default();
    let mut source = stable.clone();
    let _ = parse_markdown_chunks_cached(&source, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    reset_perf_counters();
    for part in suffix.as_bytes().chunks(3) {
        source.push_str(std::str::from_utf8(part).unwrap());
        let _ =
            parse_markdown_chunks_cached(&source, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    }
    let counters = perf_counters();
    let quadratic_full_prefix = stable.len() as u64 * suffix.len().div_ceil(3) as u64;
    assert_eq!(counters.full_parsed_bytes, 0);
    assert!(counters.tail_parsed_bytes < quadratic_full_prefix / 8);
}

#[test]
fn test_terminal_barrier_matches_full_reference_for_streaming_matrix() {
    let cases = [
        "prose with 中文 and a verylongwordwithoutanybreakpoint\n\nnext",
        "| a | 中文 |\n| - | - |\n| x | y |\n",
        "before\n\n![alt](relative.png)\n\nafter",
        "- first\n  continuation\n- second",
        "```rust\nlet value = 1;\n```\n\nafter",
        "```rust\nlet value = 1;\n// intentionally unclosed",
    ];
    for input in cases {
        for width in [12, 40, 80] {
            for chunk in [1, 2, 7, input.len().max(1)] {
                let mut cache = MarkdownRenderCache::default();
                let mut source = String::new();
                let chars: Vec<char> = input.chars().collect();
                for chars in chars.chunks(chunk) {
                    source.extend(chars);
                    let _ = parse_markdown_chunks_cached(
                        &source,
                        width,
                        Palette::default(),
                        TEST_BASE_FG,
                        &mut cache,
                    );
                }
                let terminal = parse_markdown_terminal(
                    &source,
                    width,
                    Palette::default(),
                    TEST_BASE_FG,
                    &mut cache,
                );
                assert_eq!(
                    terminal.tail,
                    parse_markdown(&source, width, Palette::default(), TEST_BASE_FG),
                    "width={width} chunk={chunk} input={input:?}"
                );
                assert!(terminal.stable.is_empty());
            }
        }
    }
}

#[test]
fn test_unclosed_fence_stays_mutable_until_closed() {
    let mut cache = MarkdownRenderCache::default();
    let first = parse_markdown_chunks_cached(
        "before\n\n```rust\nlet x = 1;",
        80,
        Palette::default(),
        TEST_BASE_FG,
        &mut cache,
    );
    assert_eq!(first.stable.len(), 1);
    let ids = first.stable_identities();
    let second = parse_markdown_chunks_cached(
        "before\n\n```rust\nlet x = 1;\n```\n\nafter",
        80,
        Palette::default(),
        TEST_BASE_FG,
        &mut cache,
    );
    assert_eq!(&second.stable_identities()[..1], &ids);
    assert!(second.stable.len() >= 2);
}

#[test]
#[serial_test::serial]
fn test_width_and_theme_invalidate_rendered_chunks() {
    use crate::kit::acp_bridge::{perf_counters, reset_perf_counters};

    let input = "stable paragraph with 中文\n\nmutable";
    let mut cache = MarkdownRenderCache::default();
    let first =
        parse_markdown_chunks_cached(input, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let first_ids = first.stable_identities();
    reset_perf_counters();
    let resized =
        parse_markdown_chunks_cached(input, 40, Palette::default(), TEST_BASE_FG, &mut cache);
    assert_eq!(perf_counters().tail_parsed_bytes, "mutable".len() as u64);
    assert_ne!(resized.stable_identities(), first_ids);

    let mut palette = Palette::default();
    palette.accent = Color::Red;
    let themed = parse_markdown_chunks_cached(input, 40, palette, TEST_BASE_FG, &mut cache);
    assert_ne!(themed.stable_identities(), resized.stable_identities());
}
