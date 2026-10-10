use super::*;

// ── Reasoning 三态（§6.3）──────────────────────────────────────────────

/// Running：`│ Thinking…` + elapsed + ≤4 行 tail；空 reasoning 仍出 header。
#[test]
fn test_reasoning_running_header_elapsed_tail() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let reasoning = TuiReasoningBlock {
        text: "t1\nt2\nt3\nt4\nt5\nt6".to_string(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now() - Duration::from_secs(8)),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);

    let header = header_of(&lines);
    assert!(header.contains("Thinking"), "运行中 header 应含 Thinking…");
    assert!(
        header.contains("8s"),
        "运行中 header 应含 elapsed 8s，实际 {header:?}"
    );

    // tail ≤ 4 行（t3..t6），t1/t2 被截掉；header 行同形竖线不计入正文
    let tail_count = body_vline_count(&lines);
    assert_eq!(tail_count, 4, "Preview tail 最多 4 行");

    // running 块尾无空行——tail 即块尾，正文未到不预留间隔空行
    assert!(
        !lines.last().is_some_and(|l| l.spans.is_empty()),
        "running 下 reasoning 块尾不应有空行"
    );

    // 空 reasoning：仍渲染 Thinking… header，不出现空白 block
    let empty = TuiReasoningBlock {
        text: "   ".to_string(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now() - Duration::from_secs(1)),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(empty),
            message_id: None,
            content_hash: 2,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    assert!(
        header_of(&lines).contains("Thinking"),
        "空 reasoning 必出 Thinking…"
    );
}

/// Completed/Collapsed：单行 `│ Thought for 12s · 14 lines`（icon 统一竖线）。
#[test]
fn test_reasoning_completed_single_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let text = (0..14)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning_block(&text)),
            message_id: None,
            content_hash: 3,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    // 单行折叠摘要（无前导/尾部空行——与工具卡片紧凑布局一致）
    assert_eq!(lines.len(), 1);
    let header = header_of(&lines);
    assert!(header.contains("12s"), "应含冻结时长 12s，实际 {header:?}");
    assert!(
        header.contains("14 lines"),
        "应含行数 14 lines，实际 {header:?}"
    );
    assert!(header.contains("\u{2502}"), "首行 icon 统一竖线 │");
}

/// Completed/Expanded：`│` header + muted+italic 正文（无 italic → dim）。
#[test]
fn test_reasoning_expanded_body_style() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let reasoning = TuiReasoningBlock {
        text: "body line".to_string(),
        fold: FoldState::Expanded,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(12_000),
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 4,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    assert!(
        header_of(&lines).contains("\u{2502}"),
        "展开态首行 icon 统一竖线 │"
    );
    let body = nth_nonempty_line(&lines, 1);
    assert!(line_text(&body).contains("body line"), "展开态应渲染正文");
}

/// Completed + duration_ms = None（历史恢复路径 `handle_committed_assistant_text`，
/// 推理时长不可得）：摘要省略时长只显示行数——不渲染「思考了 0 秒」噪音。
#[test]
fn test_reasoning_completed_no_duration_omits_seconds() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let reasoning = TuiReasoningBlock {
        text: "body line".to_string(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("Thought ·") && !header.contains("0s"),
        "无时长降级为 `Thought · 1 line`（不含 0s），实际 {header:?}"
    );
    assert!(
        header.contains("1 lines"),
        "仍显示行数（对齐工具硬编码 lines 口径），实际 {header:?}"
    );
}

/// [Fix §6.3] running + Collapsed（用户 Space 手动折叠）：仅活动状态行
/// （│ Thinking…），不渲染 tail——「隐藏 reasoning 只影响 body；活动状态行
/// 仍需可见」；Space 切换必须有视觉反馈（review F5）。
#[test]
fn test_reasoning_running_collapsed_shows_status_line_only() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let reasoning = TuiReasoningBlock {
        text: "t1\nt2\nt3\nt4".to_string(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now() - Duration::from_secs(3)),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 5,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(header.contains("Thinking"), "活动状态行仍可见");
    assert!(
        header.contains("\u{2502}"),
        "running 首行 icon 统一竖线 │（颜色承担状态感）"
    );
    assert_eq!(
        body_vline_count(&lines),
        0,
        "running+Collapsed 不渲染 tail（与 Preview 的 4 行 tail 有视觉差异）"
    );
}

/// [Fix LOW-6] completed + Preview（用户 Space 从 Collapsed 切到 Preview，§7 表
/// completed 只定义 Collapsed）：映射为单行折叠视觉（无正文即单行；正文只在
/// Expanded 渲染），icon 统一竖线。
#[test]
fn test_reasoning_completed_preview_maps_to_collapsed_symbol() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let reasoning = TuiReasoningBlock {
        text: "body line".to_string(),
        fold: FoldState::Preview,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(12_000),
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 6,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("\u{2502}") && !header.contains("\u{25b8}") && !header.contains("\u{25be}"),
        "completed+Preview 首行 icon 统一竖线 │（无箭头符号），实际 {header:?}"
    );
    assert_eq!(body_vline_count(&lines), 0, "completed+Preview 不渲染正文");
}

/// reasoning Preview tail 高度有界且不折行——wrap 后取最后 ≤4 视觉行（显示尾部
/// 而非逐行 truncate 头部），每条 tail ≤ content_width → 恰好 1 个 visual row。
#[test]
fn test_reasoning_preview_tail_wrap_height_stable() {
    let long_line = "a".repeat(400);
    let reasoning = TuiReasoningBlock {
        text: long_line,
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now()),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );

    let mut failures = Vec::new();
    for term in [40u16, 60, 80, 120] {
        let grid = GridSpec::grid_for(term);
        let lines = vm_to_lines(&vm, &grid);
        // 高度有界：header + 最后 ≤4 视觉行（wrap 后取尾），不随内容长度增长。
        assert_eq!(
            lines.len(),
            5,
            "term={term}: Preview 块高应为 1 header + 4 tail，实际 {}",
            lines.len()
        );
        let tail_count = body_vline_count(&lines);
        assert_eq!(tail_count, 4, "term={term}: Preview tail 最多 4 行");
        // 内容不丢：tail 显示尾部视觉行，无 truncate 省略号（header 行
        // 「Thinking…」的省略号是文案，跳过）。
        for l in lines
            .iter()
            .skip_while(|l| !l.spans.iter().any(|s| !s.content.is_empty()))
            .skip(1)
        {
            let text = line_text(l);
            if text.contains('\u{2502}') {
                assert!(
                    !text.contains('\u{2026}'),
                    "term={term}: tail 不应含省略号（显示尾部），实际 {text:?}"
                );
            }
        }
        let (_, wm) = build_wrap_map(&lines, term);
        for (logical_idx, info) in wm.iter().enumerate() {
            if lines[logical_idx].spans.is_empty() {
                continue;
            }
            let rows = info.visual_end - info.visual_start;
            if rows != 1 {
                failures.push(format!(
                    "term={term}: logical line {logical_idx} has {rows} visual rows, expected 1"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "截断后仍被 Paragraph 换行:\n{}",
        failures.join("\n")
    );
}

/// 长单行 wrap 后 tail 显示**尾部**内容（TAIL 可见、HEAD 不可见）——展示的是
/// 最近视觉行而非 truncate 头部（§6.3）。
#[test]
fn test_reasoning_preview_tail_shows_end_of_long_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let reasoning = TuiReasoningBlock {
        text: format!("HEAD{}TAIL", "a".repeat(296)),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now()),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);

    // content = 74 → 304 字符折 5 个视觉行，取最后 4 行（HEAD 所在第 1 行被挤出）。
    assert_eq!(lines.len(), 5, "块高应为 1 header + 4 tail");
    assert_eq!(body_vline_count(&lines), 4, "Preview tail 应为 4 行");
    let tail_first = line_text(&nth_nonempty_line(&lines, 1));
    let tail_last = line_text(&nth_nonempty_line(&lines, 4));
    assert!(
        tail_last.contains("TAIL"),
        "tail 末尾应含 TAIL（显示尾部），实际 {tail_last:?}"
    );
    assert!(
        !tail_last.contains('\u{2026}'),
        "tail 不应含省略号（内容不丢），实际 {tail_last:?}"
    );
    assert!(
        !tail_first.contains("HEAD"),
        "tail 首行不应含 HEAD（显示尾部非头部），实际 {tail_first:?}"
    );
}

/// 4→5 视觉行跨越后块高恒定 `1 + min(n, 4)`——流式增长不再引起高度跳动（§6.3）。
#[test]
fn test_reasoning_height_stable_across_4_to_5_visual_lines() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    for n in [3usize, 4, 5, 6, 8] {
        let reasoning = TuiReasoningBlock {
            text: "a".repeat(74 * n),
            fold: FoldState::Preview,
            status: EntryStatus::Running,
            is_running: true,
            started_at: Some(Instant::now()),
            duration_ms: None,
        };
        let vm = TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                started_at: None,
                duration_ms: None,
                text: String::new(),
                reasoning: Some(reasoning),
                message_id: None,
                content_hash: 1,
            }
            .into(),
        );
        let lines = vm_to_lines(&vm, &grid);
        assert_eq!(
            lines.len(),
            1 + n.min(4),
            "n={n}: 块高应为 1 + min(n,4)，流式增长超过 4 视觉行后恒定 5"
        );
    }
}

/// 摘要 N = 视觉行总数，与 Expanded 展开行数一致（N 不封顶，正文才 cap 100）。
#[test]
fn test_reasoning_summary_n_equals_expanded_visual_rows() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);

    // 长行场景：300 字符折 5 视觉行 + "bbb" 1 行 = 6 视觉行。
    let reasoning = TuiReasoningBlock {
        text: format!("{}\nbbb", "a".repeat(300)),
        fold: FoldState::Expanded,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(12_000),
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("6 lines"),
        "摘要 N 应为视觉行总数 6，实际 {header:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| !s.content.is_empty()))
            .count(),
        7,
        "N==Expanded 展开行数：1 header + 6 正文"
    );

    // cap 场景：150 短行 → 150 视觉行；N 显示总数（非 min(150,100)），
    // Expanded 正文只渲染前 100 行（§4 边界语义）。
    let text = (0..150)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let reasoning = TuiReasoningBlock {
        text,
        fold: FoldState::Expanded,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(12_000),
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 2,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("150 lines"),
        "摘要 N 应为总数 150（不封顶），实际 {header:?}"
    );
    let nonempty = lines
        .iter()
        .filter(|l| l.spans.iter().any(|s| !s.content.is_empty()))
        .count();
    assert_eq!(nonempty, 101, "1 header + 前 100 正文视觉行");
}

/// 空行过滤三消费方一致：中间空行删除、尾部空行不占 tail 槽位（§1/§2 边界语义）。
#[test]
fn test_reasoning_empty_lines_skipped_consistently() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);

    // 中间空行：raw ["a","","","b"] → 视觉 ["a","b"]。
    let text = "a\n\n\nb\n";
    // completed + Collapsed：摘要 N = 2
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(TuiReasoningBlock {
                text: text.to_string(),
                fold: FoldState::Collapsed,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: Some(12_000),
            }),
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("2 lines"),
        "空行不计入摘要 N，实际 {header:?}"
    );
    // completed + Expanded：1 header + 2 正文
    let reasoning = TuiReasoningBlock {
        text: text.to_string(),
        fold: FoldState::Expanded,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(12_000),
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 2,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| !s.content.is_empty()))
            .count(),
        3,
        "Expanded 应渲染 1 header + 2 正文"
    );
    // running + Preview：tail 2 行
    let reasoning = TuiReasoningBlock {
        text: text.to_string(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now()),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 3,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 3, "running Preview 应为 1 header + 2 tail");
    assert_eq!(body_vline_count(&lines), 2, "空行不渲染为 tail");

    // 尾部空行不占槽位：raw 末尾空元素被过滤 → 最后 4 个视觉行 = t2..t5
    // （t1 被挤出，顺序保持）。
    let text = "t1\nt2\nt3\nt4\nt5\n\n";
    let reasoning = TuiReasoningBlock {
        text: text.to_string(),
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now()),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(reasoning),
            message_id: None,
            content_hash: 4,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 5, "1 header + 4 tail（尾部空行不占槽位）");
    let tail_first = line_text(&nth_nonempty_line(&lines, 1));
    assert!(
        tail_first.contains("t2"),
        "tail 首行应为 t2（t1 被挤出、尾部空行不消耗槽位），实际 {tail_first:?}"
    );
    let tail_last = line_text(&nth_nonempty_line(&lines, 4));
    assert!(
        tail_last.contains("t5"),
        "tail 末尾应含 t5，实际 {tail_last:?}"
    );
}

// ── Slice 1：空 reasoning 占位渲染（§6.3）+ assistant 时长（§6.2）────────

/// §6.3 空 reasoning 占位：running 空块渲染 `│ Thinking…` + elapsed，
/// 无正文时只有 header 行（不出现空白 block）；completed 折叠为单行。
#[test]
fn test_empty_reasoning_placeholder_running_and_completed() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);

    // running 空块（build_bubble_parts 产出形态）
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(TuiReasoningBlock {
                text: String::new(),
                fold: FoldState::Preview,
                status: EntryStatus::Running,
                is_running: true,
                started_at: None,
                duration_ms: None,
            }),
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("Thinking"),
        "空占位块必渲染 Thinking…，实际 {header:?}"
    );
    // 无 tail（空文本）——占位块只有 header 行，不出现空白 body
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| !s.content.is_empty()))
            .count(),
        1,
        "空块应只有 1 个非空行（Thinking header）"
    );

    // completed 空块（折叠 pass 翻转后形态）→ `│ Thought for 0s · 0 lines`
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: Some(TuiReasoningBlock {
                text: String::new(),
                fold: FoldState::Collapsed,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: Some(0),
            }),
            message_id: None,
            content_hash: 2,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("Thought for 0s") || header.contains("Thought for 0 sec"),
        "completed 空块收束为折叠单行，实际 {header:?}"
    );
    assert!(
        header.contains("0 lines"),
        "空 reasoning 摘要 N 应为 0 lines，实际 {header:?}"
    );
}

/// §6.2 完成时长三档（G-Tokens 仅 duration）：Wide 右对齐 / Standard 紧跟 /
/// Compact 隐藏。
#[test]
fn test_assistant_duration_meta_three_breakpoints() {
    crate::i18n::init(Some("en"));
    let mk = |term: u16| {
        let vm = TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: Some(12_400), // 12.4s
                text: "answer text".to_string(),
                reasoning: None,
                message_id: None,
                content_hash: 0,
            }
            .into(),
        );
        vm_to_lines(&vm, &GridSpec::grid_for(term))
    };

    // Wide（120）：正文末行右对齐含 12.4s，行总宽铺满到居中带右缘（line_width）
    let wide = mk(120);
    let last = wide.iter().rev().find(|l| has_body_content(l)).unwrap();
    let text = line_text(last);
    assert!(text.contains("12.4s"), "Wide 应显示 12.4s，实际 {text:?}");
    let g = GridSpec::grid_for(120);
    assert_eq!(
        last.width(),
        g.line_width() as usize,
        "Wide 右对齐铺满到居中带右缘"
    );

    // Standard（80）：duration 也右对齐（不再紧跟正文）
    let std_lines = mk(80);
    let last = std_lines
        .iter()
        .rev()
        .find(|l| has_body_content(l))
        .unwrap();
    let text = line_text(last);
    assert!(
        text.contains("12.4s"),
        "Standard 应显示 12.4s，实际 {text:?}"
    );
    assert!(
        text.trim_end().ends_with("12.4s"),
        "Standard 右对齐在行尾，实际 {text:?}"
    );

    // Compact（48）：隐藏非关键 duration
    let compact = mk(48);
    assert!(
        !all_text(&compact).contains("12.4s"),
        "Compact 应隐藏 duration"
    );
}
