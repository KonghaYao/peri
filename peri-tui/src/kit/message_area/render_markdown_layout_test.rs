use super::semantic_copy_tests::sem_at;
use super::*;

// ── md 复制按钮 ─────────────────────────────────────────────────────────

/// 顶层渲染（render_copy_button=true）时，超过 MD_COPY_MIN_CHARS（400）字符的
/// AssistantBubble 的正文后、尾随空行前追加复制按钮行（右对齐在 content 列右缘）。
#[test]
fn test_assistant_bubble_renders_copy_button() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let vm = assistant_bubble_with_text(&"x".repeat(401), 1);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn, _, _) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let btn = btn.expect("超过 400 字符的 AssistantBubble 应返回按钮布局");
    let btn_line = &lines[btn.logical_idx];
    let text: String = btn_line.spans.iter().map(|s| s.content.as_ref()).collect();
    let btn_width = 2 + crate::i18n::tr("msg-copy-md").width();
    let line_width = grid.first_prefix_width() + grid.content_width();
    assert_eq!(
        text,
        format!("{}{}", " ".repeat(line_width - btn_width), " Copy "),
        "按钮行 = 前导填充空格（右对齐）+ 左右各 1 空格 + i18n 按钮文本"
    );

    assert_eq!(
        btn.logical_idx,
        lines.len() - 2,
        "按钮行位于正文与尾随空行之间"
    );
    assert_eq!(
        btn.x_start,
        (line_width - btn_width) as u16,
        "点击区域 = 反色块本身（右对齐，不含前导填充）"
    );
    assert_eq!(btn.x_end, line_width as u16, "x_end = 行尾");
}

/// 宽度不足时按钮行会折行 → 不渲染按钮（也不返回布局）。
#[test]
fn test_copy_button_hidden_when_narrow() {
    crate::i18n::init(Some("en"));
    let vm = assistant_bubble_with_text(&"x".repeat(401), 2);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn, _, _) =
        super::vm_to_lines_cached(&vm, &GridSpec::grid_for(3), &mut cache, true);

    assert!(btn.is_none(), "宽度不足时不应返回按钮布局");
    let text = all_text(&lines);
    assert!(!text.contains("Copy"), "宽度不足时不应渲染按钮行");
}

/// 空文本不渲染按钮（没有可复制的内容），且返回 0 行。
#[test]
fn test_copy_button_hidden_for_empty_text() {
    let vm = assistant_bubble_with_text("", 3);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn, _, _) =
        super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

    assert!(lines.is_empty());
    assert!(btn.is_none());
}

/// UserBubble 不渲染复制按钮（仅 AI 回复）。
#[test]
fn test_copy_button_hidden_for_user_bubble() {
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble {
        text: "my input".to_string(),
        content_hash: 4,
        reminder: None,
        source: None,
    });
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn, _, _) =
        super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

    let text = all_text(&lines);
    assert!(!text.contains("Copy"), "UserBubble 不应包含复制按钮文本");
    assert!(btn.is_none());
}

/// 嵌套渲染（render_copy_button=false）不渲染复制按钮。
#[test]
fn test_copy_button_hidden_in_nested_render() {
    crate::i18n::init(Some("en"));
    let vm = assistant_bubble_with_text(&"x".repeat(401), 5);
    let lines = vm_to_lines(&vm, &GridSpec::grid_for(80));
    let text = all_text(&lines);
    assert!(!text.contains("Copy"), "嵌套渲染不应包含复制按钮行");
}

/// 短文本（≤400 字符）不渲染复制按钮。
#[test]
fn test_copy_button_hidden_for_short_text() {
    crate::i18n::init(Some("en"));
    for text in ["hello world".to_string(), "x".repeat(400)] {
        let vm = assistant_bubble_with_text(&text, 6);
        let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
        let (lines, btn, _, _) =
            super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

        assert!(btn.is_none(), "≤400 字符不应返回按钮布局");
        assert!(
            !all_text(&lines).contains("Copy"),
            "≤400 字符不应渲染按钮行"
        );
    }
}

// ── md 行宽回归（§6.2 竖线连续性）──────────────────────────────────────

/// [Fix] markdown 段落行必须在 convert 阶段折行到 content 宽度——否则超宽行
/// 到达渲染层后会被视口 Paragraph 二次折行，折出的行丢失 `│` 竖线前缀
/// （左侧竖线被打断）。
#[test]
fn test_assistant_md_paragraph_lines_stay_within_viewport() {
    let grid = GridSpec::grid_for(80); // content = 74，前缀 = 6
    let text = format!("{} 结尾", "word ".repeat(40)); // 200+ 字符超宽行
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text,
            reasoning: None,
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    assert!(!lines.is_empty(), "应有渲染行");
    for (i, l) in lines.iter().enumerate() {
        let w: usize = l.spans.iter().map(|s| s.content.width()).sum();
        assert!(w <= 80, "行 {i} 宽度 {w} 超过视口 80：{:#?}", line_text(l));
    }
    // 除 leading 空行外，每行第 2 个 span 都是竖线前缀
    for (i, l) in lines.iter().enumerate().skip(1) {
        if l.spans.is_empty() {
            continue;
        }
        assert_eq!(
            l.spans[1].content.as_ref(),
            "\u{2502}",
            "行 {i} 缺竖线前缀：{:#?}",
            line_text(l)
        );
    }
}

/// [Fix] 全块类型（heading / list / code / 段落 + 行内样式）超宽行都在
/// convert 阶段折行：每行（前缀 + 内容）≤ 视口宽度，竖线前缀连续。
#[test]
fn test_assistant_md_all_block_types_stay_within_viewport() {
    let grid = GridSpec::grid_for(80);
    let text = [
        format!("# {}", "很长的标题".repeat(30)),
        format!("- {}", "列表项内容".repeat(40)),
        format!("段落正文 {}", "**加粗强调** ".repeat(30)),
        "```".to_string(),
        format!("let long_code = {}", "x".repeat(200)),
        "```".to_string(),
    ]
    .join("\n");
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text,
            reasoning: None,
            message_id: None,
            content_hash: 2,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    assert!(lines.len() > 1, "应有渲染行");
    for (i, l) in lines.iter().enumerate() {
        if l.spans.is_empty() {
            continue; // 段间空行
        }
        let w: usize = l.spans.iter().map(|s| s.content.width()).sum();
        assert!(w <= 80, "行 {i} 宽度 {w} 超过视口 80：{:#?}", line_text(l));
        assert_eq!(
            l.spans[1].content.as_ref(),
            "\u{2502}",
            "行 {i} 缺竖线前缀：{:#?}",
            line_text(l)
        );
    }
    // 折行不丢内容：代码行 x 总数守恒；`**` 强调文本仍完整
    let all = all_text(&lines);
    assert_eq!(all.matches('x').count(), 200, "代码行折行不丢内容");
    assert!(
        all.replace('*', "").contains("加粗强调"),
        "段落折行不丢内容"
    );
}

/// [Fix] 折行后的行语义复制仍剥离 `│ ` gutter——复制文本不含 UI chrome。
#[test]
fn test_semantic_wrapped_md_line_strips_prefix() {
    let grid = GridSpec::grid_for(60); // 窄 content，保证段落折行
    let text = format!("{} 结尾", "word ".repeat(40));
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text,
            reasoning: None,
            message_id: None,
            content_hash: 3,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let sem_lines: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.spans.is_empty())
        .map(|(idx, _l)| sem_at(&vm, idx, &grid).unwrap_or_default())
        .collect();
    let joined = sem_lines.join("");
    assert!(
        joined.starts_with("word word"),
        "语义复制以正文开始（无 `│ ` gutter），实际: {joined:?}"
    );
    assert!(
        joined.ends_with("结尾"),
        "语义复制保留全部内容（折行不丢字），实际: {joined:?}"
    );
    assert!(
        !joined.contains('\u{2502}'),
        "语义复制不含竖线字符，实际: {joined:?}"
    );
}

// ── T3：Image segment 渲染层（│ 前缀 + 段间隙规则）────────────────────

/// 独占图片段：渲染输出带 │ 前缀、独占段前后各有一空行（§3.5 间隙规则）。
#[test]
fn test_image_standalone_segment_rendering() {
    let vm = assistant_bubble_with_text("before\n\n![a](u)\n\nafter", 7);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) =
        super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

    let texts: Vec<String> = lines.iter().map(line_text).collect();
    let img_idx = texts
        .iter()
        .position(|t| t.contains("[Image: a] (u)"))
        .expect("应渲染出降级行 [Image: a] (u)");
    assert!(
        texts[img_idx].contains('\u{2502}'),
        "Image 段行应带 │ 前缀，实际: {:?}",
        texts[img_idx]
    );
    assert!(is_blank_separator(&lines[img_idx - 1]), "独占段前应有空行");
    assert!(is_blank_separator(&lines[img_idx + 1]), "独占段后应有空行");
}

/// 行内图片段：前后无空行（拆段后仍属同一视觉段落，§3.5 行内规则）。
#[test]
fn test_image_inline_segment_no_gap() {
    let vm = assistant_bubble_with_text("before ![a](u) after", 8);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) =
        super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

    let texts: Vec<String> = lines.iter().map(line_text).collect();
    let img_idx = texts
        .iter()
        .position(|t| t.contains("[Image: a] (u)"))
        .expect("应渲染出降级行");
    assert!(
        !is_blank_separator(&lines[img_idx - 1]),
        "行内图片段前不应有空行，实际: {:?}",
        texts[img_idx - 1]
    );
    assert!(
        !is_blank_separator(&lines[img_idx + 1]),
        "行内图片段后不应有空行，实际: {:?}",
        texts[img_idx + 1]
    );
    // 前后文本保留在同一连续流中（无竖线之外的内容丢失）
    assert!(
        texts[img_idx - 1].contains("before "),
        "行内图片前文本应保留"
    );
    assert!(
        texts[img_idx + 1].contains(" after"),
        "行内图片后文本应保留"
    );
}

/// [P1-1 回归] 跨段独立图片 `![a](u)\n\n![b](v)`：两降级行之间必须有空行
/// （§3.5「独占段前后有空行」）——同段多图已在 convert 层合并为单段，
/// 此处 [Image, Image] 跨段走默认 gap 规则。
#[test]
fn test_image_cross_paragraph_rendering() {
    let vm = assistant_bubble_with_text("![a](u)\n\n![b](v)", 7);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) =
        super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

    let texts: Vec<String> = lines.iter().map(line_text).collect();
    let a_idx = texts
        .iter()
        .position(|t| t.contains("[Image: a] (u)"))
        .expect("应渲染出第一张降级行");
    let b_idx = texts
        .iter()
        .position(|t| t.contains("[Image: b] (v)"))
        .expect("应渲染出第二张降级行");
    assert_eq!(
        b_idx - a_idx,
        2,
        "两降级行之间应有恰好 1 空行，实际: {:?}",
        &texts[a_idx..b_idx]
    );
    assert!(
        is_blank_separator(&lines[a_idx + 1]),
        "跨段独立图片之间应有空行（P1-1）"
    );
}

/// [P1-1] 同段多图 `![a](u) ![b](v)`：合并为单段 → 两降级行连续无空行
/// （convert 层合并，render 层默认规则不误加空行）。
#[test]
fn test_image_same_paragraph_multiple_no_gap() {
    let vm = assistant_bubble_with_text("![a](u) ![b](v)", 7);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _, _) =
        super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

    let texts: Vec<String> = lines.iter().map(line_text).collect();
    let a_idx = texts
        .iter()
        .position(|t| t.contains("[Image: a] (u)"))
        .expect("应渲染出第一张降级行");
    let b_idx = texts
        .iter()
        .position(|t| t.contains("[Image: b] (v)"))
        .expect("应渲染出第二张降级行");
    assert_eq!(b_idx - a_idx, 1, "同段多图之间无空行（P1-1）");
}

// ── 终端 resize（宽度变化）───────────────────────────────────────────────

/// 终端 resize 后同一流式消息的 stable chunk 重建：不得 panic。
///
/// 回归背景：宽度变化会清空 `MarkdownLineCache` 并重建全部 stable chunk；
/// 旧 `retain_and_wrap` 在空 lines 的 chunk 上越界索引 → TUI panic
/// （`index out of bounds: the len is 0 but the index is 0`）。
#[test]
fn test_assistant_bubble_resize_rebuilds_stable_chunks_without_panic() {
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // 流式路径（started_at 有值）→ parse_markdown_chunks_cached 冻结 stable chunk。
            started_at: Some(Instant::now()),
            duration_ms: None,
            text: "段落甲\n\n段落乙\n\n段落丙".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 77,
        }
        .into(),
    );

    let mut md_cache = crate::kit::markdown::MarkdownRenderCache::default();
    let mut layout = crate::kit::message_area::vm_cache::MarkdownLineCache::default();

    for term in [80u16, 30, 55, 20] {
        let grid = GridSpec::grid_for(term);
        let (lines, ..) = super::vm_to_lines_cached_with_layout(
            &vm,
            &grid,
            &mut md_cache,
            Some(&mut layout),
            false,
        );

        let (_, stable) = layout
            .stable_overlay()
            .expect("流式 AssistantBubble 应产生 stable overlay");
        let stable_text: String = stable
            .iter()
            .flat_map(|chunk| chunk.iter().map(line_text))
            .collect();
        assert!(
            stable_text.contains("段落甲"),
            "term={term}: resize 后 stable chunk 内容不得丢失，实际 {stable_text:?}"
        );
        assert!(!lines.is_empty(), "term={term}: 渲染行不得为空");
    }
}

/// 无前导竖线的 GFM 表格（合法写法）不得被冻结进 stable chunk。
///
/// stable 渲染路径只处理 `MarkdownSegment::Text`；表格段冻结进 stable 后
/// 既渲染为空（内容丢失），又会在宽度变化时触发空 chunk 越界。
#[test]
fn test_streaming_table_without_leading_pipe_stays_visible() {
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: Some(Instant::now()),
            duration_ms: None,
            text: "甲 | 乙\n--- | ---\n丙 | 丁\n\n表格后的段落".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 78,
        }
        .into(),
    );

    let grid = GridSpec::grid_for(80);
    let mut md_cache = crate::kit::markdown::MarkdownRenderCache::default();
    let mut layout = crate::kit::message_area::vm_cache::MarkdownLineCache::default();
    let (lines, ..) =
        super::vm_to_lines_cached_with_layout(&vm, &grid, &mut md_cache, Some(&mut layout), false);

    let stable_text: String = layout
        .stable_overlay()
        .map(|(_, stable)| {
            stable
                .iter()
                .flat_map(|chunk| chunk.iter().map(line_text))
                .collect::<String>()
        })
        .unwrap_or_default();
    let rendered = format!("{}{}", all_text(&lines), stable_text);
    for cell in ["甲", "乙", "丙", "丁"] {
        assert!(
            rendered.contains(cell),
            "表格单元格 {cell:?} 应可见（stable 冻结判定漏检无前导竖线的表格），实际 {rendered:?}"
        );
    }
}
