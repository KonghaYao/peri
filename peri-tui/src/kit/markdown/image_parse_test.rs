use super::*;
use unicode_width::UnicodeWidthStr;

const TEST_BASE_FG: Color = Color::White;

fn line_text(line: &ratatui::text::Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

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
/// 流式图片语法序列（S3 §5 F 组）：每步 cached 输出 == 全量输出。
/// F1-F4 未闭合阶段扫描无命中、替换为无操作；F5 闭合瞬间命中并替换——此时
/// placeholder 前缀断裂（旧 stable_text 是 F4 的字面文本）→ 全量重跑兜底，
/// 输出正确性由 cached == full 断言（T2 spec 2.5）。
#[test]
fn test_cached_parse_image_streaming_closure() {
    let mut cache = MarkdownRenderCache::default();
    let steps = ["!", "![alt]", "![alt](", "![alt](url", "![alt](url)"];
    for (i, text) in steps.iter().enumerate() {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i}（{text:?}）cached 应与全量一致"
        );
    }
}

/// 边界 `![alt](\n\n`（图片语法未闭合但段落被空行闭合）：追加 `url)` 后
/// cached == full（S1 §3.3：正确性由 placeholder 前缀断裂全量重跑兜底，
/// 不依赖尾部回滚）。
#[test]
fn test_cached_parse_image_unclosed_blank_line_boundary() {
    let mut cache = MarkdownRenderCache::default();

    let t1 = "![alt](\n\n";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full1 = parse_markdown(t1, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r1),
        segments_to_text(&full1),
        "空行闭合的未闭合图片首次解析应正确"
    );

    let t2 = "![alt](\n\nurl)";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "追加 url) 后 cached 应与全量一致"
    );
}

/// 表格行内图片：占位 token 不破坏 `|` 行首表头检测（convert.rs:185-192）与
/// 表格结构，cached == full（表格走 has_table_in_processed_blocks 全量路径）。
#[test]
fn test_cached_parse_image_in_table() {
    let mut cache = MarkdownRenderCache::default();
    let steps = [
        "| ![a](u) | b |",
        "| ![a](u) | b |\n|---|---|",
        "| ![a](u) | b |\n|---|---|\n| 1 | 2 |",
    ];
    for (i, text) in steps.iter().enumerate() {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} 表格行内图片 cached 应与全量一致"
        );
        assert_eq!(cached.len(), full.len(), "step {i} 段数应一致（含 Table）");
    }
}

/// 列表项内图片：已闭合前缀块稳定，续跑只处理新增项，cached == full（S3 B1/B2）。
/// [P0-1] 追加「输出含降级文案」断言——仅一致性断言在「双方都丢图」时也通过
/// （评审 P0-1 测试盲区）。
#[test]
fn test_cached_parse_image_in_list() {
    let mut cache = MarkdownRenderCache::default();
    let steps = ["- ![a](u)", "- ![a](u)\n- ![b](v)"];
    for (i, text) in steps.iter().enumerate() {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} 列表项内图片 cached 应与全量一致"
        );
        assert!(
            segments_to_text(&cached).contains("[Image: a] (u)"),
            "step {i} 列表项内图片必须渲染降级文案（P0-1，不得丢失）"
        );
    }
}

/// [P0-1 回归] 标题/列表项内图片：token 未还原时 NUL 宽度为 0，标题渲染为
/// 空行、列表项只剩 `• `。修复后必须显示降级文案（span 层替换，不拆段）。
#[test]
fn test_image_in_heading_and_list_item_renders_degraded_text() {
    let heading = parse_markdown("# ![a](u)", 80, Palette::default(), TEST_BASE_FG);
    let text = segments_to_text(&heading);
    assert!(
        text.contains("[Image: a] (u)"),
        "标题内图片应显示降级文案（P0-1），实际: {text:?}"
    );

    let list = parse_markdown("- ![a](u)", 80, Palette::default(), TEST_BASE_FG);
    let text = segments_to_text(&list);
    assert!(
        text.contains("[Image: a] (u)"),
        "列表项内图片应显示降级文案（P0-1），实际: {text:?}"
    );
    assert!(text.contains('•'), "列表项标记应保留，实际: {text:?}");

    // 嵌套上下文（blockquote > list > image，P2-3 同源）。
    let nested = parse_markdown("> - ![a](u)", 80, Palette::default(), TEST_BASE_FG);
    assert!(
        segments_to_text(&nested).contains("[Image: a] (u)"),
        "blockquote 列表项内图片应显示降级文案"
    );
}

/// 代码上下文内的 `![` 不识别为图片（S3 E1-E2）：字面保留、替换无操作，
/// cached == full；代码块外的图片正常识别为 token。
#[test]
fn test_cached_parse_image_in_code_block_not_detected() {
    let mut cache = MarkdownRenderCache::default();

    let t1 = "```\n![a](u)";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full1 = parse_markdown(t1, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r1),
        segments_to_text(&full1),
        "未闭合围栏内图片语法 cached 应与全量一致"
    );

    let t2 = "```\n![a](u)\n```\n\nafter ![b](v)";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "代码块外图片被替换后 cached 应与全量一致"
    );
    // [T3 升级] 代码块外图片已替换为 Image 段降级文案（token 不再字面出现在输出）
    assert!(
        segments_to_text(&r2).contains("[Image: b] (v)"),
        "代码块外的 ![b](v) 应渲染为降级文案 [Image: b] (v)"
    );
    assert!(
        r2.iter().any(|s| matches!(s, MarkdownSegment::Image(_))),
        "代码块外的图片应产生 Image segment"
    );
}

// ── Phase 2c：Image segment 渲染（T3）────────────────────────────────

/// 三式降级文案 + 空 alt（spec §3.6 / §8.1 R4）：
/// `[Image: alt] (url)` / `[Image] (url)` / `[Remote image: alt] (…)` / `[Remote image] (…)`。
#[test]
fn test_image_segment_degraded_text_forms() {
    let cases: &[(&str, &str, bool, bool)] = &[
        // (输入, 期望降级文案, is_remote, standalone)
        ("![a](u)", "[Image: a] (u)", false, true),
        ("![](u)", "[Image] (u)", false, true),
        (
            "![a](https://x.com/i.png)",
            "[Remote image: a] (https://x.com/i.png)",
            true,
            true,
        ),
        (
            "![](https://x.com/i.png)",
            "[Remote image] (https://x.com/i.png)",
            true,
            true,
        ),
        (
            "![a](data:image/png;base64,AA)",
            "[Image: a] (data:image/png;base64,AA)",
            false,
            true,
        ),
    ];
    for (input, expected, is_remote, standalone) in cases {
        let segments = parse_markdown(input, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(segments.len(), 1, "{input:?} 应解析为单个 Image segment");
        let MarkdownSegment::Image(img) = &segments[0] else {
            panic!("{input:?} 应产生 Image segment");
        };
        assert_eq!(img.is_remote, *is_remote, "{input:?} is_remote");
        assert_eq!(img.standalone, *standalone, "{input:?} standalone");
        // 降级行已 wrap（80 宽不会折行）→ 单行
        assert_eq!(img.lines.len(), 1, "{input:?} 降级行数");
        assert_eq!(
            line_text(&img.lines[0]),
            *expected,
            "{input:?} 降级文案三式"
        );
        // 类型化字段：字节区间覆盖完整语法 `![` 到 `)`
        assert_eq!(
            &input[img.byte_start..img.byte_end]
                .chars()
                .next()
                .unwrap()
                .to_string(),
            "!"
        );
        assert!(input[img.byte_start..img.byte_end].ends_with(')'));
        assert_eq!(
            &input[img.byte_start..img.byte_end],
            *input,
            "独占段区间应覆盖整段"
        );
    }
}

/// 行内混排：`before ![a](u) after` → 拆为 [Text, Image, Text] 且**无中间空行**
/// （§3.4-1 行内拆段 + §3.5 间隙规则）。
#[test]
fn test_image_inline_split_no_gap() {
    let segments = parse_markdown("before ![a](u) after", 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(segments.len(), 3, "行内图片应拆为 3 段");
    assert!(matches!(&segments[0], MarkdownSegment::Text(_)));
    assert!(matches!(&segments[2], MarkdownSegment::Text(_)));
    let MarkdownSegment::Image(img) = &segments[1] else {
        panic!("中间段应为 Image");
    };
    assert!(!img.standalone, "行内混排 standalone 应为 false");
    // flatten：三行连续、无空行（空行 = spans.is_empty()）
    let lines = flatten(&segments);
    assert_eq!(lines.len(), 3);
    assert!(
        lines.iter().all(|l| !l.spans.is_empty()),
        "行内拆段不应产生空行"
    );
    assert_eq!(line_text(&lines[0]), "before ");
    assert_eq!(line_text(&lines[1]), "[Image: a] (u)");
    assert_eq!(line_text(&lines[2]), " after");
}

/// 独占段间距：`before\n\n![a](u)\n\nafter` → [Text, Image, Text]；
/// 同段多图 `![a](u) ![b](v)` → **合并为一个** Image 段（P1-1：段内多行、
/// 无空行）——与跨段独立图片区分（见 [`test_image_cross_paragraph_gap`]）。
#[test]
fn test_image_standalone_segments() {
    let segments = parse_markdown(
        "before\n\n![a](u)\n\nafter",
        80,
        Palette::default(),
        TEST_BASE_FG,
    );
    assert_eq!(segments.len(), 3, "应拆为 [Text, Image, Text]");
    assert!(matches!(&segments[0], MarkdownSegment::Text(_)));
    assert!(matches!(&segments[2], MarkdownSegment::Text(_)));
    let MarkdownSegment::Image(img) = &segments[1] else {
        panic!("中间段应为 Image");
    };
    assert!(img.standalone, "独占段落 standalone 应为 true");

    // 同段多图：`![a](u) ![b](v)` → 单个 Image 段（P1-1 合并），两降级行连续
    let multi = parse_markdown("![a](u) ![b](v)", 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(multi.len(), 1, "同段多图应合并为 1 个 Image segment");
    let MarkdownSegment::Image(multi_img) = &multi[0] else {
        panic!("同段多图应为 Image segment");
    };
    assert!(multi_img.standalone, "同段多图 standalone 应为 true");
    let lines: Vec<String> = multi_img.lines.iter().map(line_text).collect();
    assert_eq!(
        lines,
        vec!["[Image: a] (u)", "[Image: b] (v)"],
        "段内多行连续"
    );
}

/// [P1-1 回归] 跨段独立图片 `![a](u)\n\n![b](v)`：两个独立段落 → **两个**
/// 独立 Image 段（不合并）；段间距由 render 层默认 gap 规则补空行
/// （见 render_test.rs `test_image_cross_paragraph_rendering`）。
#[test]
fn test_image_cross_paragraph_keeps_separate_segments() {
    let segments = parse_markdown("![a](u)\n\n![b](v)", 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(segments.len(), 2, "跨段两图应为 2 个独立 Image segment");
    for s in &segments {
        let MarkdownSegment::Image(img) = s else {
            panic!("跨段图片应为 Image segment");
        };
        assert!(img.standalone, "跨段图片 standalone 应为 true");
    }
    let lines: Vec<String> = segments
        .iter()
        .flat_map(|s| match s {
            MarkdownSegment::Image(img) => img.lines.clone(),
            _ => vec![],
        })
        .map(|l| line_text(&l))
        .collect();
    assert_eq!(lines, vec!["[Image: a] (u)", "[Image: b] (v)"]);
}

/// 流式闭合瞬间：`![alt](url` → `![alt](url)` 输出含 Image 段（T2 一致性测试
/// 复用 + T3 补断言，spec §3.6）。
#[test]
fn test_cached_image_streaming_closure_yields_image_segment() {
    let mut cache = MarkdownRenderCache::default();
    let steps = ["![alt](url", "![alt](url)"];
    for (i, text) in steps.iter().enumerate() {
        let cached = parse_markdown_cached(text, 80, Palette::default(), TEST_BASE_FG, &mut cache);
        let full = parse_markdown(text, 80, Palette::default(), TEST_BASE_FG);
        assert_eq!(
            segments_to_text(&cached),
            segments_to_text(&full),
            "step {i} cached 应与全量一致"
        );
        // 未闭合阶段：无 Image 段（字面文本）；闭合瞬间：Image 段出现
        let has_image = cached
            .iter()
            .any(|s| matches!(s, MarkdownSegment::Image(_)));
        assert_eq!(has_image, i == 1, "step {i} Image 段出现时机");
    }
}

/// 代码块内 `![a](u)` → 原样文本，无 Image 段（S3 E1）。
#[test]
fn test_image_inside_code_block_literal() {
    let segments = parse_markdown("```\n![a](u)\n```", 80, Palette::default(), TEST_BASE_FG);
    assert!(
        !segments
            .iter()
            .any(|s| matches!(s, MarkdownSegment::Image(_))),
        "代码块内图片不应产生 Image segment"
    );
    assert!(
        segments_to_text(&segments).contains("![a](u)"),
        "代码块内图片语法应原样显示"
    );
}

/// 表格单元格内图片：降级文案出现在单元格、表格结构完整（spec §3.6，
/// P0 不拆段、span 样式层替换）。
#[test]
fn test_image_in_table_cell() {
    let segments = parse_markdown(
        "| ![a](u) | b |\n|---|---|\n| ![c](v) | d |",
        80,
        Palette::default(),
        TEST_BASE_FG,
    );
    let MarkdownSegment::Table(data) = segments.last().expect("应含 Table segment") else {
        panic!("应解析出 Table segment");
    };
    assert_eq!(data.headers.len(), 2);
    assert_eq!(data.rows.len(), 1);
    // header 单元格 0：token → 降级文案 spans
    let header_text: String = data.headers[0]
        .iter()
        .flat_map(|s| s.content.as_ref().chars())
        .collect();
    assert_eq!(header_text, "[Image: a] (u)", "表头单元格应显示降级文案");
    // 数据行单元格 0 同理
    let cell_text: String = data.rows[0][0]
        .iter()
        .flat_map(|s| s.content.as_ref().chars())
        .collect();
    assert_eq!(cell_text, "[Image: c] (v)", "数据单元格应显示降级文案");
    // 无 token 的单元格不受影响
    assert_eq!(data.headers[1][0].content.as_ref(), "b");
}

/// 降级行 wrap：超宽 url 折行不丢内容，每行 ≤ max_width（TUI-TEXT-001 口径，
/// 与 wrap_styled_line 一致）。
#[test]
fn test_image_degraded_line_wrap() {
    let url = format!("https://example.com/{}", "x".repeat(60));
    let input = format!("![a]({url})");
    let segments = parse_markdown(&input, 20, Palette::default(), TEST_BASE_FG);
    let MarkdownSegment::Image(img) = &segments[0] else {
        panic!("应产生 Image segment");
    };
    assert!(img.lines.len() > 1, "超宽降级行应折行");
    for line in &img.lines {
        let w = line.spans.iter().map(|s| s.content.width()).sum::<usize>();
        assert!(w <= 20, "折行后每行宽应 ≤ max_width，实际 {w}");
    }
    // 折行不丢内容：拼接 == 未折行文案
    let joined: String = img.lines.iter().map(line_text).collect();
    assert_eq!(
        joined,
        format!("[Remote image: a] ({url})"),
        "折行不应丢内容"
    );
}

/// 展示字段安全：控制字符过滤（T5 复用）+ 长度截断（alt ≤ 64、url ≤ 200，截断加 …）。
#[test]
fn test_image_segment_sanitization_and_truncation() {
    // alt 含 NUL 控制字符 → 剥离（sanitize_for_terminal）
    let segments = parse_markdown("![a\u{0}b](u)", 80, Palette::default(), TEST_BASE_FG);
    let MarkdownSegment::Image(img) = &segments[0] else {
        panic!("应产生 Image segment");
    };
    assert_eq!(img.alt, "ab", "alt 控制字符应被剥离");
    assert_eq!(img.title, None, "无 title 应为 None");
    assert_eq!(line_text(&img.lines[0]), "[Image: ab] (u)");

    // title 字段透传（有 title 时）
    let segments = parse_markdown("![a](u \"t\")", 80, Palette::default(), TEST_BASE_FG);
    let MarkdownSegment::Image(img) = &segments[0] else {
        panic!("应产生 Image segment");
    };
    assert_eq!(img.title.as_deref(), Some("t"));

    // alt 超 64 字符 → 截断 + …
    let long_alt = "a".repeat(100);
    let segments = parse_markdown(
        &format!("![{long_alt}](u)"),
        80,
        Palette::default(),
        TEST_BASE_FG,
    );
    let MarkdownSegment::Image(img) = &segments[0] else {
        panic!("应产生 Image segment");
    };
    assert_eq!(img.alt.chars().count(), 65, "alt 截断 = 64 + …");
    assert!(img.alt.ends_with('…'));

    // url 超 200 字符 → 截断 + …（is_remote 仍按原始 url 判定）
    let long_url = format!("https://example.com/{}", "y".repeat(220));
    let segments = parse_markdown(
        &format!("![a]({long_url})"),
        80,
        Palette::default(),
        TEST_BASE_FG,
    );
    let MarkdownSegment::Image(img) = &segments[0] else {
        panic!("应产生 Image segment");
    };
    assert_eq!(img.url.chars().count(), 201, "url 截断 = 200 + …");
    assert!(img.url.ends_with('…'));
    assert!(img.is_remote, "scheme 分类应基于原始 url");
}

/// 流式追加闭合图片块后，后续追加 cached 输出仍完整（含 Image 段）——
/// 图片文本不参与增量续跑，每帧全量重跑（S1 §6.2 / §8.1 R3 正确性优先）。
#[test]
fn test_cached_image_block_append_keeps_image_segment() {
    let mut cache = MarkdownRenderCache::default();
    let t1 = "![a](u)\n\n";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert!(r1.iter().any(|s| matches!(s, MarkdownSegment::Image(_))));

    let t2 = "![a](u)\n\nafter";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "追加后 cached 应与全量一致（Image 段不得丢失）"
    );
    assert!(r2.iter().any(|s| matches!(s, MarkdownSegment::Image(_))));
}

/// 行内图片 + 后续文本追加（已闭合段落续跑）：Image 段不得丢失。
#[test]
fn test_cached_inline_image_append_keeps_image_segment() {
    let mut cache = MarkdownRenderCache::default();
    let t1 = "x ![a](u) y";
    let r1 = parse_markdown_cached(t1, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    assert!(r1.iter().any(|s| matches!(s, MarkdownSegment::Image(_))));

    let t2 = "x ![a](u) y\n\nmore";
    let r2 = parse_markdown_cached(t2, 80, Palette::default(), TEST_BASE_FG, &mut cache);
    let full2 = parse_markdown(t2, 80, Palette::default(), TEST_BASE_FG);
    assert_eq!(
        segments_to_text(&r2),
        segments_to_text(&full2),
        "已闭合行内图片追加后不得丢失"
    );
}

/// [P1-3] reference 图片（有定义）渲染断言：`![a][ref]` + 定义 → url 已从
/// 定义解析，渲染为降级文案（决策 b：与 inline 一致，对用户信息量更大；
/// `ImageInfo::id` 注释已同步，scan.rs）。

#[test]
fn test_image_reference_with_definition_renders_degraded() {
    let src = "![a][ref]\n\n[ref]: https://example.com/x";
    let segments = parse_markdown(src, 80, Palette::default(), TEST_BASE_FG);
    let text = segments_to_text(&segments);
    assert!(
        text.contains("[Remote image: a] (https://example.com/x)"),
        "reference 图片应渲染降级文案（url 来自定义），实际: {text:?}"
    );
}

/// [P2-3] blockquote 内图片渲染断言：`> ![a](u)` 输出降级文案（`> - ![b](v)`
/// 嵌套列表项场景已由 [`test_image_in_heading_and_list_item_renders_degraded_text`]
/// 覆盖；scan 侧命中断言在 scan.rs `scan_blockquote`）。
#[test]
fn test_image_in_blockquote_renders_degraded() {
    let q = parse_markdown("> ![a](u)", 80, Palette::default(), TEST_BASE_FG);
    assert!(
        segments_to_text(&q).contains("[Image: a] (u)"),
        "blockquote 内图片应显示降级文案"
    );
}
