use super::*;

// ── 宽度 1 不 panic（回归）──────────────────────────────────────────────

/// 宽度为 1 时，所有 VM 变体的 vm_to_lines 不应 panic。
#[test]
fn test_vm_to_lines_all_variants_width_1() {
    let empty_bubble = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: None,
            message_id: None,
            content_hash: 42,
        }
        .into(),
    );

    let text_bubble = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: "hello world\n测试内容".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 43,
        }
        .into(),
    );

    let table_bubble = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: "| col1 | col2 |\n|------|------|\n| a    | b    |\n| c    | d    |".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 44,
        }
        .into(),
    );

    let system_note = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "系统通知".to_string(),
        level: TuiNoteLevel::Info,
        content_hash: 45,
    });

    let divider = TuiRenderUnit::TuiDivider(TuiDivider {
        label: None,
        content_hash: 46,
    });

    let collapsed = TuiRenderUnit::TuiCollapsedGroup(TuiCollapsedGroup {
        title: "折叠组标题".to_string(),
        count: 3,
        failed_count: 0,
        view_models: vec![],
        fold: FoldState::Collapsed,
        content_hash: 47,
    });

    let ask_user = TuiRenderUnit::TuiAskUserBlock(TuiAskUserBlock {
        items: vec![],
        is_error: false,
        // [Slice 4 §6.8] 生产路径字段：completed 结果行（无 pending 选项）。
        kind: crate::kit::tui_render_unit::InteractionKind::Permission,
        pending: false,
        verb: "Bash".to_string(),
        question: "Bash wants to run: cargo test".to_string(),
        options: vec![],
        result: Some("Allowed once".to_string()),
        request_id: None,
        owner: None,
        question_ids: vec![],
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 48,
    });

    let todo = TuiRenderUnit::TuiTodoSummary(TuiTodoSummary::new("3/7 tasks".into()));

    let subagent = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        instance_id: "instance-test-agent".to_string(),
        agent_id: "test-agent".to_string(),
        agent_name: "Test Agent".to_string(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Bash",
            "bash test",
            false,
            false,
        ))]),
        collapsed: false,
        is_running: false,
        is_error: false,
        error_reason: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 49,
    });

    let all_variants: Vec<(&str, TuiRenderUnit)> = vec![
        ("空 AssistantBubble", empty_bubble),
        ("文本 AssistantBubble", text_bubble),
        ("表格 AssistantBubble", table_bubble),
        ("SystemNote", system_note),
        ("Divider", divider),
        ("CollapsedGroup", collapsed),
        ("AskUserBlock", ask_user),
        ("TodoSummary", todo),
        ("SubAgentGroup", subagent),
    ];

    for term in [1u16, 2, 3, 5, 12] {
        let grid = GridSpec::grid_for(term);
        for (_label, vm) in &all_variants {
            let _lines = vm_to_lines(vm, &grid);
            // 只要不 panic 就算通过
        }
    }
}

/// build_wrap_map 在宽度为 1 时不应 panic（所有字符折为单独行）。
#[test]
fn test_build_wrap_map_width_1_no_panic() {
    let lines = vec![
        ratatui_kit::ratatui::text::Line::from(
            "这是一段包含中英文 mixed content 的代表性消息，模拟真实对话内容。",
        ),
        ratatui_kit::ratatui::text::Line::from("第二行：包含各种字符 hello world 12345 !@#$%"),
        ratatui_kit::ratatui::text::Line::from(
            "Third line: purely ASCII text for comparison purposes.",
        ),
    ];
    for width in [1u16, 2, 3, 5, 10] {
        let _ = build_wrap_map(&lines, width);
    }
}

/// 空 AssistantBubble（无 text、无 reasoning）返回 0 行——沿用历史契约
/// （避免 total_visual_rows=0 触发 scrollbar underflow）。
#[test]
fn test_empty_assistant_bubble_returns_zero_lines() {
    let vm = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: String::new(),
            reasoning: None,
            message_id: None,
            content_hash: 1,
        }
        .into(),
    );
    let lines = vm_to_lines(&vm, &GridSpec::grid_for(80));
    assert!(lines.is_empty(), "空 AssistantBubble 应返回 0 行");
}

// ── 网格前缀结构（§3.1）───────────────────────────────────────────────

/// 块首行前缀 = [outer 空][accent 符号][gap]；续行前缀 = [outer 空][│][gap]。
#[test]
fn test_prefix_structure_first_and_continuation() {
    let grid = GridSpec::grid_for(80); // Standard: outer=1 accent=1 gap=2 content=74
    let vm = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false));
    let lines = vm_to_lines(&vm, &grid);

    let first = &lines[0];
    assert_eq!(
        first.spans[0].content.as_ref(),
        " ",
        "首行第 1 列 = outer 空 cell"
    );
    assert_eq!(first.spans[1].content.as_ref(), "\u{2713}", "accent 列 = ✓");
    // gap 后 content 列：span 索引 3（outer/符号/gap）
    let content_col = first.spans[..3]
        .iter()
        .map(|s| s.content.width())
        .sum::<usize>();
    assert_eq!(content_col, grid.first_prefix_width());

    // 续行前缀 = [outer 空][│][gap]
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: "line1\nline2".into(),
        ..tool_card("Read", "src/main.rs", false, false)
    });
    let lines = vm_to_lines(&tool, &grid);
    let cont = lines
        .iter()
        .find(|l| l.spans.len() >= 2 && l.spans[1].content.as_ref() == "\u{2502}")
        .expect("展开体应有续行前缀");
    assert_eq!(cont.spans[0].content.as_ref(), " ");
    assert_eq!(cont.spans[1].content.as_ref(), "\u{2502}");
    assert_eq!(
        cont.spans[..3]
            .iter()
            .map(|s| s.content.width())
            .sum::<usize>(),
        grid.cont_prefix_width()
    );
}

/// 用户与 AI 正文左侧竖线分别使用次等色和主题色。
#[test]
fn test_message_line_colors_follow_role_tokens() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;

    let user = vm_to_lines(
        &TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
            "user message".into(),
        )),
        &grid,
    );
    assert_eq!(user[1].spans[1].content.as_ref(), "\u{2502}");
    assert_eq!(user[1].spans[1].style.fg, Some(sem.text.secondary));

    let assistant = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: "assistant message".into(),
                reasoning: None,
                message_id: None,
                content_hash: 1,
            }
            .into(),
        ),
        &grid,
    );
    assert_eq!(assistant[1].spans[1].content.as_ref(), "\u{2502}");
    assert_eq!(assistant[1].spans[1].style.fg, Some(sem.accent));
}

/// Narrow 断点：首行 accent 符号退化为 dim bullet（§11）。
#[test]
fn test_narrow_accent_bullet() {
    let grid = GridSpec::grid_for(30); // Narrow
    let vm = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false));
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(
        lines[0].spans[1].content.as_ref(),
        "\u{b7}",
        "Narrow 首行 accent 应为 bullet"
    );
}

/// 所有 entry 的正文（content 列）起点一致（§3.1：禁止不同 entry 不同正文起点）。
#[test]
fn test_content_column_aligned_across_entries() {
    let grid = GridSpec::grid_for(80);
    let content_col = grid.first_prefix_width();

    let user = vm_to_lines(
        &TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
            "你好世界".into(),
        )),
        &grid,
    );
    let tool = vm_to_lines(
        &TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false)),
        &grid,
    );
    let assistant = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                started_at: None,
                duration_ms: None,
                text: "hello".into(),
                reasoning: None,
                message_id: None,
                content_hash: 5,
            }
            .into(),
        ),
        &grid,
    );

    // user 正文与其余 entry 正文同列（§3.1；无 role label 行）
    let user_body_line = &user[1];
    // 找到第一个非前缀文本 span（前缀 = outer + accent + gap）
    let text_start = |l: &Line<'static>| -> usize {
        let mut col = 0;
        for span in &l.spans {
            if col >= content_col {
                return col;
            }
            col += span.content.width();
        }
        col
    };
    assert_eq!(text_start(user_body_line), content_col, "user 正文起点");
    assert_eq!(text_start(&tool[0]), content_col, "tool 首行起点");
    assert_eq!(text_start(&assistant[1]), content_col, "assistant 正文起点");
}

// ── 垂直节奏（§3.2）────────────────────────────────────────────────────

/// user 与 assistant 正文块前后各 1 空行；turn 内 tool 过程 entry 无空行；
/// 空文本 user（thinking 回传建模）渲染 0 行。
#[test]
fn test_vertical_rhythm_blank_lines() {
    let grid = GridSpec::grid_for(80);

    let user_lines = vm_to_lines(
        &TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new("hi".into())),
        &grid,
    );
    assert!(
        is_rail_blank(&user_lines[0]),
        "user 前应有 1 空行（带竖线前缀，时间线不断链）"
    );
    assert!(
        user_lines.last().is_some_and(is_rail_blank),
        "user 后应有 1 空行（turn 节拍对称且带竖线前缀）"
    );
    let header = header_of(&user_lines);
    assert!(
        header.contains("hi"),
        "正文直接开始（无 role label），实际: {header:?}"
    );
    assert!(
        !header.contains("›") && !header.contains("You"),
        "无 role label 文本，实际: {header:?}"
    );

    let assistant_lines = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                started_at: None,
                duration_ms: None,
                text: "reply".into(),
                reasoning: None,
                message_id: None,
                content_hash: 6,
            }
            .into(),
        ),
        &grid,
    );
    assert!(
        is_rail_blank(&assistant_lines[0]),
        "assistant 正文前应有 1 空行（带竖线前缀，时间线不断链）"
    );
    assert!(
        assistant_lines.last().is_some_and(is_rail_blank),
        "assistant 正文后应有 1 空行（带竖线前缀）"
    );

    // 工具行之间无空行：completed + expanded 展开体（首行 + 输出行）无内部空行
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: "out1\nout2".into(),
        ..tool_card("Read", "src/main.rs", false, false)
    });
    let tool_lines = vm_to_lines(&tool, &grid);
    assert!(
        tool_lines.iter().all(|l| !l.spans.is_empty()),
        "工具块内部不应有空行"
    );
}

/// 连续 tool 卡片保持紧凑：前一张卡片末尾和后一张卡片开头都不是空行。
#[test]
fn test_consecutive_tool_cards_have_no_gap() {
    let grid = GridSpec::grid_for(80);
    let first = vm_to_lines(
        &TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false)),
        &grid,
    );
    let second = vm_to_lines(
        &TuiRenderUnit::TuiToolCard(tool_card("Grep", "needle", false, false)),
        &grid,
    );

    assert!(first.last().is_some_and(|line| !line.spans.is_empty()));
    assert!(second.first().is_some_and(|line| !line.spans.is_empty()));
}

/// 空文本 user（rewind/重放路径的 thinking 回传消息建模为 user role）→ 渲染
/// 0 行——不产生 turn 节拍空行，thinking 底下不出现悬空空行。
#[test]
fn test_empty_user_bubble_renders_zero_lines() {
    let grid = GridSpec::grid_for(80);
    let empty_user = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
        String::new(),
    ));
    let lines = vm_to_lines(&empty_user, &grid);
    assert!(
        lines.is_empty(),
        "空 user 应渲染 0 行，实际 {} 行",
        lines.len()
    );

    // 非空 user：前导空行 + 正文 + 尾部空行（turn 节拍对称）
    let real_user =
        TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new("hi".into()));
    let real_lines = vm_to_lines(&real_user, &grid);
    assert!(
        is_rail_blank(&real_lines[0]),
        "非空 user 前应有 1 空行（带竖线前缀）"
    );
    assert!(
        real_lines.last().is_some_and(is_rail_blank),
        "非空 user 后应有 1 空行（带竖线前缀）"
    );
}
