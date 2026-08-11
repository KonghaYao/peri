//! Tests —— [Slice 3] 统一网格 / user entry / 垂直节奏 / 各变体行渲染器。
//!
//! 约定：
//! - 渲染宽度一律用 `GridSpec::grid_for(term)`（content = term-6），wrap_map
//!   以 `term` 为宽构建——行渲染器保证「行宽 ≤ term_width - 1 < term，
//!   不折行」（metadata 右对齐到消息区右缘，§6.4）。
//! - 前缀结构（§3.1）：首行 `[outer 空][accent 符号][gap]`，续行 `[outer 空][│][gap]`。

use super::*;
use crate::kit::message_area::selection::build_wrap_map;
use crate::kit::tui_render_unit::{
    EntryStatus, FoldState, TuiAskUserBlock, TuiAssistantBubble, TuiCollapsedGroup, TuiDivider,
    TuiNoteLevel, TuiReasoningBlock, TuiRenderUnit, TuiSkillPresentation, TuiSubAgentGroup,
    TuiSystemNote, TuiTodoChange, TuiTodoChangeKind, TuiTodoItem, TuiTodoPresentation,
    TuiTodoStatus, TuiTodoSummary, TuiToolCard, TuiToolPresentation, TuiUserBubble,
};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

/// 拼接渲染行全部 span 文本。
fn line_text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// 拼接所有行文本。
fn all_text(lines: &[Line<'static>]) -> String {
    lines.iter().map(line_text).collect::<Vec<_>>().join("\n")
}

/// 首个非空渲染行的文本（跳过 leading 空行）。
fn header_of(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .find(|l| l.spans.iter().any(|s| !s.content.is_empty()))
        .map(line_text)
        .unwrap_or_default()
}

/// 第 n 个非空行（0-based）。
fn nth_nonempty_line(lines: &[Line<'static>], n: usize) -> Line<'static> {
    lines
        .iter()
        .filter(|l| l.spans.iter().any(|s| !s.content.is_empty()))
        .nth(n)
        .cloned()
        .unwrap_or_default()
}

/// running 符号帧判定：unicode 终端 braille 动画帧或 ASCII 降级 `*`（§8.2）。
/// 动画帧由壁钟 tick 推进，测试不依赖具体帧值。
fn is_running_frame(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(
            c,
            '⠋' | '⠙' | '⠹' | '⠸' | '⠼' | '⠴' | '⠦' | '⠧' | '⠇' | '⠏' | '*'
        )
    })
}

/// 构造 reasoning 块（默认 completed + collapsed）。
fn reasoning_block(text: &str) -> TuiReasoningBlock {
    TuiReasoningBlock {
        text: text.to_string(),
        fold: FoldState::Collapsed,
        status: EntryStatus::Completed,
        is_running: false,
        started_at: None,
        duration_ms: Some(12_000),
    }
}

fn tool_card(tool_name: &str, summary: &str, is_error: bool, is_running: bool) -> TuiToolCard {
    TuiToolCard {
        tool_id: format!("tc-{tool_name}"),
        tool_name: tool_name.to_string(),
        input_summary: summary.to_string(),
        output_summary: if is_error {
            "Error: something went wrong".into()
        } else {
            "done".into()
        },
        is_error,
        is_running,
        running_duration_ms: None,
        completed_duration_ms: if is_running { None } else { Some(37) },
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: if is_error {
            FoldState::Expanded
        } else if is_running {
            FoldState::Preview
        } else {
            FoldState::Collapsed
        },
        user_modified: false,
        tool_calls_count: 0,
        content_hash: 1,
    }
}

// ── 宽度 1 不 panic（回归）──────────────────────────────────────────────

/// 宽度为 1 时，所有 VM 变体的 vm_to_lines 不应 panic。
#[test]
fn test_vm_to_lines_all_variants_width_1() {
    let empty_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: None,
        message_id: None,
        content_hash: 42,
    });

    let text_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "hello world\n测试内容".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 43,
    });

    let table_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: "| col1 | col2 |\n|------|------|\n| a    | b    |\n| c    | d    |".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 44,
    });

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
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 48,
    });

    let todo = TuiRenderUnit::TuiTodoSummary(TuiTodoSummary::new("3/7 tasks".into()));

    let subagent = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: None,
        message_id: None,
        content_hash: 1,
    });
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
        &TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
            started_at: None,
            duration_ms: None,
            text: "assistant message".into(),
            reasoning: None,
            message_id: None,
            content_hash: 1,
        }),
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
        &TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: "hello".into(),
            reasoning: None,
            message_id: None,
            content_hash: 5,
        }),
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
    assert!(user_lines[0].spans.is_empty(), "user 前应有 1 空行");
    assert!(
        user_lines.last().is_some_and(|l| l.spans.is_empty()),
        "user 后应有 1 空行（turn 节拍对称）"
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
        &TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
            // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
            started_at: None,
            duration_ms: None,
            text: "reply".into(),
            reasoning: None,
            message_id: None,
            content_hash: 6,
        }),
        &grid,
    );
    assert!(
        assistant_lines[0].spans.is_empty(),
        "assistant 正文前应有 1 空行"
    );
    assert!(
        assistant_lines
            .last()
            .is_some_and(|line| line.spans.is_empty()),
        "assistant 正文后应有 1 空行"
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
    assert!(real_lines[0].spans.is_empty(), "非空 user 前应有 1 空行");
    assert!(
        real_lines.last().is_some_and(|l| l.spans.is_empty()),
        "非空 user 后应有 1 空行"
    );
}

// ── User entry（§6.1）──────────────────────────────────────────────────

/// 长 prompt 最多 6 个视觉行 + `… +N lines`；无全宽背景色。
#[test]
fn test_user_long_prompt_capped_at_six_lines() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let text = (0..12)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(text));
    let lines = vm_to_lines(&vm, &grid);

    // 1 空行 + 6 正文 + 1 `… +6 lines` + 1 尾部空行（无 role label 行）
    assert_eq!(lines.len(), 9, "6 行上限 + 省略行 + 尾部空行，其余截断");
    assert!(
        lines.last().is_some_and(|l| l.spans.is_empty()),
        "尾部空行（turn 节拍对称）"
    );
    let last = line_text(&lines[lines.len() - 2]);
    assert!(
        last.contains("+6 lines"),
        "省略行格式 `… +N lines`，实际 {last:?}"
    );
    assert!(!all_text(&lines).contains("line 9"), "第 7 行起应被截断");

    // 无气泡背景：所有 span bg 均为 None
    for line in &lines {
        for span in &line.spans {
            assert!(span.style.bg.is_none(), "用户消息不应有全宽背景");
        }
    }
    // 不再使用 ❯（§6.1 移除）
    assert!(!all_text(&lines).contains('\u{276f}'));
}

/// slash/@ 局部强调（§6.1）：token 用 accent.user。
#[test]
fn test_user_slash_at_emphasis() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble::new(
        "run /build and ping @matt".into(),
    ));
    let lines = vm_to_lines(&vm, &grid);
    let body = &lines[1];
    let emphasized = body
        .spans
        .iter()
        .filter(|s| s.style.fg == Some(sem.accents.user))
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        emphasized.contains("/build") && emphasized.contains("@matt"),
        "slash/@ token 应局部强调，实际强调内容: {emphasized:?}"
    );
}

// ── Reasoning 三态（§6.3）──────────────────────────────────────────────

/// Running：`◐ Thinking…` + elapsed + ≤4 行 tail；空 reasoning 仍出 header。
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(reasoning),
        message_id: None,
        content_hash: 1,
    });
    let lines = vm_to_lines(&vm, &grid);

    let header = header_of(&lines);
    assert!(header.contains("Thinking"), "运行中 header 应含 Thinking…");
    assert!(
        header.contains("8s"),
        "运行中 header 应含 elapsed 8s，实际 {header:?}"
    );

    // tail ≤ 4 行（t3..t6），t1/t2 被截掉
    let tail_count = lines
        .iter()
        .filter(|l| l.spans.iter().any(|s| s.content.as_ref() == "\u{2502}"))
        .count();
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(empty),
        message_id: None,
        content_hash: 2,
    });
    let lines = vm_to_lines(&vm, &grid);
    assert!(
        header_of(&lines).contains("Thinking"),
        "空 reasoning 必出 Thinking…"
    );
}

/// Completed/Collapsed：单行 `▸ Thought for 12s · 14 lines`。
#[test]
fn test_reasoning_completed_single_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let text = (0..14)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(reasoning_block(&text)),
        message_id: None,
        content_hash: 3,
    });
    let lines = vm_to_lines(&vm, &grid);
    // 单行折叠摘要（无前导/尾部空行——与工具卡片紧凑布局一致）
    assert_eq!(lines.len(), 1);
    let header = header_of(&lines);
    assert!(header.contains("12s"), "应含冻结时长 12s，实际 {header:?}");
    assert!(
        header.contains("14 lines"),
        "应含行数 14 lines，实际 {header:?}"
    );
    assert!(header.contains("\u{25b8}"), "折叠符号 ▸");
}

/// Completed/Expanded：`▾` header + muted+italic 正文（无 italic → dim）。
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(reasoning),
        message_id: None,
        content_hash: 4,
    });
    let lines = vm_to_lines(&vm, &grid);
    assert!(header_of(&lines).contains("\u{25be}"), "展开符号 ▾");
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(reasoning),
        message_id: None,
        content_hash: 1,
    });
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
/// （◐ Thinking…），不渲染 tail——「隐藏 reasoning 只影响 body；活动状态行
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(reasoning),
        message_id: None,
        content_hash: 5,
    });
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(header.contains("Thinking"), "活动状态行仍可见");
    let tail_count = lines
        .iter()
        .filter(|l| l.spans.iter().any(|s| s.content.as_ref() == "\u{2502}"))
        .count();
    assert_eq!(
        tail_count, 0,
        "running+Collapsed 不渲染 tail（与 Preview 的 4 行 tail 有视觉差异）"
    );
}

/// [Fix LOW-6] completed + Preview（用户 Space 从 Collapsed 切到 Preview，§7 表
/// completed 只定义 Collapsed）：映射为单行折叠视觉（▸），不渲染「▾ 但无正文」
/// 的假展开箭头（正文只在 Expanded 渲染）。
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(reasoning),
        message_id: None,
        content_hash: 6,
    });
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("\u{25b8}") && !header.contains("\u{25be}"),
        "completed+Preview 用折叠符号 ▸（非 ▾），实际 {header:?}"
    );
    let tail_count = lines
        .iter()
        .filter(|l| l.spans.iter().any(|s| s.content.as_ref() == "\u{2502}"))
        .count();
    assert_eq!(tail_count, 0, "completed+Preview 不渲染正文");
}

/// reasoning tail 行不折行——每条 tail 恰好 1 个 visual row。
#[test]
fn test_reasoning_truncate_no_wrap() {
    let long_line = "a".repeat(400);
    let reasoning = TuiReasoningBlock {
        text: long_line,
        fold: FoldState::Preview,
        status: EntryStatus::Running,
        is_running: true,
        started_at: Some(Instant::now()),
        duration_ms: None,
    };
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: String::new(),
        reasoning: Some(reasoning),
        message_id: None,
        content_hash: 1,
    });

    let mut failures = Vec::new();
    for term in [40u16, 60, 80, 120] {
        let grid = GridSpec::grid_for(term);
        let lines = vm_to_lines(&vm, &grid);
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

// ── Tool activity 行（§6.4）────────────────────────────────────────────

/// 完成工具：`✓ {Verb} {summary}` 单行；summary 用 muted 暗色（§6.4 不抢 label）。
#[test]
fn test_tool_completed_single_line_with_path_color() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let vm = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false));
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 1, "completed 折叠态 = 单行");
    let header = header_of(&lines);
    assert!(header.contains("Read"), "label 应含工具名，实际 {header:?}");
    assert!(header.contains("src/main.rs"), "summary 应含路径");
    let path_span = lines[0]
        .spans
        .iter()
        .find(|s| s.content.as_ref() == " src/main.rs")
        .expect("路径 span");
    assert_eq!(
        path_span.style.fg,
        Some(sem.text.muted),
        "路径 summary 应使用 muted 暗色（不抢 label）"
    );
}

/// Bash 首行 command summary 用 muted 暗色；展开态显示 `$ command` 行（syntax.command）。
#[test]
fn test_tool_bash_command_and_expanded_dollar_line() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;

    let collapsed = TuiRenderUnit::TuiToolCard(tool_card("Bash", "cargo test", false, false));
    let lines = vm_to_lines(&collapsed, &grid);
    let cmd_span = lines[0]
        .spans
        .iter()
        .find(|s| s.content.as_ref() == " cargo test")
        .expect("command span");
    assert_eq!(
        cmd_span.style.fg,
        Some(sem.text.muted),
        "Bash command summary 应使用 muted 暗色"
    );

    let expanded = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: "test result: ok".into(),
        ..tool_card("Bash", "cargo test", false, false)
    });
    let lines = vm_to_lines(&expanded, &grid);
    let text = all_text(&lines);
    assert!(text.contains("$ cargo test"), "展开态应有 `$ command` 行");
    assert!(text.contains("\u{2500}"), "展开态应有分隔线");
}

/// 错误态：× + 明确错误词（Failed）；错误输出按 ` - Error: ` 拆行、error 色。
#[test]
fn test_tool_error_splits_and_uses_error_color() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let mut card = tool_card("Edit", "render.rs", true, false);
    card.output_summary = "Tool execution failed: Edit - Error: File not found at /x.rs".into();
    card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(card);
    let lines = vm_to_lines(&vm, &grid);

    let header = header_of(&lines);
    assert!(header.contains('\u{d7}'), "错误符号 ×");
    assert!(
        header.contains("Failed"),
        "明确错误词 Failed，实际 {header:?}"
    );

    // [§9.2] ` - Error: ` 分隔符拆成两行：首行工具名 + 次行错误详情
    let joined: Vec<String> = lines.iter().map(line_text).collect();
    let joined = joined.join("\n");
    assert!(
        joined.contains("Tool execution failed: Edit"),
        "错误首行含工具名，实际: {joined:?}"
    );
    assert!(
        joined.contains("- Error: File not found at /x.rs"),
        "错误详情独立成行，实际: {joined:?}"
    );

    // 错误输出行 error 色（前缀竖线 tool 角色色除外）
    for line in lines.iter().skip(1) {
        for span in &line.spans {
            if span.content.trim().is_empty() || span.content == "\u{2502}" {
                continue;
            }
            assert_eq!(
                span.style.fg,
                Some(sem.status.error),
                "错误输出行使用 error 色，实际: {:?}",
                span.content
            );
        }
    }
}

/// duration 三档：Wide/Standard 右对齐到屏幕右缘 / Compact+Narrow 隐藏。
#[test]
fn test_tool_duration_three_tiers() {
    let card = TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, false)); // 37ms

    // Wide（term=120, content=100）：右对齐 → 行末 = 时长，行宽铺满到右缘（term-1）
    let wide = GridSpec::grid_for(120);
    let lines = vm_to_lines(&card, &wide);
    let text = line_text(&lines[0]);
    assert!(
        text.trim_end().ends_with("37ms"),
        "Wide 时长应右对齐在行尾，实际 {text:?}"
    );
    assert_eq!(
        lines[0].width(),
        wide.term_width.saturating_sub(1) as usize,
        "Wide 行应铺满到消息区右缘（跳过滚动条列）"
    );

    // Standard（term=80, content=74）：同样右对齐（不紧跟 summary）
    let std = GridSpec::grid_for(80);
    let lines = vm_to_lines(&card, &std);
    let text = line_text(&lines[0]);
    assert!(
        text.trim_end().ends_with("37ms"),
        "Standard 时长应右对齐在行尾，实际 {text:?}"
    );

    // Compact（term=50）与 Narrow（term=30）：隐藏非关键 duration（§11）
    for term in [50u16, 30] {
        let grid = GridSpec::grid_for(term);
        let lines = vm_to_lines(&card, &grid);
        assert!(
            !line_text(&lines[0]).contains("37ms"),
            "term={term} 应隐藏 duration"
        );
    }
}

/// 运行中工具：braille 动画帧 + 活动行（无输出 dump）。
#[test]
fn test_tool_running_symbol_and_no_output() {
    let grid = GridSpec::grid_for(80);
    let vm = TuiRenderUnit::TuiToolCard(tool_card("Bash", "sleep 6", false, true));
    let lines = vm_to_lines(&vm, &grid);
    assert_eq!(lines.len(), 1, "running 无输出 → 单活动行");
    assert!(
        is_running_frame(&header_of(&lines)),
        "running 符号应为 braille 动画帧（§8.2），实际 {:?}",
        header_of(&lines)
    );
}

/// [Fix F6 §11] Compact/Narrow 断点：tool 展开体最多 2 行（§11「tool summary
/// 最多 2 行」）；Standard 保持 4 行上限（TOOL_OUTPUT_MAX_LINES）。
#[test]
fn test_tool_expanded_output_caps_by_breakpoint() {
    let output = (0..6)
        .map(|i| format!("out line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let expanded = TuiRenderUnit::TuiToolCard(TuiToolCard {
        fold: FoldState::Expanded,
        output_summary: output,
        ..tool_card("Bash", "cargo test", false, false)
    });

    // Standard（80 列）：展开体 ≤4 行（TOOL_OUTPUT_MAX_LINES）
    let std_lines = vm_to_lines(&expanded, &GridSpec::grid_for(80));
    let std_body = std_lines
        .iter()
        .filter(|l| line_text(l).contains("out line"))
        .count();
    assert_eq!(std_body, 4, "Standard 展开体上限 4 行");

    // Compact（50 列）与 Narrow（30 列）：展开体 ≤2 行（§11）
    for term in [50u16, 30] {
        let grid = GridSpec::grid_for(term);
        assert!(
            matches!(
                grid.bp,
                crate::kit::message_area::grid::Breakpoint::Compact
                    | crate::kit::message_area::grid::Breakpoint::Narrow
            ),
            "term={term} 应为 Compact/Narrow"
        );
        let lines = vm_to_lines(&expanded, &grid);
        let body = lines
            .iter()
            .filter(|l| line_text(l).contains("out line"))
            .count();
        assert_eq!(body, 2, "term={term} 展开体最多 2 行");
    }
}

// ── System note（§6.6）────────────────────────────────────────────────

#[test]
fn test_system_note_levels() {
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;

    // Info → divider 线
    let info_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "Context compacted \u{273b} 18k \u{2192} 7k".to_string(),
        level: TuiNoteLevel::Info,
        content_hash: 200,
    });
    let info_lines = vm_to_lines(&info_vm, &grid);
    assert_eq!(info_lines.len(), 1, "Info 单行 divider");
    let text = line_text(&info_lines[0]);
    assert!(text.contains("\u{2500}"), "divider 线，实际 {text:?}");
    assert!(text.contains("Context compacted"), "来源文本可见");

    // Warning → `!` + warning accent 首行
    let warn_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "Model switched to claude-sonnet-4-5".to_string(),
        level: TuiNoteLevel::Warning,
        content_hash: 201,
    });
    let warn_lines = vm_to_lines(&warn_vm, &grid);
    let header = header_of(&warn_lines);
    assert!(header.contains('!'), "warning 符号 !，实际 {header:?}");
    assert!(
        warn_lines[0].spans[1].style.fg == Some(sem.status.warning),
        "warning accent 色"
    );

    // Error → `×` + error accent 首行；正文 muted
    let err_vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "Connection lost \u{b7} retrying in 3s".to_string(),
        level: TuiNoteLevel::Error,
        content_hash: 202,
    });
    let err_lines = vm_to_lines(&err_vm, &grid);
    let header = header_of(&err_lines);
    assert!(header.contains('\u{d7}'), "error 符号 ×，实际 {header:?}");
    assert!(header.contains("retrying in 3s"), "恢复动作文本可见");
}

// ── SubAgent 单行（§6.7）───────────────────────────────────────────────

fn subagent_group(children: im::Vector<TuiRenderUnit>, is_running: bool) -> TuiRenderUnit {
    TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "agent-1".into(),
        agent_name: "Agent explorer".into(),
        view_models: children,
        collapsed: false,
        is_running,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    })
}

/// §6.7 running 子 agent：显示最近 ≤3 个子工具调用行（最新在前）。
/// 工具行为嵌套从属弱化形态（设计文档 §3）：续行竖线 + 2 格缩进 + 无 bold label
/// + dim 符号——与主时间轴 tool activity row（bold primary + 状态色）差异化。
#[test]
fn test_subagent_running_shows_recent_tool_lines() {
    let grid = GridSpec::grid_for(80);
    let children = im::Vector::from(vec![
        TuiRenderUnit::TuiToolCard(tool_card("Read", "file-a.rs", false, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Glob", "src/**/*.rs", false, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Bash", "cargo test", false, false)),
        // 最新子工具仍在运行
        TuiRenderUnit::TuiToolCard(tool_card("Read", "file-d.rs", false, true)),
    ]);
    let running = subagent_group(children, true);
    let lines = vm_to_lines(&running, &grid);

    // 最多 3 行（SUBAGENT_TOOL_LINES），反向取最近工具，最新在前
    assert_eq!(
        lines.len(),
        3,
        "最多显示最近 3 个工具行，实际 {}",
        lines.len()
    );
    let texts: Vec<String> = lines.iter().map(line_text).collect();
    assert!(
        texts[0].contains("file-d.rs") && texts[0].contains("Read"),
        "最新工具在最前，实际 {texts:?}"
    );
    assert!(
        texts[1].contains("cargo test") && texts[1].contains("Shell"),
        "次新工具（Bash → Shell），实际 {texts:?}"
    );
    assert!(texts[2].contains("Glob"), "第三新工具，实际 {texts:?}");
    assert!(
        !texts[0].contains("file-a.rs"),
        "超过 3 个的旧工具不显示，实际 {texts:?}"
    );

    // running 工具行用动画符号（braille 帧 / ASCII *）
    assert!(
        is_running_frame(&header_of(&lines)),
        "running 工具行用动画符号，实际 {:?}",
        texts[0]
    );

    // 首列结构：`[outer 空][│][gap][2 格缩进]`（设计文档 §11 层级结构）——
    // 工具行永远不是独立 entry 首行（不再用 first_prefix），正文起点 = content + 2
    let sem = THEME_ATOM.state().read().semantic;
    let s = &lines[0].spans;
    assert_eq!(s[0].content, " ", "outer 空列");
    assert_eq!(s[1].content, "\u{2502}", "续行竖线");
    assert_eq!(s[2].content, "  ", "gap（80 列 = 2）");
    assert_eq!(s[3].content, "  ", "2 格缩进 SUBAGENT_TOOL_INDENT");
    // 符号 span（符号 + 分隔空格）dim 色（P2 低显著，对照主时间线 status.running）
    assert_eq!(
        s[4].style.fg,
        Some(sem.text.dim),
        "running 符号 fg = text.dim，实际 {:?}",
        s[4].style
    );
    // Verb span 无 BOLD（P2 权重弱化——bold 是主时间线工具的专属锚点）
    let verb = lines[0]
        .spans
        .iter()
        .find(|sp| sp.content == "Read")
        .expect("Verb span");
    assert!(
        !verb.style.add_modifier.contains(Modifier::BOLD),
        "label 无 bold，实际 {:?}",
        verb.style
    );

    // completed 工具行 duration 右对齐在行尾
    assert!(
        texts[1].trim_end().ends_with("37ms"),
        "completed 工具行 duration 在行尾，实际 {:?}",
        texts[1]
    );
}

/// §6.7 running 子 agent 但子工具尚未路由：回退单行 activity 摘要。
#[test]
fn test_subagent_running_no_tools_falls_back_to_summary() {
    let grid = GridSpec::grid_for(80);
    let running = subagent_group(im::Vector::new(), true);
    let lines = vm_to_lines(&running, &grid);
    assert_eq!(lines.len(), 1, "无工具 → 单行摘要");
    let header = header_of(&lines);
    assert!(
        header.contains("Agent explorer"),
        "名称可见，实际 {header:?}"
    );
    assert!(is_running_frame(&header), "running 符号为动画帧");
}

/// Narrow 断点：符号位省略（设计文档 §6 断点表）——`[outer][│][gap=1][2 格缩进]`
/// 后直接是 Verb，无 braille/✓/× 字符；错误信号由错误词与原因行兜底。
#[test]
fn test_subagent_tool_line_narrow_omits_symbol() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(30); // Narrow: gap=1
    let running = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Read",
            "src/main.rs",
            false,
            true,
        ))]),
        true,
    );
    let lines = vm_to_lines(&running, &grid);
    assert_eq!(lines.len(), 1, "1 个工具行，实际 {}", lines.len());
    let s = &lines[0].spans;
    assert_eq!(s[0].content, " ", "outer 空列");
    assert_eq!(s[1].content, "\u{2502}", "续行竖线");
    assert_eq!(s[2].content, " ", "Narrow gap=1");
    assert_eq!(s[3].content, "  ", "2 格缩进 SUBAGENT_TOOL_INDENT");
    let text = line_text(&lines[0]);
    assert!(
        !is_running_frame(&text) && !text.contains('✓') && !text.contains('×'),
        "Narrow 无符号位，实际 {text:?}"
    );
    assert!(
        text.contains("Read") && text.contains("src/main.rs"),
        "Verb + summary 保留，实际 {text:?}"
    );
    // 无 duration（§11 Compact/Narrow 隐藏非关键 duration——place_meta 既有行为）
    assert!(
        !text.trim_end().chars().any(|c| c.is_ascii_digit()),
        "Narrow 无 duration，实际 {text:?}"
    );
}

/// §6.7 running 子 agent 已有失败工具：工具行 + 原因行。
/// error 工具行符号升级 status.error + ` — Failed` 错误词（P3 错误不弱化）；
/// 原因行缩进与工具行同列对齐（设计文档 §5）。
#[test]
fn test_subagent_running_with_failed_tool_shows_reason() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let sem = THEME_ATOM.state().read().semantic;
    let children = im::Vector::from(vec![
        TuiRenderUnit::TuiToolCard(tool_card("Grep", "src", true, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Read", "a.rs", false, true)),
    ]);
    let running = subagent_group(children, true);
    let lines = vm_to_lines(&running, &grid);
    // 行序：最近工具行（Read running）→ error 工具行（Grep）→ 原因行
    assert_eq!(lines.len(), 3, "工具行 ×2 + 原因行，实际 {}", lines.len());
    let texts: Vec<String> = lines.iter().map(line_text).collect();
    assert!(
        texts[2].contains("Error: something went wrong"),
        "失败原因行可见，实际 {texts:?}"
    );

    // error 工具行：× 符号 + ` — Failed` 错误词
    let err_text = line_text(&lines[1]);
    assert!(
        err_text.contains('\u{d7}'),
        "失败工具行用 × 符号，实际 {err_text:?}"
    );
    assert!(
        err_text.contains(" \u{2014} Failed"),
        "error 行含 ` — Failed` 错误词，实际 {err_text:?}"
    );
    // error 符号 span fg = status.error（P3 升级，对照 running/success 的 text.dim）
    let sym_span = lines[1]
        .spans
        .iter()
        .find(|sp| sp.content.contains('\u{d7}'))
        .expect("error 符号 span");
    assert_eq!(
        sym_span.style.fg,
        Some(sem.status.error),
        "error 符号 fg = status.error，实际 {:?}",
        sym_span.style
    );
    // 错误词 span：bold + error 色（§6.4 主时间线同款）
    let failed_span = lines[1]
        .spans
        .iter()
        .find(|sp| sp.content.contains("Failed"))
        .expect("错误词 span");
    assert_eq!(
        failed_span.style.fg,
        Some(sem.status.error),
        "错误词 fg = status.error，实际 {:?}",
        failed_span.style
    );
    assert!(
        failed_span.style.add_modifier.contains(Modifier::BOLD),
        "错误词 bold，实际 {:?}",
        failed_span.style
    );
    // 原因行与工具行同列对齐（`[outer 空][│][gap][2 格缩进]` 前缀）
    assert!(
        texts[2].starts_with(" \u{2502}    "),
        "原因行缩进与工具行对齐，实际 {:?}",
        texts[2]
    );
}

/// failed 子 agent：× + 原因行（缩进与工具行对齐）；completed：✓ + 结果摘要。
#[test]
fn test_subagent_failed_reason_line_and_completed() {
    let grid = GridSpec::grid_for(80);
    let failed = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Grep", "src", true, false,
        ))]),
        false,
    );
    let lines = vm_to_lines(&failed, &grid);
    let text = all_text(&lines);
    assert!(header_of(&lines).contains('\u{d7}'), "failed 符号 ×");
    assert!(
        text.contains("Error: something went wrong"),
        "failed 原因行可见"
    );
    // 原因行（非 running 分支同样走 cont_prefix + 2 格缩进，与工具行同列）
    assert!(
        text.lines()
            .nth(1)
            .unwrap_or("")
            .starts_with(" \u{2502}    "),
        "failed 原因行缩进对齐，实际 {text:?}"
    );

    let completed = subagent_group(
        im::Vector::from(vec![
            TuiRenderUnit::TuiToolCard(tool_card("Read", "a.rs", false, false)),
            TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                started_at: None,
                duration_ms: None,
                text: "Found 8 UI patterns".into(),
                reasoning: None,
                message_id: None,
                content_hash: 7,
            }),
        ]),
        false,
    );
    let lines = vm_to_lines(&completed, &grid);
    let header = header_of(&lines);
    assert!(header.contains('\u{2713}'), "completed 符号 ✓");
    assert!(header.contains("Found 8 UI patterns"), "结果摘要可见");
}

/// derive_subagent_summary 纯函数矩阵：status/tool_count/failed_count/result。
#[test]
fn test_derive_subagent_summary_matrix() {
    let mk = |view_models: im::Vector<TuiRenderUnit>, is_running: bool| {
        SubAgentSummary::derive(&view_models, is_running)
    };
    // 空 children → 全默认
    assert_eq!(mk(im::Vector::new(), true).status, EntryStatus::Running);
    // running → Running，tool_count 统计
    let s = mk(
        im::Vector::from(vec![
            TuiRenderUnit::TuiToolCard(tool_card("Read", "a", false, false)),
            TuiRenderUnit::TuiToolCard(tool_card("Glob", "b", false, false)),
        ]),
        true,
    );
    assert_eq!(s.status, EntryStatus::Running);
    assert_eq!(s.tool_count, 2);
    assert_eq!(s.failed_count, 0);
    // completed + 有 error 子工具 → Error + failed_count + last_error
    let s = mk(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Edit", "c", true, false,
        ))]),
        false,
    );
    assert_eq!(s.status, EntryStatus::Error);
    assert_eq!(s.failed_count, 1);
    assert!(s.last_error.is_some());
    // completed 无 error → Completed + result 取最近文本首行
    let s = mk(
        im::Vector::from(vec![TuiRenderUnit::TuiAssistantBubble(
            TuiAssistantBubble {
                // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
                started_at: None,
                duration_ms: None,
                text: "final answer line".into(),
                reasoning: None,
                message_id: None,
                content_hash: 8,
            },
        )]),
        false,
    );
    assert_eq!(s.status, EntryStatus::Completed);
    assert_eq!(s.result, "final answer line");
}

// ── 分组 / divider / todo（§6.6/§6.7/§6.9/§7）───────────────────────────

/// TuiCollapsedGroup：`▸ {title}`（title 含隐藏数；组后相邻 error → `· N failed`）。
#[test]
fn test_collapsed_group_line() {
    let grid = GridSpec::grid_for(80);
    let mut group = TuiCollapsedGroup {
        title: "Read 3 · Glob 2".into(),
        count: 5,
        failed_count: 0,
        view_models: vec![],
        content_hash: 0,
    };
    group.recompute_hash();
    let lines = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group), &grid);
    assert_eq!(lines.len(), 1);
    let text = line_text(&lines[0]);
    assert!(text.contains("\u{25b8}"), "折叠符号 ▸，实际 {text:?}");
    assert!(text.contains("Read 3 · Glob 2"), "标题含隐藏数");
    assert!(
        !text.contains("failed"),
        "无相邻 error 时无失败后缀，实际 {text:?}"
    );
}

/// [D2] 组后相邻 error 数 >0 → 标题追加 `· N failed`（error 色 span），
/// 与 `+N −M` 计数（diff_change_summary）不混淆。
#[test]
fn test_collapsed_group_line_with_failed_count() {
    let grid = GridSpec::grid_for(80);
    let mut group = TuiCollapsedGroup {
        title: "Read 2".into(),
        count: 2,
        failed_count: 1,
        view_models: vec![],
        content_hash: 0,
    };
    group.recompute_hash();
    let lines = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group), &grid);
    assert_eq!(lines.len(), 1);
    let text = line_text(&lines[0]);
    assert!(text.contains("Read 2"), "标题含隐藏数");
    assert!(text.contains("· 1 failed"), "失败后缀，实际 {text:?}");
    // 失败后缀使用 status.error 色（只染后缀，不染整行）
    let line = &lines[0];
    let last = line.spans.last().expect("末 span 为失败后缀");
    assert!(last.content.contains("failed"), "后缀是独立 span");
    assert_ne!(last.style.fg, None, "失败后缀必须有 error 前景色");

    // 窄屏：title 截断优先于失败后缀（失败数不可被截断吞掉）
    let narrow = GridSpec::grid_for(40);
    let mut group2 = TuiCollapsedGroup {
        title: "Read 100 · Glob 100 · Bash 100".into(),
        count: 300,
        failed_count: 3,
        view_models: vec![],
        content_hash: 0,
    };
    group2.recompute_hash();
    let lines2 = vm_to_lines(&TuiRenderUnit::TuiCollapsedGroup(group2), &narrow);
    let text2 = line_text(&lines2[0]);
    assert!(
        text2.contains("· 3 failed"),
        "窄屏失败数仍可见，实际 {text2:?}"
    );
}

/// Divider：无 label 纯分隔线填满 content 列；有 label 显示 `── label ──`。
#[test]
fn test_divider_lines() {
    let grid = GridSpec::grid_for(80);
    let plain = vm_to_lines(
        &TuiRenderUnit::TuiDivider(TuiDivider {
            label: None,
            content_hash: 0,
        }),
        &grid,
    );
    let text = line_text(&plain[0]);
    assert!(text.contains("\u{2500}"), "divider 线");
    assert!(text.trim().chars().all(|c| c == '\u{2500}' || c == ' '));

    let labeled = vm_to_lines(
        &TuiRenderUnit::TuiDivider(TuiDivider {
            label: Some("Round 3".into()),
            content_hash: 1,
        }),
        &grid,
    );
    let text = line_text(&labeled[0]);
    assert!(text.contains("Round 3"), "label 可见");
}

/// TuiTodoSummary：`◼ {3/7 tasks · Running tests}`。
#[test]
fn test_todo_summary_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let vm = TuiRenderUnit::TuiTodoSummary(TuiTodoSummary::new("3/7 tasks · Running tests".into()));
    let lines = vm_to_lines(&vm, &grid);
    let text = line_text(&lines[0]);
    assert!(text.contains("\u{25fc}"), "todo 符号 ◼");
    assert!(text.contains("3/7 tasks · Running tests"));
}

// ── Skill / Todo 语义卡（保持专属展示）──────────────────────────────────

#[test]
fn skill_card_hides_raw_skill_output() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "skill-1".into(),
        tool_name: "Skill".into(),
        input_summary: "skill: using-superpowers".into(),
        output_summary: "---\nname: using-superpowers\n---\nfull SKILL.md body".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(37),
        diff: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        presentation: TuiToolPresentation::Skill(TuiSkillPresentation {
            name: "using-superpowers".into(),
        }),
        content_hash: 1,
        tool_calls_count: 0,
    };

    let text = all_text(&vm_to_lines(
        &TuiRenderUnit::TuiToolCard(card),
        &GridSpec::grid_for(80),
    ));
    assert!(text.contains("Skill"));
    assert!(text.contains("✓"));
    assert!(text.contains("using-superpowers"));
    assert!(!text.contains("full SKILL.md body"));
    assert!(!text.contains("---"));
}

#[test]
fn todo_card_renders_progress_and_changes_without_raw_output() {
    crate::i18n::init(Some("en"));
    let card = TuiToolCard {
        tool_id: "todo-1".into(),
        tool_name: "TodoWrite".into(),
        input_summary: "todos: 2".into(),
        output_summary: "+[0],[1]".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(37),
        diff: None,
        fold: FoldState::Collapsed,
        user_modified: false,
        presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
            current_items: vec![TuiTodoItem {
                content: "实现语义卡片".into(),
                active_form: None,
                status: TuiTodoStatus::Completed,
            }],
            changes: vec![TuiTodoChange {
                kind: TuiTodoChangeKind::Completed,
                content: "实现语义卡片".into(),
            }],
            is_initial: false,
            completed_count: 1,
            total_count: 1,
        }),
        content_hash: 2,
        tool_calls_count: 0,
    };

    let text = all_text(&vm_to_lines(
        &TuiRenderUnit::TuiToolCard(card),
        &GridSpec::grid_for(80),
    ));
    assert!(text.contains("TodoUpdate"));
    assert!(text.contains("1/1"));
    assert!(text.contains("✓"));
    assert!(text.contains("实现语义卡片"));
    assert!(!text.contains("+[0],[1]"));
}

/// 语义卡在极端窄宽度（Narrow）下不 panic 且不超宽。
#[test]
fn semantic_cards_respect_narrow_widths() {
    crate::i18n::init(Some("en"));
    for width in 1..=8u16 {
        let grid = GridSpec::grid_for(width);
        let card = TuiToolCard {
            tool_id: format!("todo-{width}"),
            tool_name: "TodoWrite".into(),
            input_summary: String::new(),
            output_summary: "+[0]".into(),
            is_error: true,
            is_running: false,
            running_duration_ms: None,
            completed_duration_ms: Some(37),
            diff: None,
            fold: FoldState::Collapsed,
            user_modified: false,
            presentation: TuiToolPresentation::Todo(TuiTodoPresentation {
                current_items: vec![],
                changes: vec![TuiTodoChange {
                    kind: TuiTodoChangeKind::Added,
                    content: "这是一个足够长的任务标题，用于验证窄终端截断行为".into(),
                }],
                is_initial: true,
                completed_count: 0,
                total_count: 1,
            }),
            content_hash: width as u64,
            tool_calls_count: 0,
        };
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
        // 截断省略号允许 +1 列（content 列内放不下的省略号落在 gap 前）
        let max_width = grid.first_prefix_width() + grid.content_width() + 1;
        for line in &lines {
            assert!(
                line.width() <= max_width,
                "term={width}: 行宽 {} 超过 {max_width}",
                line.width()
            );
        }
    }
}

// ── md 复制按钮 ─────────────────────────────────────────────────────────

fn assistant_bubble_with_text(text: &str, hash: u64) -> TuiRenderUnit {
    TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        // [Slice 1] 正文时长（§6.2 `12.4s`）：测试构造默认无起点/冻结值。
        started_at: None,
        duration_ms: None,
        text: text.to_string(),
        reasoning: None,
        message_id: None,
        content_hash: hash,
    })
}

/// 顶层渲染（render_copy_button=true）时，超过 MD_COPY_MIN_CHARS（400）字符的
/// AssistantBubble 的正文后、尾随空行前追加复制按钮行（右对齐在 content 列右缘）。
#[test]
fn test_assistant_bubble_renders_copy_button() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);
    let vm = assistant_bubble_with_text(&"x".repeat(401), 1);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn, _) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

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
    let (lines, btn, _) = super::vm_to_lines_cached(&vm, &GridSpec::grid_for(3), &mut cache, true);

    assert!(btn.is_none(), "宽度不足时不应返回按钮布局");
    let text = all_text(&lines);
    assert!(!text.contains("Copy"), "宽度不足时不应渲染按钮行");
}

/// 空文本不渲染按钮（没有可复制的内容），且返回 0 行。
#[test]
fn test_copy_button_hidden_for_empty_text() {
    let vm = assistant_bubble_with_text("", 3);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, btn, _) = super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

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
    let (lines, btn, _) = super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

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
        let (lines, btn, _) =
            super::vm_to_lines_cached(&vm, &GridSpec::grid_for(80), &mut cache, true);

        assert!(btn.is_none(), "≤400 字符不应返回按钮布局");
        assert!(
            !all_text(&lines).contains("Copy"),
            "≤400 字符不应渲染按钮行"
        );
    }
}

// ── 工具头行后缀（历史行为保留）────────────────────────────────────────

/// Glob/Grep 完成后头行显示 `— N matches`；错误态不显示。
#[test]
fn test_glob_grep_header_match_suffix() {
    crate::i18n::init(Some("zh-CN"));
    let grid = GridSpec::grid_for(120);
    let files: Vec<String> = (0..163)
        .map(|i| format!("/repo/peri-tui/src/kit/file_{i}.rs"))
        .collect();
    let output = files.join("\n");

    for tool_name in ["Glob", "Grep"] {
        let card = TuiToolCard {
            output_summary: output.clone(),
            ..tool_card(
                tool_name,
                r#"pattern: "peri-tui/src/**/*.rs""#,
                false,
                false,
            )
        };
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
        let header = header_of(&lines);
        assert!(
            header.contains("— 163 matches"),
            "{tool_name} 头行应包含 '— 163 matches'，实际: {header:?}"
        );
        assert!(
            header.contains("pattern:"),
            "{tool_name} 头行应包含 pattern 参数，实际: {header:?}"
        );
    }

    // 错误态：头行不得包含匹配数后缀
    for tool_name in ["Glob", "Grep"] {
        let card = tool_card(tool_name, "pattern: x", true, false);
        let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
        let header = header_of(&lines);
        assert!(
            !header.contains("matches"),
            "{tool_name} 错误态头行不应包含 'matches'，实际: {header:?}"
        );
    }
}

/// Wide 右对齐 meta 不被 fit_summary_to_content 误截（回归：Wide 定位填充
/// 计入总宽导致 subagent tool count 被截成 `N 次…`）。
#[test]
fn test_wide_aligned_meta_not_truncated_by_fit() {
    // subagent 行：名称 + 长 activity + 右对齐 tool count（zh 格式 `N 次工具`）
    let group = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "a1".into(),
        agent_name: "general-purpose".into(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Bash",
            "echo hello-subagent-internal",
            false,
            false,
        ))]),
        collapsed: false,
        is_running: false,
        fold: FoldState::Collapsed,
        user_modified: false,
        content_hash: 0,
    });
    let wide = GridSpec::grid_for(120);
    let lines = vm_to_lines(&group, &wide);
    let text = line_text(&lines[0]);
    assert!(
        text.contains("1 \u{6b21}\u{5de5}\u{5177}") || text.contains("1 tools"),
        "Wide 下 tool count 不应被截断，实际 {text:?}"
    );
    // 行宽 = 消息区右缘（term_width - 1，右对齐铺满）
    assert_eq!(lines[0].width(), wide.term_width.saturating_sub(1) as usize);
}

// ── Slice 1：空 reasoning 占位渲染（§6.3）+ assistant 时长（§6.2）────────

/// §6.3 空 reasoning 占位：running 空块渲染 `◐ Thinking…` + elapsed，
/// 无正文时只有 header 行（不出现空白 block）；completed 折叠为单行。
#[test]
fn test_empty_reasoning_placeholder_running_and_completed() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(80);

    // running 空块（build_bubble_parts 产出形态）
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
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
    });
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

    // completed 空块（折叠 pass 翻转后形态）→ `▸ Thought for 0s · 0 lines`
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
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
    });
    let lines = vm_to_lines(&vm, &grid);
    let header = header_of(&lines);
    assert!(
        header.contains("Thought for 0s") || header.contains("Thought for 0 sec"),
        "completed 空块收束为折叠单行，实际 {header:?}"
    );
}

/// §6.2 完成时长三档（G-Tokens 仅 duration）：Wide 右对齐 / Standard 紧跟 /
/// Compact 隐藏。
#[test]
fn test_assistant_duration_meta_three_breakpoints() {
    crate::i18n::init(Some("en"));
    let mk = |term: u16| {
        let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
            started_at: None,
            duration_ms: Some(12_400), // 12.4s
            text: "answer text".to_string(),
            reasoning: None,
            message_id: None,
            content_hash: 0,
        });
        vm_to_lines(&vm, &GridSpec::grid_for(term))
    };

    // Wide（120）：正文末行右对齐含 12.4s，行总宽铺满到消息区右缘（term-1）
    let wide = mk(120);
    let last = wide.iter().rev().find(|l| !l.spans.is_empty()).unwrap();
    let text = line_text(last);
    assert!(text.contains("12.4s"), "Wide 应显示 12.4s，实际 {text:?}");
    let g = GridSpec::grid_for(120);
    assert_eq!(
        last.width(),
        g.term_width.saturating_sub(1) as usize,
        "Wide 右对齐铺满到消息区右缘"
    );

    // Standard（80）：duration 也右对齐（不再紧跟正文）
    let std_lines = mk(80);
    let last = std_lines
        .iter()
        .rev()
        .find(|l| !l.spans.is_empty())
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

// ── [Slice 4 §6.8] Interaction block 渲染 ──

fn pending_permission_block() -> TuiAskUserBlock {
    let mut b = TuiAskUserBlock {
        items: vec![],
        is_error: false,
        kind: crate::kit::tui_render_unit::InteractionKind::Permission,
        pending: true,
        verb: "Bash".into(),
        question: "Bash wants to run: cargo test".into(),
        options: vec!["Allow once".into(), "Deny".into()],
        result: None,
        request_id: Some("rid-1".into()),
        fold: FoldState::Expanded,
        user_modified: false,
        content_hash: 0,
    };
    b.recompute_hash();
    b
}

fn completed_block(result: &str) -> TuiAskUserBlock {
    let mut b = pending_permission_block();
    b.pending = false;
    b.result = Some(result.to_string());
    b.fold = FoldState::Collapsed;
    b.recompute_hash();
    b
}

/// pending 态：标题（`Approval required`）+ 问题摘要 + 横向选项行 + 布局信息。
#[test]
fn test_interaction_pending_permission_layout() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(pending_permission_block());
    let grid = GridSpec::grid_for(80); // Standard，横向选项
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("Approval required"), "标题行应含 FTL 文案");
    assert!(
        text.contains("Bash wants to run: cargo test"),
        "问题摘要行（人类摘要）"
    );
    assert!(
        text.contains("[Allow once]  [Deny]"),
        "横向选项行（§6.8 视觉）"
    );

    let layout = layout.expect("pending 态必须返回选项布局");
    assert_eq!(layout.option_rows.len(), 1, "横向选项共享一行");
    assert_eq!(layout.option_cols.len(), 2, "每选项一个列区间");
    // 列区间：第二选项起始列 > 第一选项
    let (s0, e0) = layout.option_cols[0].expect("宽屏下第一选项有列区间");
    let (s1, _e1) = layout.option_cols[1].expect("宽屏下第二选项有列区间");
    assert!(s0 < s1 && e0 <= s1, "选项列区间不重叠且有序");
}

/// Narrow 断点（§11）：interaction 选项垂直排列（每行一个），整行命中。
#[test]
fn test_interaction_pending_narrow_vertical_options() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(pending_permission_block());
    let grid = GridSpec::grid_for(30); // Narrow
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("[Allow once]"), "垂直排列第一项");
    assert!(text.contains("[Deny]"), "垂直排列第二项");
    // 不在同一行（垂直）→ 不包含双选项拼接文本
    assert!(!text.contains("[Allow once]  [Deny]"));

    let layout = layout.expect("pending 态必须返回选项布局");
    assert_eq!(layout.option_rows.len(), 2);
    assert_ne!(layout.option_rows[0], layout.option_rows[1], "垂直分行");
    assert_eq!(
        layout.option_cols[0], None,
        "Narrow 垂直排列整行命中（无列区间）"
    );
}

/// completed：Collapsed 单行结果（`✓ Allowed once` 风格：符号 + result）。
#[test]
fn test_interaction_completed_single_line_result() {
    crate::i18n::init(Some("en"));
    let vm = TuiRenderUnit::TuiAskUserBlock(completed_block("Allowed once"));
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, layout) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("Allowed once"), "结果行含 result 文案");
    assert!(
        !text.contains("Bash wants to run"),
        "Collapsed 单行不显示问题摘要"
    );
    assert!(layout.is_none(), "completed 无选项布局");
    // 单行
    assert_eq!(lines.len(), 1, "Collapsed 收束为单行");
}

/// completed + 手动展开：附加问题摘要行（verb + question 可见）。
#[test]
fn test_interaction_completed_expanded_shows_question() {
    crate::i18n::init(Some("en"));
    let mut b = completed_block("Denied");
    b.fold = FoldState::Expanded;
    let vm = TuiRenderUnit::TuiAskUserBlock(b);
    let grid = GridSpec::grid_for(80);
    let mut cache = crate::kit::markdown::MarkdownRenderCache::default();
    let (lines, _, _) = super::vm_to_lines_cached(&vm, &grid, &mut cache, true);

    let text = all_text(&lines);
    assert!(text.contains("Denied"), "结果行");
    assert!(
        text.contains("Bash wants to run: cargo test"),
        "展开时问题摘要可见"
    );
}

// ── [G-Diff] §6.5 diff 展开体渲染（120/80/48 列 golden）──────────────────

use crate::kit::diff_parser::parse_unified_diff;

/// 构造带 diff 的 Edit 卡片（fold=Expanded 展示展开体）。
/// output_summary 设为 diff 文本本身（真实形态——Edit 输出即 diff 文本）。
fn edit_card_with_diff(text: &str) -> TuiToolCard {
    let mut card = tool_card("Edit", "src/main.rs", false, false);
    card.output_summary = text.to_string();
    card.diff = parse_unified_diff(text, Some("src/main.rs"));
    card.fold = FoldState::Expanded;
    card.recompute_hash();
    card
}

const EDIT_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1234567..89abcde 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,6 +10,7 @@ pub fn main() {
 fn main() {
-    let x = 1;
+    let x = 2;
     println!(\"{}\", x);
 }
";

/// 120 列 Wide：header `path +N −M` + hunk 头 + 行号 gutter + patch 标记。
#[test]
fn test_diff_block_renders_wide_120() {
    crate::i18n::init(Some("en"));
    let card = edit_card_with_diff(EDIT_DIFF);
    let grid = GridSpec::grid_for(120);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);

    assert!(text.contains("src/main.rs"), "header 含路径，实际: {text}");
    assert!(
        text.contains("+1"),
        "header 含 +N 计数（1 add），实际: {text}"
    );
    assert!(
        text.contains("\u{2212}1"),
        "header 含 −M 计数（1 del，U+2212），实际: {text}"
    );
    assert!(
        text.contains("@@ -10,6 +10,7 @@"),
        "hunk 头渲染，实际: {text}"
    );
    assert!(text.contains("10"), "context 行号 gutter");
    assert!(text.contains("11 -"), "del 行保留 `-` patch 标记");
    assert!(text.contains("11 +"), "add 行保留 `+` patch 标记");
    assert!(text.contains("fn main() {"), "context 正文");
    assert!(!text.contains("diff --git"), "diff 元数据头不进入渲染");
    assert!(!text.contains("index 1234567"), "index 头不进入渲染");

    // 原始输出行被 diff 块替代（Edit 输出即 diff 文本，避免重复）
    assert!(!text.contains("done"), "diff 卡不显示原始 output_summary");
}

/// 80 列 Standard：行号 gutter 保留，代码不软换行（硬截断）。
#[test]
fn test_diff_block_renders_standard_80() {
    crate::i18n::init(Some("en"));
    let long_diff = "\
--- a/long.rs
+++ b/long.rs
@@ -1,1 +1,1 @@
-very long deleted line that definitely exceeds the eighty column content budget in this terminal width
+replacement line that is also extremely long and will be truncated by width not wrapped
";
    let card = edit_card_with_diff(long_diff);
    let grid = GridSpec::grid_for(80);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    for line in &lines {
        let w = line_text(line).width();
        assert!(
            w <= grid.total_width(),
            "diff 行不软换行（硬截断），宽度 {w} > {}，行: {:?}",
            grid.total_width(),
            line_text(line)
        );
    }
    let text = all_text(&lines);
    assert!(text.contains("+ replacement"), "patch 标记保留");
    assert!(text.contains("- very long"), "patch 标记保留");
}

/// 48 列 Compact：先隐藏行号 gutter，再裁切代码（§6.5「窄屏先隐行号」）。
#[test]
fn test_diff_block_hides_gutter_at_48() {
    crate::i18n::init(Some("en"));
    let card = edit_card_with_diff(EDIT_DIFF);
    let grid = GridSpec::grid_for(48);
    assert!(matches!(grid.bp, Breakpoint::Compact), "48 列是 Compact");
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);

    assert!(text.contains("fn main() {"), "正文仍可见");
    assert!(text.contains("+"), "patch 标记仍可见");
    assert!(text.contains("-"), "patch 标记仍可见");
    assert!(
        text.contains("@@ -10,6 +10,7 @@"),
        "hunk 头（dim 元信息）窄屏仍显示"
    );

    // 行号 gutter 隐藏：`11 -` 的 `11 ` 不应出现在行首（符号后正文紧跟）
    let del_line = lines
        .iter()
        .find(|l| line_text(l).contains("let x = 1"))
        .map(line_text)
        .unwrap_or_default();
    assert!(
        !del_line.trim_start().starts_with("11"),
        "窄屏无行号列，实际: {del_line:?}"
    );
    assert!(
        del_line.contains("let x = 1"),
        "patch 标记 + 正文，实际: {del_line:?}"
    );
}

/// 截断指示：>8 change 行 → `… +N more lines`（§6.5）。
#[test]
fn test_diff_block_more_lines_indicator() {
    crate::i18n::init(Some("en"));
    let mut text = String::from("--- a/x\n+++ b/x\n@@ -1,20 +1,20 @@\n");
    for i in 0..10 {
        text.push_str(&format!("- old {i}\n"));
    }
    for i in 0..10 {
        text.push_str(&format!("+ new {i}\n"));
    }
    let card = edit_card_with_diff(&text);
    assert!(card.diff.is_some(), "截断 diff 仍可解析（不降级）");
    let grid = GridSpec::grid_for(120);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);
    assert!(
        text.contains("\u{2026} +12 more lines"),
        "截断指示（12 剩余 change 行），实际: {text}"
    );
}

/// 解析失败（非 diff 输出）→ diff=None → 渲染保持历史行为（无 diff 块）。
#[test]
fn test_diff_block_falls_back_when_unparsable() {
    crate::i18n::init(Some("en"));
    let mut card = tool_card("Edit", "src/x.rs", false, false);
    card.fold = FoldState::Expanded;
    card.output_summary = "Wrote 3 lines to x.rs".into();
    card.recompute_hash();
    assert!(card.diff.is_none(), "非 diff 输出静默降级");
    let grid = GridSpec::grid_for(120);
    let lines = vm_to_lines(&TuiRenderUnit::TuiToolCard(card), &grid);
    let text = all_text(&lines);
    assert!(text.contains("Wrote 3 lines to x.rs"), "兜底显示原始输出");
}

// ── [D3 §9] 语义复制：semantic_line_text 变体分派矩阵 ────────────────────

use crate::kit::message_area::render::semantic_line_text;

/// 测试辅助：渲染 VM 后对 `local_idx` 行提取语义文本（复制路径语义——
/// 传入已渲染行，不重渲染 VM）。
fn sem_at(vm: &TuiRenderUnit, local_idx: usize, grid: &GridSpec) -> Option<String> {
    let lines = vm_to_lines(vm, grid);
    let line = lines.get(local_idx)?;
    semantic_line_text(vm, local_idx, line, grid)
}

/// 普通 assistant 正文行：剥前缀列（outer + accent + gap），无符号无竖线。
#[test]
fn test_semantic_plain_lines_strip_prefix() {
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        // 两个 markdown 段落（段落间渲染空行）
        text: "第一行\n\nsecond line with 中文".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 0,
    });
    // 行 0 = 前导空行；行 1 = 段落 1；行 2 = 段间空行；行 3 = 段落 2
    let l1 = sem_at(&vm, 1, &grid).expect("段落 1");
    assert_eq!(l1, "第一行", "段落剥前缀，实际: {l1:?}");
    let l2 = sem_at(&vm, 3, &grid).expect("段落 2");
    assert_eq!(l2, "second line with 中文", "段落剥前缀，实际: {l2:?}");
}

/// §6.1/§6.2 无 role label 行：`You` / `Perihelion` 不渲染，正文从第 1 行开始。
#[test]
fn test_no_role_label_line_rendered() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);

    // assistant：行 0 = 前导空行；行 1 = 正文（无 `Perihelion` label）
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: "hello".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 0,
    });
    let lines = vm_to_lines(&vm, &grid);
    let text = crate::kit::text_selection::line_to_plain_text(&lines[1]);
    assert!(
        !text.contains("Perihelion"),
        "assistant 无 role label，实际: {text:?}"
    );
    assert_eq!(
        sem_at(&vm, 1, &grid).as_deref(),
        Some("hello"),
        "正文从行 1 开始（前后各 1 空行）"
    );

    // user：行 0 = leading 空行；行 1 = 正文（无 `You` label）
    let user_vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: "hello".to_string(),
        reminder: None,
        source: None,
        content_hash: 0,
    });
    let ulines = vm_to_lines(&user_vm, &grid);
    let utext = crate::kit::text_selection::line_to_plain_text(&ulines[1]);
    assert!(
        !utext.contains("You"),
        "user 无 role label，实际: {utext:?}"
    );
    assert_eq!(
        sem_at(&user_vm, 1, &grid).as_deref(),
        Some("hello"),
        "正文从行 1 开始"
    );
}

/// §9.1 Edit/Write 头行只保留 diff 计数（`· +N −M`）——摘要文本含路径，
/// 与 header 的 `input_summary` 重复，不再拼接。
#[test]
fn test_semantic_tool_header_edit_write_count_only() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let card = edit_card_with_diff(EDIT_DIFF);
    let vm = TuiRenderUnit::TuiToolCard(card);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Edit src/main.rs \u{b7} +1 \u{b7} -1",
        "header 只含 label+path+计数，实际: {sem:?}"
    );
    assert!(
        !sem.contains("Removed") && !sem.contains("lines changed"),
        "不拼接 output_summary 原文，实际: {sem:?}"
    );

    // Write：新文件摘要（`Wrote 1 line to x.rs`）→ 同样只留计数
    let mut write = tool_card("Write", "/tmp/x.rs", false, false);
    write.output_summary = "Wrote 1 line to /tmp/x.rs".into();
    write.diff = crate::kit::diff_parser::parse_edit_write_summary(
        "Wrote 1 line to /tmp/x.rs",
        Some("/tmp/x.rs"),
    );
    write.recompute_hash();
    let sem = sem_at(&TuiRenderUnit::TuiToolCard(write), 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Write /tmp/x.rs \u{b7} +1",
        "Write 只留计数，实际: {sem:?}"
    );
}

/// tool header 行：`{Verb} {summary}{suffix}`（无符号、无 duration）。
#[test]
fn test_semantic_tool_header() {
    let grid = GridSpec::grid_for(120);
    // Read：label + path + `— N lines` 后缀
    let mut read_card = tool_card("Read", "src/main.rs", false, false);
    read_card.output_summary = "line1\nline2\nline3\n".into();
    read_card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(read_card);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Read src/main.rs \u{2014} 3 lines",
        "label+summary+suffix，无符号无时长，实际: {sem:?}"
    );

    // Bash：label + command（不展开时；label 用显示名 Shell）
    let bash_card = tool_card("Bash", "cargo test -p peri-tui", false, false);
    let vm = TuiRenderUnit::TuiToolCard(bash_card);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(
        sem, "Shell cargo test -p peri-tui",
        "Bash header 复制 command"
    );

    // Bash 展开态：summary 移到 `$ ` 行——header 只留 label
    let mut bash_expanded = tool_card("Bash", "cargo test", false, false);
    bash_expanded.fold = FoldState::Expanded;
    bash_expanded.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(bash_expanded);
    let sem = sem_at(&vm, 0, &grid).expect("header 行");
    assert_eq!(sem, "Shell", "展开态 header 只留 label（显示名）");
}

/// §8 子 agent 工具行语义复制：`{Verb} {summary}`（与主时间线 tool header 同口径，
/// 无符号、无 duration、无缩进/竖线）；原因行 → 纯错误正文（剥缩进）；顶层
/// 单行摘要（非 running）走默认剥离。
#[test]
fn test_semantic_subagent_tool_line() {
    crate::i18n::init(Some("en"));
    let grid = GridSpec::grid_for(120);
    let children = im::Vector::from(vec![
        TuiRenderUnit::TuiToolCard(tool_card("Grep", "pattern: x", true, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Bash", "cargo test", false, false)),
        TuiRenderUnit::TuiToolCard(tool_card("Read", "src/main.rs", false, true)),
    ]);
    let running = subagent_group(children, true);
    // 行序：Read（running）/ Bash（completed）/ Grep（error）/ 原因行
    // ——running 有工具时无顶层单行摘要（render_subagent_group_lines 早退）
    let lines = vm_to_lines(&running, &grid);
    assert_eq!(lines.len(), 4, "3 工具行 + 原因行，实际 {}", lines.len());

    let sem_read = sem_at(&running, 0, &grid).expect("Read 工具行");
    assert_eq!(sem_read, "Read src/main.rs", "实际: {sem_read:?}");
    let sem_bash = sem_at(&running, 1, &grid).expect("Bash 工具行");
    assert_eq!(
        sem_bash, "Shell cargo test",
        "Bash → Shell 本地化，实际: {sem_bash:?}"
    );
    let sem_grep = sem_at(&running, 2, &grid).expect("Grep 工具行");
    assert_eq!(sem_grep, "Grep pattern: x", "实际: {sem_grep:?}");
    // 原因行 → 纯错误正文（剥缩进）
    let sem_reason = sem_at(&running, 3, &grid).expect("原因行");
    assert_eq!(
        sem_reason, "Error: something went wrong",
        "实际: {sem_reason:?}"
    );
    // 语义不含 chrome：无 ✓/×/竖线、无前导空格（§11 语义复制要点）
    for s in [&sem_read, &sem_bash, &sem_grep] {
        assert!(
            !s.contains('\u{2713}') && !s.contains('\u{d7}') && !s.contains('\u{2502}'),
            "无符号/竖线 chrome，实际: {s:?}"
        );
        assert!(!s.starts_with(' '), "无前导空格，实际: {s:?}");
    }

    // 非 running（单行摘要 + 原因行）：顶层行默认剥离；原因行剥缩进
    let failed = subagent_group(
        im::Vector::from(vec![TuiRenderUnit::TuiToolCard(tool_card(
            "Grep", "src", true, false,
        ))]),
        false,
    );
    let sem_top = sem_at(&failed, 0, &grid).expect("顶层行");
    assert!(
        sem_top.contains("Agent explorer") && !sem_top.starts_with(' '),
        "顶层行默认剥离，实际: {sem_top:?}"
    );
    let sem_failed_reason = sem_at(&failed, 1, &grid).expect("failed 原因行");
    assert_eq!(
        sem_failed_reason, "Error: something went wrong",
        "非 running 原因行同样剥缩进，实际: {sem_failed_reason:?}"
    );
}

/// Bash 展开 `$ cmd` 行保留 command（§9）。
#[test]
fn test_semantic_bash_command_line() {
    let grid = GridSpec::grid_for(120);
    let mut bash_expanded = tool_card("Bash", "cargo test --workspace", false, false);
    bash_expanded.fold = FoldState::Expanded;
    bash_expanded.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(bash_expanded);
    let lines = vm_to_lines(&vm, &grid);
    // 找 `$ cmd` 行（非空且含 `$ `）
    let idx = lines
        .iter()
        .position(|l| line_text(l).contains("$ cargo test --workspace"))
        .expect("`$ cmd` 行存在");
    let sem = sem_at(&vm, idx, &grid).expect("$ 行");
    assert_eq!(sem, "$ cargo test --workspace", "保留 $ 前缀与 command");
}

/// diff 行：剥离行号 gutter，保留 `+`/`-` patch 标记（§9）。
#[test]
fn test_semantic_diff_lines_strip_gutter() {
    let grid = GridSpec::grid_for(120);
    let card = edit_card_with_diff(EDIT_DIFF);
    let vm = TuiRenderUnit::TuiToolCard(card);
    let lines = vm_to_lines(&vm, &grid);
    let del_idx = lines
        .iter()
        .position(|l| line_text(l).contains("let x = 1"))
        .expect("del 行");
    let sem = sem_at(&vm, del_idx, &grid).expect("del 行");
    assert_eq!(
        sem, "-     let x = 1;",
        "patch 标记保留、行号剥离，实际: {sem:?}"
    );
    let add_idx = lines
        .iter()
        .position(|l| line_text(l).contains("let x = 2"))
        .expect("add 行");
    let sem = sem_at(&vm, add_idx, &grid).expect("add 行");
    assert_eq!(sem, "+     let x = 2;", "add 行同规则，实际: {sem:?}");
    // context 行：纯正文（符号为空格，剥离后无补丁标记）
    let ctx_idx = lines
        .iter()
        .position(|l| line_text(l).contains("fn main() {"))
        .expect("context 行");
    let sem = sem_at(&vm, ctx_idx, &grid).expect("context 行");
    assert_eq!(sem, "fn main() {", "context 行纯正文，实际: {sem:?}");
}

/// 普通输出行以数字开头（如 Bash 输出 `42  foo`）不误判为 diff 行。
#[test]
fn test_semantic_output_line_not_mistaken_for_diff() {
    let grid = GridSpec::grid_for(120);
    let mut card = tool_card("Bash", "echo 42", false, false);
    card.output_summary = "42  foo\nbar".into();
    card.fold = FoldState::Expanded;
    card.recompute_hash();
    let vm = TuiRenderUnit::TuiToolCard(card);
    let lines = vm_to_lines(&vm, &grid);
    let idx = lines
        .iter()
        .position(|l| line_text(l).contains("42  foo"))
        .expect("输出行");
    let sem = sem_at(&vm, idx, &grid).expect("输出行");
    assert_eq!(sem, "42  foo", "输出行不剥内容（非 diff gutter 模式）");
}

/// code block 行：剥 `│ ` gutter（现状无语言标签行/行号）。
#[test]
fn test_semantic_code_block_strips_gutter() {
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text: "```rs\nlet x = 1;\n```".to_string(),
        reasoning: None,
        message_id: None,
        content_hash: 0,
    });
    let lines = vm_to_lines(&vm, &grid);
    let idx = lines
        .iter()
        .position(|l| line_text(l).contains("let x = 1"))
        .expect("code 行");
    let sem = sem_at(&vm, idx, &grid).expect("code 行");
    assert_eq!(sem, "let x = 1;", "剥 `│ ` gutter，实际: {sem:?}");
}

/// user bubble：label 行（`You`）与正文行剥离前缀。
#[test]
fn test_semantic_user_bubble() {
    let grid = GridSpec::grid_for(120);
    let vm = TuiRenderUnit::TuiUserBubble(crate::kit::tui_render_unit::TuiUserBubble {
        text: "重构消息流".to_string(),
        source: None,
        reminder: None,
        content_hash: 0,
    });
    // 行 0 = leading 空行；行 1 = 正文（无 role label 行）
    let body = sem_at(&vm, 1, &grid).expect("正文行");
    assert_eq!(body, "重构消息流", "正文剥前缀，实际: {body:?}");
}

// ── md 行宽回归（§6.2 竖线连续性）──────────────────────────────────────

/// [Fix] markdown 段落行必须在 convert 阶段折行到 content 宽度——否则超宽行
/// 到达渲染层后会被视口 Paragraph 二次折行，折出的行丢失 `│` 竖线前缀
/// （左侧竖线被打断）。
#[test]
fn test_assistant_md_paragraph_lines_stay_within_viewport() {
    let grid = GridSpec::grid_for(80); // content = 74，前缀 = 6
    let text = format!("{} 结尾", "word ".repeat(40)); // 200+ 字符超宽行
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text,
        reasoning: None,
        message_id: None,
        content_hash: 1,
    });
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text,
        reasoning: None,
        message_id: None,
        content_hash: 2,
    });
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
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        started_at: None,
        duration_ms: None,
        text,
        reasoning: None,
        message_id: None,
        content_hash: 3,
    });
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
