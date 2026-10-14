use super::*;
use crate::kit::message_area::grid::TextLineSource;

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

/// 前 `n` 个前缀 span 的累计宽度（正文起点列）——`n` 由该行所属区的前缀形态决定
/// （文本区 3 段 / 过程区 4 段：outer / indent 留空 / 标记 / gap）。
fn prefix_col(line: &Line<'static>, n: usize) -> usize {
    line.spans
        .iter()
        .take(n)
        .map(|s| s.content.width())
        .sum::<usize>()
}

/// 过程区前缀 = `[outer 空][indent 留空][标记][gap]`（§3/§4）——首行标记为状态
/// 符号、续行标记为 `│`，标记列同为 col1 的文本竖线位右移 `indent`（col3 0 起）；
/// 文本竖线不再贯穿过程段，转场由台阶行连接（§6）。
#[test]
fn test_prefix_structure_first_and_continuation() {
    let grid = GridSpec::grid_for(80); // Standard: outer=1 accent=1 gap=2 indent=2 content=74
    let vm = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false));
    let lines = vm_to_lines(&vm, &grid);

    let first = &lines[0];
    assert_eq!(
        first.spans[0].content.as_ref(),
        " ",
        "首行第 1 列 = outer 空 cell"
    );
    assert_eq!(
        first.spans[1].content.as_ref(),
        "  ",
        "indent 格留空（col1 竖线位让给转场台阶）"
    );
    assert_eq!(
        first.spans[2].content.as_ref(),
        "\u{2713}",
        "状态符号占左缘标记列（col3 0 起）"
    );
    assert_eq!(first.spans[3].content.as_ref(), "  ", "gap 分隔");
    assert_eq!(
        prefix_col(first, 4),
        grid.process_prefix_width(),
        "首行标签列 = 过程区前缀宽（6，与旧形态同列）"
    );

    // 续行前缀 = [outer 空][indent 留空][│][gap]——与首行标签同列（§5）
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: "line1\nline2".into(),
        ..tool_card("Read", "src/main.rs", false, false)
    });
    let lines = vm_to_lines(&tool, &grid);
    let cont = lines
        .iter()
        .find(|l| {
            l.spans.len() >= 4
                && l.spans[2].content.as_ref() == "\u{2502}"
                && l.spans[3].content.as_ref() == "  "
        })
        .expect("展开体应有续行前缀");
    assert_eq!(cont.spans[0].content.as_ref(), " ");
    assert_eq!(cont.spans[1].content.as_ref(), "  ", "indent 格留空");
    assert_eq!(
        cont.spans[2].content.as_ref(),
        "\u{2502}",
        "续行标记 = 竖线"
    );
    assert_eq!(
        prefix_col(cont, 4),
        grid.process_prefix_width(),
        "续行内容与首行标签同列"
    );
}

/// 过程区首行（标记符号）与续行（`│`）前缀同宽（§5）——wrap map、可视高度与列
/// 映射不因标记字形变化；duration 仍右对齐到带右缘（§8，跳过最右滚动条列）。
/// 转场台阶行（§6）独立成行、宽 4，不占过程区前缀列。
#[test]
fn test_process_prefix_equal_width_and_duration_flush_right() {
    let grid = GridSpec::grid_for(80);
    let tool = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: "out1\nout2".into(),
        ..tool_card("Read", "src/main.rs", false, false)
    });
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, ..) = super::vm_to_lines_cached_with_layout(
        &tool,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::EnterProcess(TextLineSource::User),
    );
    // 入口台阶：` ╰─╮`（col1 `╰` 接上方文本竖线 / col3 `╮` 接过程区标记列）
    assert!(super::is_step_row(&lines[0]), "首行应是入口台阶");
    assert_eq!(line_text(&lines[0]), " \u{2570}\u{2500}\u{256e}");
    assert_eq!(lines[0].width(), 4, "台阶行宽 4（不含内容）");
    let first = &lines[1];
    // 展开态首行标记 = 折叠符（`▾`），落在过程区左缘标记列（0 起 col3）
    assert_eq!(
        prefix_col(first, 2),
        grid.process_marker_col() as usize,
        "标记落在过程区左缘标记列"
    );
    assert_eq!(
        first.spans[2].content.width(),
        1,
        "标记独占 1 格（首行前缀 = outer / indent 留空 / 标记 / gap 四段）"
    );
    let cont = lines
        .iter()
        .find(|l| {
            l.spans.len() >= 3
                && l.spans[2].content.as_ref() == "\u{2502}"
                && l.spans[3].content.as_ref() == "  "
        })
        .expect("展开体应有续行前缀");
    assert_eq!(
        prefix_col(first, 4),
        prefix_col(cont, 4),
        "标记行与续行前缀等宽"
    );
    assert_eq!(prefix_col(first, 4), grid.process_prefix_width());
    // duration 右对齐：整行铺到 line_width（右缘留滚动条列），行尾即 duration。
    assert_eq!(
        first.width(),
        grid.line_width() as usize,
        "右对齐铺满带右缘"
    );
    assert_eq!(
        first.spans.last().unwrap().content.as_ref(),
        "0.4s",
        "duration 落在右缘"
    );
    assert!(line_text(first).contains("Read"), "标签仍在 col6 之后");
}

/// Compact/Narrow（<60 列）过程区前缀回退基准形态（§9）：无缩进、无台阶，
/// 逐列与改动前一致。
#[test]
fn test_compact_and_narrow_process_prefix_falls_back_to_baseline() {
    use super::helpers::{cont_prefix, first_prefix, process_cont_prefix, process_first_prefix};
    let dim = THEME_ATOM.state().read().semantic.text.dim;
    for term in [59u16, 39] {
        let grid = GridSpec::grid_for(term);
        assert_eq!(grid.indent, 0, "term={term}: <60 列不缩进");
        let style = Style::default();
        assert_eq!(
            process_first_prefix(&grid, "\u{2713}", style),
            first_prefix(&grid, "\u{2713}", style),
            "term={term}: 首行前缀恒回退（符号占 accent 列，不缩进）"
        );
        assert_eq!(
            process_cont_prefix(&grid, dim),
            cont_prefix(&grid, dim),
            "term={term}: 续行前缀恒回退"
        );

        let vm = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false));
        let lines = vm_to_lines(&vm, &grid);
        let first = &lines[0];
        assert_eq!(
            first.spans[..3]
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>(),
            first_prefix(&grid, super::helpers::sym().success, style)
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>(),
            "term={term}: 渲染首行前缀 = 基准形态（outer / 符号 / gap）"
        );
        let text = line_text(first);
        assert!(
            !text.contains('\u{2500}') && !super::is_step_row(first),
            "term={term}: 无台阶字形，实际 {text:?}"
        );
        assert_eq!(
            prefix_col(first, 3),
            grid.first_prefix_width(),
            "term={term}: 正文回基准列"
        );
        assert!(
            lines
                .iter()
                .all(|l| l.width() <= grid.first_prefix_width() + grid.content_width()),
            "term={term}: 行宽不超基准前缀 + content 预算"
        );

        // 短回复（<120 字）的过程带判定在 <60 列不改变渲染：无缩进即无过程带，
        // 正文仍取基准前缀（逐列与改动前一致）
        let brief = TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: "reply".into(),
                reasoning: None,
                message_id: None,
                content_hash: 0,
            }
            .into(),
        );
        let brief_lines = vm_to_lines(&brief, &grid);
        let brief_body = brief_lines
            .iter()
            .find(|l| line_text(l).contains("reply"))
            .expect("短回复正文行");
        assert_eq!(
            prefix_col(brief_body, 3),
            grid.first_prefix_width(),
            "term={term}: 短回复正文回基准列（与长回复同列）"
        );
        assert_eq!(
            brief_body.spans[1].content.as_ref(),
            "\u{2502}",
            "term={term}: 短回复仍取基准竖线前缀"
        );

        // 转场形态存在也不落台阶（§9：Compact/Narrow 无缩进即无台阶）——逐列
        // 与改动前一致，仅前缀回退形态。
        let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
        let (transited, ..) = super::vm_to_lines_cached_with_layout(
            &vm,
            &grid,
            &mut cache,
            None,
            false,
            ZoneTransition::EnterProcess(TextLineSource::User),
        );
        assert_eq!(
            transited.len(),
            lines.len(),
            "term={term}: 台阶不占行（无缩进不画台阶）"
        );
        assert!(
            transited.iter().all(|l| !super::is_step_row(l)),
            "term={term}: 回退形态不出现台阶行"
        );
    }
}

/// 用户与 AI 正文左侧竖线分别使用次等色和主题色——长回复竖线在文本区 accent
/// 列，短回复并入过程带后竖线退到标记列，两者同取 `accent`。
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
    assert_eq!(user[0].spans[1].content.as_ref(), "\u{2502}");
    assert_eq!(user[0].spans[1].style.fg, Some(sem.text.secondary));

    // 长回复（≥120 字）留在文本区：竖线在 accent 列（spans[1]）
    let assistant = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: "assistant message ".repeat(10),
                reasoning: None,
                message_id: None,
                content_hash: 1,
            }
            .into(),
        ),
        &grid,
    );
    assert_eq!(assistant[0].spans[1].content.as_ref(), "\u{2502}");
    assert_eq!(assistant[0].spans[1].style.fg, Some(sem.accent));

    // 短回复（<120 字）并入过程带：正文前有 1 空行，竖线在过程区标记列（spans[2]），同色 accent
    let brief = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: "short answer".into(),
                reasoning: None,
                message_id: None,
                content_hash: 2,
            }
            .into(),
        ),
        &grid,
    );
    assert_eq!(brief[1].spans[2].content.as_ref(), "\u{2502}");
    assert_eq!(brief[1].spans[2].style.fg, Some(sem.accent));
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

/// 分层列坐标（§2/§3）：user/AI 长正文留在基准列（col4）；过程区 entry（工具卡、
/// 短回复 AI 正文）起点右移 `indent` 到 col6。
#[test]
fn test_text_zone_baseline_and_process_zone_indent_columns() {
    let grid = GridSpec::grid_for(80);
    let text_col = grid.first_prefix_width();
    let process_col = grid.process_prefix_width();
    assert_eq!((text_col, process_col), (4, 6), "基准列 4 / 过程区列 6");

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
                // 长回复（≥120 字）留在文本区——短回复并入过程带的列位见下方断言
                text: "hello ".repeat(30),
                reasoning: None,
                message_id: None,
                content_hash: 5,
            }
            .into(),
        ),
        &grid,
    );
    let brief = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: "hello".into(),
                reasoning: None,
                message_id: None,
                content_hash: 6,
            }
            .into(),
        ),
        &grid,
    );

    // user / assistant 长正文：文本区前缀 3 段（outer / 竖线 / gap）→ col4
    assert_eq!(prefix_col(&user[0], 3), text_col, "user 正文起点");
    assert!(line_text(&user[0]).contains("你好世界"));
    assert_eq!(
        prefix_col(&assistant[0], 3),
        text_col,
        "assistant 长正文起点"
    );
    assert!(line_text(&assistant[0]).contains("hello"));
    // 工具卡首行：过程区前缀 4 段（outer / indent 留空 / 标记 / gap）→ col6
    assert_eq!(prefix_col(&tool[0], 4), process_col, "tool 首行标签起点");
    assert!(tool[0].spans[4].content.contains("Read"));
    // 短回复正文：同过程区前缀 4 段 → col6（行 0 = 前导空行，行 1 = 正文）
    assert!(is_rail_blank(&brief[0]), "短回复前保留 1 空行");
    assert_eq!(prefix_col(&brief[1], 4), process_col, "短回复正文起点");
    assert!(line_text(&brief[1]).contains("hello"));
}

// ── 垂直节奏（§3.2）────────────────────────────────────────────────────

/// Wide/Standard：长回复正文块分隔由转场台阶承担，user 与 assistant 长正文块
/// 前后不再保留节拍空行；短回复并入过程带（无台阶）→ 上下各 1 空行作分隔；
/// Compact 无台阶，长正文仍保留首尾 1 空行。turn 内 tool 过程 entry 无空行；
/// 空文本 user（thinking 回传建模）渲染 0 行。
#[test]
fn test_vertical_rhythm_blank_lines() {
    let grid = GridSpec::grid_for(80);

    let user_lines = vm_to_lines(
        &TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new("hi".into())),
        &grid,
    );
    assert!(
        !is_blank_separator(&user_lines[0]),
        "Wide 下 user 首行应为正文（台阶承担分隔，不再保留前导空行）"
    );
    assert!(
        !user_lines.last().is_some_and(is_blank_separator),
        "Wide 下 user 末行应为正文（不再保留尾随空行）"
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

    // Wide：长回复（≥120 字）留在文本区 → 前后不保留空行（台阶承担分隔）
    let assistant_lines = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                started_at: None,
                duration_ms: None,
                text: "reply ".repeat(30),
                reasoning: None,
                message_id: None,
                content_hash: 6,
            }
            .into(),
        ),
        &grid,
    );
    assert!(
        !is_blank_separator(&assistant_lines[0]),
        "Wide 下 assistant 长正文前不再保留空行"
    );
    assert!(
        !assistant_lines.last().is_some_and(is_blank_separator),
        "Wide 下 assistant 长正文后不再保留空行"
    );

    // Wide：短回复并入过程带（与过程 entry 之间不画台阶）→ 上下各 1 空行
    let brief_lines = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: "reply".into(),
                reasoning: None,
                message_id: None,
                content_hash: 8,
            }
            .into(),
        ),
        &grid,
    );
    assert!(
        is_rail_blank(&brief_lines[0]),
        "Wide 下短回复前保留 1 空行（过程带前缀）"
    );
    assert!(
        brief_lines.last().is_some_and(is_rail_blank),
        "Wide 下短回复后保留 1 空行（过程带前缀）"
    );
    assert!(
        !is_blank_separator(&brief_lines[1]) && line_text(&brief_lines[1]).contains("reply"),
        "短回复正文在空行之后，实际: {:?}",
        line_text(&brief_lines[1])
    );

    // Compact（无转场台阶）：节拍空行保留，承担正文块分隔
    let compact = GridSpec::grid_for(59);
    let user_compact = vm_to_lines(
        &TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new("hi".into())),
        &compact,
    );
    assert!(
        is_rail_blank(&user_compact[0]),
        "Compact user 前保留 1 空行（带竖线前缀）"
    );
    assert!(
        user_compact.last().is_some_and(is_rail_blank),
        "Compact user 后保留 1 空行（带竖线前缀）"
    );
    let assistant_compact = vm_to_lines(
        &TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                started_at: None,
                duration_ms: None,
                text: "reply".into(),
                reasoning: None,
                message_id: None,
                content_hash: 7,
            }
            .into(),
        ),
        &compact,
    );
    assert!(
        is_rail_blank(&assistant_compact[0]),
        "Compact assistant 正文前保留 1 空行"
    );
    assert!(
        assistant_compact.last().is_some_and(is_rail_blank),
        "Compact assistant 正文后保留 1 空行"
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

    // 非空 user（Wide）：无节拍空行——首行即正文、末行即正文
    let real_user =
        TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new("hi".into()));
    let real_lines = vm_to_lines(&real_user, &grid);
    assert!(
        real_lines.first().is_some_and(has_body_content),
        "Wide 下非空 user 首行即正文（无前导空行）"
    );
    assert!(
        real_lines.last().is_some_and(has_body_content),
        "Wide 下非空 user 末行即正文（无尾随空行）"
    );
}

// ── 转场台阶线色（§6）──────────────────────────────────────────────────

/// 台阶线色随所接文本左侧线条：进入台阶接上方文本 entry（user → `text.secondary` /
/// assistant → `accent` / divider → `text.dim`），退出台阶接下方文本 entry
/// （assistant 正文 → `accent` / divider → `text.dim`），entry 内部
/// （reasoning → 正文）台阶用正文色。
#[test]
fn test_step_line_color_follows_text_line() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let tool = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false));
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();

    let (user_step, ..) = super::vm_to_lines_cached_with_layout(
        &tool,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::EnterProcess(TextLineSource::User),
    );
    assert!(super::is_step_row(&user_step[0]), "首行应是入口台阶");
    assert_eq!(
        user_step[0].spans[0].style.fg,
        Some(sem.text.secondary),
        "进入台阶线色 = user 文本竖线色"
    );

    let (assistant_step, ..) = super::vm_to_lines_cached_with_layout(
        &tool,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::EnterProcess(TextLineSource::Assistant),
    );
    assert_eq!(
        assistant_step[0].spans[0].style.fg,
        Some(sem.accent),
        "进入台阶线色 = assistant 文本竖线色"
    );

    let answer = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            // 长回复（≥120 字）回文本区 → 入口/退出台阶参与转场
            text: "answer ".repeat(20),
            reasoning: None,
            message_id: None,
            content_hash: 9,
        }
        .into(),
    );
    let (exit_step, ..) = super::vm_to_lines_cached_with_layout(
        &answer,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::ExitProcess(TextLineSource::Assistant),
    );
    assert!(super::is_step_row(&exit_step[0]), "首行应是退出台阶");
    assert_eq!(
        exit_step[0].spans[0].style.fg,
        Some(sem.accent),
        "退出台阶线色 = assistant 正文竖线色"
    );

    // entry 内部（reasoning → 长正文）台阶：正文竖线色
    let mixed = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "body ".repeat(30),
            reasoning: Some(TuiReasoningBlock {
                text: "思考".into(),
                fold: FoldState::Collapsed,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: Some(1_000),
            }),
            message_id: None,
            content_hash: 10,
        }
        .into(),
    );
    let (mixed_lines, ..) = super::vm_to_lines_cached_with_layout(
        &mixed,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::None,
    );
    let inner_step = mixed_lines
        .iter()
        .find(|line| super::is_step_row(line))
        .expect("reasoning → 正文间应画内部退出台阶");
    assert_eq!(
        inner_step.spans[0].style.fg,
        Some(sem.accent),
        "内部台阶线色 = 正文竖线色"
    );

    // 短回复（<120 字）正文同属过程区（无 zone 跳变）：不画内部台阶，正文
    // 走过程带前缀（标记列竖线）
    let brief_mixed = TuiRenderUnit::TuiAssistantBubble(
        TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "短答复".into(),
            reasoning: Some(TuiReasoningBlock {
                text: "思考".into(),
                fold: FoldState::Collapsed,
                status: EntryStatus::Completed,
                is_running: false,
                started_at: None,
                duration_ms: Some(1_000),
            }),
            message_id: None,
            content_hash: 12,
        }
        .into(),
    );
    let (brief_mixed_lines, ..) = super::vm_to_lines_cached_with_layout(
        &brief_mixed,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::None,
    );
    assert!(
        !brief_mixed_lines.iter().any(super::is_step_row),
        "短回复不画内部/入口台阶"
    );
    // 按正文文本定位（首尾另有过程带空行，`last()` 会取到尾随空行）
    let brief_body = brief_mixed_lines
        .iter()
        .find(|line| line.spans.iter().any(|s| s.content.contains("短答复")))
        .expect("正文行");
    assert_eq!(
        brief_body.spans[2].content.as_ref(),
        "\u{2502}",
        "正文并入过程带（标记列竖线）"
    );

    // turn 边界 Divider（文本区装饰，`── label ──` 左侧线色 text.dim）：
    // 退出台阶接下方 divider、进入台阶接上方 divider，线色都取 dim。
    let divider = TuiRenderUnit::TuiDivider(TuiDivider {
        label: None,
        content_hash: 11,
    });
    let (divider_exit, ..) = super::vm_to_lines_cached_with_layout(
        &divider,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::ExitProcess(TextLineSource::Divider),
    );
    assert!(
        super::is_step_row(&divider_exit[0]),
        "divider 前应是退出台阶"
    );
    assert_eq!(
        divider_exit[0].spans[0].style.fg,
        Some(sem.text.dim),
        "退出台阶线色 = divider 左侧线色"
    );

    let (divider_enter, ..) = super::vm_to_lines_cached_with_layout(
        &tool,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::EnterProcess(TextLineSource::Divider),
    );
    assert_eq!(
        divider_enter[0].spans[0].style.fg,
        Some(sem.text.dim),
        "进入台阶线色 = 上方 divider 左侧线色"
    );
}

// ── 系统提醒：过程区（§7）──────────────────────────────────────────────

/// 系统提醒属过程区 entry（§7）：入口台阶独立成行、标题列与续行内容同列
/// （§3/§5）；<60 列回退基准形态（§9）——提醒行不留在文本基准列。
#[test]
fn test_system_reminder_renders_in_process_zone() {
    crate::i18n::init(Some("en"));
    let mut reminder = TuiSystemReminder::legacy("first line\nsecond line".into());
    reminder.fold = FoldState::Expanded;
    reminder.recompute_hash();
    let vm = TuiRenderUnit::TuiSystemReminder(reminder);

    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, ..) = super::vm_to_lines_cached_with_layout(
        &vm,
        &grid,
        &mut cache,
        None,
        false,
        ZoneTransition::EnterProcess(TextLineSource::User),
    );
    assert!(super::is_step_row(&lines[0]), "首行应是入口台阶");
    assert_eq!(line_text(&lines[0]), " \u{2570}\u{2500}\u{256e}");
    let first = &lines[1];
    assert_eq!(
        first.spans[2].content.as_ref(),
        super::helpers::sym().expanded,
        "折叠标记占左缘标记列"
    );
    assert_eq!(
        prefix_col(first, 4),
        grid.process_prefix_width(),
        "标题列 = 过程区前缀宽"
    );
    let body = lines
        .iter()
        .find(|line| line_text(line).contains("first line"))
        .expect("展开提醒应显示正文");
    assert_eq!(body.spans[2].content.as_ref(), "\u{2502}", "续行竖线");
    assert_eq!(body.spans[1].content.as_ref(), "  ", "indent 格留空");
    assert_eq!(
        prefix_col(body, 4),
        grid.process_prefix_width(),
        "正文与标题同列"
    );

    // <60 列：前缀回退基准形态（符号回 accent 列、无台阶）——逐列与改动前一致
    let narrow = GridSpec::grid_for(59);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, ..) = super::vm_to_lines_cached_with_layout(
        &vm,
        &narrow,
        &mut cache,
        None,
        false,
        ZoneTransition::EnterProcess(TextLineSource::User),
    );
    let first = &lines[0];
    assert!(
        !super::is_step_row(first),
        "回退形态不插台阶（无缩进即无台阶）"
    );
    assert_eq!(first.spans[0].content.as_ref(), " ", "outer 空列");
    assert_eq!(
        first.spans[1].content.as_ref(),
        super::helpers::sym().expanded,
        "符号回到 accent 列"
    );
    assert_eq!(
        prefix_col(first, 3),
        narrow.first_prefix_width(),
        "正文回基准列"
    );
    let text = line_text(first);
    assert!(!text.contains('\u{2500}'), "无台阶字形，实际 {text:?}");
}

// ── 正文裁剪与右缘元数据（gutter 不入正文预算）──────────────────────────

/// 正文裁剪在 gutter 放置之前执行（`fit_summary_to_content` 调用约定）：
/// summary 宽度落进「旧口径越界、新口径内」的区间时，duration 保持完整并右对齐
/// 带右缘——不得被裁成存根（§6.4/§8）。
///
/// 80 列：`content = 74` / `indent = 2` / 过程区前缀 6 / `line_width = 79`；
/// `content_total = 前缀 6 + label 4 + " " + summary s + duration 4`，
/// `s ∈ {60, 61, 62}` 时 `content_total ∈ {75, 76, 77}` > 74（旧上限）且
/// `used + 2 + 4 ≤ 79`（duration 仍右对齐）。
#[test]
fn test_summary_fit_keeps_duration_intact_and_flush_right() {
    let grid = GridSpec::grid_for(80);
    for summary_len in [60usize, 61, 62] {
        let summary = "x".repeat(summary_len);
        let mut card = tool_card("Read", &summary, false, false);
        card.output_summary.clear(); // 去掉 ` — N lines` 后缀，隔离正文与 duration 两段
        assert_eq!(
            card.completed_duration_ms,
            Some(400),
            "duration span = 0.4s"
        );
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
        let first = &lines[0];
        assert_eq!(
            first.width(),
            grid.line_width() as usize,
            "summary={summary_len}: duration 仍右对齐带右缘"
        );
        assert_eq!(
            first.spans.last().unwrap().content.as_ref(),
            "0.4s",
            "summary={summary_len}: duration 不应被正文裁剪波及"
        );
        let summary_span = first
            .spans
            .iter()
            .find(|span| span.content.starts_with(" xxx"))
            .expect("summary span");
        assert_eq!(
            summary_span.content.as_ref(),
            format!(" {summary}"),
            "summary={summary_len}: 正文预算内不裁剪"
        );
    }
}
