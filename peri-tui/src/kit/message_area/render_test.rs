/// 消息区渲染边界条件测试：极小宽度、空内容等边缘场景。

use crate::kit::message_area::render::vm_to_lines;
use crate::kit::message_area::selection::build_wrap_map;
use crate::kit::tui_render_unit::{
    TuiAssistantBubble, TuiAskUserBlock, TuiCollapsedGroup, TuiDivider, TuiNoteLevel,
    TuiRenderUnit, TuiSubAgentGroup, TuiSystemNote, TuiToolCard,
};

/// 宽度为 1 时，所有 VM 变体的 vm_to_lines 不应 panic。
/// [回归测试] 快速缩小终端到极小宽度时程序崩溃。
#[test]
fn test_vm_to_lines_all_variants_width_1() {
    let empty_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: String::new(),
        reasoning: None,
        content_hash: 42,
    });

    let text_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: "hello world\n测试内容".to_string(),
        reasoning: None,
        content_hash: 43,
    });

    let table_bubble = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: "| col1 | col2 |\n|------|------|\n| a    | b    |\n| c    | d    |"
            .to_string(),
        reasoning: None,
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
        view_models: vec![],
        content_hash: 47,
    });

    let ask_user = TuiRenderUnit::TuiAskUserBlock(TuiAskUserBlock {
        items: vec![],
        is_error: false,
        content_hash: 48,
    });

    let subagent = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "test-agent".to_string(),
        agent_name: "Test Agent".to_string(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_id: "t1".to_string(),
            tool_name: "Bash".to_string(),
            input_summary: "bash test".to_string(),
            output_summary: String::new(),
            is_error: false,
            is_running: false,
            running_duration_ms: None,
            diff: None,
            content_hash: 100,
            tool_calls_count: 0,
        })]),
        collapsed: false,
        is_running: false,
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
        ("SubAgentGroup", subagent),
    ];

    for width in [1usize, 2, 3, 5] {
        for (_label, vm) in &all_variants {
            let _lines = vm_to_lines(vm, width);
            // 只要不 panic 就算通过
        }
    }
}

/// build_wrap_map 在宽度为 1 时不应 panic（所有字符折为单独行）。
/// [回归测试] 历史消息中长文本在极窄宽度下可能导致 line_count 异常。
#[test]
fn test_build_wrap_map_width_1_no_panic() {
    // 模拟一段典型对话文本（含 CJK）
    let lines = vec![
        ratatui_kit::ratatui::text::Line::from("这是一段包含中英文 mixed content 的代表性消息，模拟真实对话内容。"),
        ratatui_kit::ratatui::text::Line::from("第二行：包含各种字符 hello world 12345 !@#$%"),
        ratatui_kit::ratatui::text::Line::from("Third line: purely ASCII text for comparison purposes."),
    ];
    for width in [1u16, 2, 3, 5, 10] {
        let _ = build_wrap_map(&lines, width);
    }
}

/// 空 AssistantBubble（无 text、无 reasoning）返回空 lines。
/// [回归测试] 这会导致 build_wrap_map 产出 visual_rows=0，
/// 进而 total_visual_rows 可能为 0，触发 scrollbar position 计算的 usize underflow。
#[test]
fn test_empty_assistant_bubble_returns_zero_lines() {
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: String::new(),
        reasoning: None,
        content_hash: 1,
    });
    let lines = vm_to_lines(&vm, 80);
    assert!(lines.is_empty(), "空 AssistantBubble 应返回 0 行");
}
