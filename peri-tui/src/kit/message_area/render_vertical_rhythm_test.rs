use super::*;

// ── 垂直节奏（§3.2）────────────────────────────────────────────────────

/// Wide/Standard：长回复正文块分隔由转场台阶承担，user 与 assistant 长正文块
/// 前后不再保留节拍空行；短回复并入过程带（无台阶）→ 上下各 1 空行作分隔；
/// Compact user 只保留尾部空行，assistant 长正文仍保留首尾空行。
/// turn 内 tool 过程 entry 无空行；
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

    // Compact（无转场台阶）：user 正文直接跟在分隔线后，尾部保留空行
    let compact = GridSpec::grid_for(59);
    let user_compact = vm_to_lines(
        &TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new("hi".into())),
        &compact,
    );
    assert!(
        has_body_content(&user_compact[0]),
        "Compact user 首行直接显示正文"
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
