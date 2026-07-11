//! view_render 模块测试。

use super::tool_card::format_running_duration;
use super::*;
use crate::kit::tui_render_unit::{
    ReminderInfo, ReminderType, TuiAssistantBubble, TuiCollapsedGroup, TuiDiffBlock, TuiDivider,
    TuiHunk, TuiHunkLine, TuiHunkLineKind, TuiNoteLevel, TuiReasoningBlock, TuiSubAgentGroup,
    TuiSystemNote, TuiToolCard, TuiUserBubble,
};
use peri_theme::atoms::THEME_ATOM;
use std::sync::atomic::Ordering;

/// 测试辅助：一个最小 SubAgentStatusProbe，返回固定 SubAgentRenderInfo。
struct StaticProbe {
    info: Option<SubAgentRenderInfo>,
}
impl SubAgentStatusProbe for StaticProbe {
    fn lookup_by_agent_id(&self, _agent_id: &str) -> Option<SubAgentRenderInfo> {
        self.info.clone()
    }
}

fn collect_text(lines: &[ratatui::text::Line]) -> String {
    lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.clone())
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn test_user_bubble_basic() {
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: "hello world".into(),
        content_hash: 0,
        reminder: None,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert!(
        !lines.is_empty(),
        "TuiUserBubble should produce at least one line"
    );
}

#[test]
fn test_user_bubble_has_spec_spacing_and_prefix() {
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: "hello\nworld".into(),
        content_hash: 0,
        reminder: None,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert_eq!(
        lines.len(),
        2,
        "用户消息应 1 行头部空行 + 1 行内容：{}",
        text
    );
    assert!(
        text.contains("\u{276f} hello world"),
        "首行应使用 \u{276f} 前缀：{}",
        text
    );
    assert!(
        lines
            .first()
            .is_some_and(|line| collect_text(std::slice::from_ref(line)).is_empty())
    );
}

#[test]
fn test_user_bubble_reminder_two_line_rendering() {
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: "<system-reminder>Cron task: cleanup</system-reminder>".into(),
        content_hash: 0,
        reminder: Some(ReminderInfo {
            reminder_type: ReminderType::CronReminder,
            summary: "Cron task: cleanup".into(),
        }),
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(text.contains("Cron 任务"), "首行应为类型标签：{}", text);
    assert!(
        text.contains("\u{23bf} Cron task: cleanup"),
        "第二行应为摘要：{}",
        text
    );
    assert!(
        !text.contains("\u{276f}"),
        "reminder 不应有 \u{276f} 前缀：{}",
        text
    );
}

#[test]
fn test_user_bubble_reminder_no_summary() {
    let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
        text: "<system-reminder></system-reminder>".into(),
        content_hash: 0,
        reminder: Some(ReminderInfo {
            reminder_type: ReminderType::GenericReminder,
            summary: String::new(),
        }),
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    // 仅一行（无摘要行）
    assert_eq!(lines.len(), 1, "空摘要应仅有一行");
}

#[test]
fn test_assistant_bubble_text() {
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: "**bold** text".into(),
        reasoning: None,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert!(!lines.is_empty());
}

#[test]
fn test_assistant_bubble_with_reasoning() {
    let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
        text: String::new(),
        reasoning: Some(TuiReasoningBlock {
            text: "thinking deeply...\nline 2\nline 3\nline 4".into(),
            collapsed: false,
        }),
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert!(!lines.is_empty());
    // 首行为空行（间距），第二行为 "Thought for N chars"
    let second = &lines[1].spans;
    assert!(second.iter().any(|s| s.content.contains("Thought for")));
}

#[test]
fn test_tool_card_success() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-1".into(),
        tool_name: "Read".into(),
        input_summary: "path: foo.rs".into(),
        output_summary: "3 lines".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert!(!lines.is_empty());
    let first = &lines[1].spans;
    assert!(first.iter().any(|s| s.content.contains("Read")));
}

#[test]
fn test_tool_card_read_collapsed_shows_line_count() {
    // Read 折叠态现在显示行数摘要（"N lines"），不再隐藏全部输出
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-read-collapsed".into(),
        tool_name: "Read".into(),
        input_summary: "path: foo.rs".into(),
        output_summary: "47 lines".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{25cf} Read (path: foo.rs)"),
        "工具头应使用括号摘要：{}",
        text
    );
    assert!(
        text.contains("47 lines"),
        "Read 折叠态应显示行数摘要：{}",
        text
    );
    assert!(
        lines
            .first()
            .is_some_and(|line| collect_text(std::slice::from_ref(line)).is_empty())
    );
}

#[test]
fn test_tool_card_error() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-2".into(),
        tool_name: "Bash".into(),
        input_summary: "rm -rf /".into(),
        output_summary: "permission denied".into(),
        is_error: true,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let first = &lines[1].spans;
    assert!(first.iter().any(|s| s.content.contains("\u{25cf}")));
}

#[test]
fn test_tool_card_collapsed_error_shows_error_summary() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-read-error".into(),
        tool_name: "Read".into(),
        input_summary: "foo.rs".into(),
        output_summary: "permission denied".into(),
        is_error: true,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{25cf} Read (foo.rs)"),
        "错误工具应显示失败标识（红色 \u{25cf}）：{}",
        text
    );
    assert!(
        text.contains("\u{23bf} permission denied"),
        "错误摘要应展开显示：{}",
        text
    );
}

#[test]
fn test_tool_card_running_shows_status() {
    // is_running 的 ● 现在是常量白色指示（不再依赖 RENDER_CALL_COUNT 闪烁）。
    RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));

    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-running".into(),
        tool_name: "Edit".into(),
        input_summary: "path: foo.rs\nold_string: hello".into(),
        output_summary: String::new(),
        is_error: false,
        is_running: true,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{25cf}"),
        "运行中工具应显示状态 \u{25cf}：{}",
        text
    );
    // 运行中状态仅通过前导白色 ● 表示（常量，不闪烁），不再追加尾部 · ●
    assert!(
        !text.contains("\u{b7} \u{25cf}"),
        "运行中工具不应显示尾部标记：{}",
        text
    );
    assert!(text.contains("Edit (path: foo.rs \u{b7} old_string: hello"));
}

#[test]
fn test_tool_card_bash_running_shows_elapsed_line() {
    RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));

    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-bash-running".into(),
        tool_name: "Bash".into(),
        input_summary: "cargo test".into(),
        output_summary: String::new(),
        is_error: false,
        is_running: true,
        running_duration_ms: Some(61_000),
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{25cf} Shell (cargo test)"),
        "Bash 应显示为 Shell：{}",
        text
    );
    assert!(
        text.contains("\u{23bf} Running (1min 1s)"),
        "运行中 Bash 应显示耗时行（含秒数）：{}",
        text
    );
}

#[test]
fn test_format_running_duration_seconds_only() {
    assert_eq!(format_running_duration(0), "0s");
    assert_eq!(format_running_duration(45_000), "45s");
    assert_eq!(format_running_duration(59_000), "59s");
}

#[test]
fn test_format_running_duration_minutes_and_seconds() {
    assert_eq!(format_running_duration(60_000), "1min 0s");
    assert_eq!(format_running_duration(61_000), "1min 1s");
    assert_eq!(format_running_duration(85_000), "1min 25s");
    assert_eq!(format_running_duration(3_600_000), "60min 0s");
}

#[test]
fn test_tool_card_bash_completed_does_not_show_running_line() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-bash-complete".into(),
        tool_name: "Bash".into(),
        input_summary: "cargo test".into(),
        output_summary: "line 1\nline 2\nline 3".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("Running ("),
        "完成态不应显示 Running：{}",
        text
    );
    assert!(
        text.contains("line 1"),
        "完成态应保留现有输出摘要：{}",
        text
    );
    assert!(
        text.contains("\u{2026} 2 more lines"),
        "完成态仍应压缩输出：{}",
        text
    );
}

#[test]
fn test_tool_card_agent_running_shows_tool_calls_and_duration() {
    RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));

    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "agent-tc-1".into(),
        tool_name: "Agent".into(),
        input_summary: "search rust patterns".into(),
        output_summary: String::new(),
        is_error: false,
        is_running: true,
        running_duration_ms: Some(85_000),
        diff: None,
        tool_calls_count: 5,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{25cf} Agent (search rust patterns)"),
        "Agent 工具卡应使用 \u{25cf} 原点前缀：{}",
        text
    );
    assert!(
        text.contains("\u{23bf} 5 tool calls, running 1min 25s"),
        "Agent 运行行应显示 tool calls 计数和时长：{}",
        text
    );
}

#[test]
fn test_tool_card_agent_not_running_shows_no_running_line() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "agent-tc-done".into(),
        tool_name: "Agent".into(),
        input_summary: "search done".into(),
        output_summary: "found matches".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("tool calls"),
        "Agent 完成态不应显示 running 行：{}",
        text
    );
}

#[test]
fn test_tool_card_output_is_compacted() {
    // Bash 默认折叠（COLLAPSED_BY_DEFAULT），max_lines=1，5 行输出 → 1 行 + "… 4 more lines"
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-output".into(),
        tool_name: "Bash".into(),
        input_summary: "cargo test".into(),
        output_summary: "line 1\nline 2\nline 3\nline 4\nline 5".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{2026} 4 more lines"),
        "长输出应被压缩：{}",
        text
    );
}

#[test]
fn test_tool_card_write_shows_output_summary_no_diff_hint() {
    // Write 工具完成后不再渲染 diff（已移除），仅显示 output_summary。
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-diff-hint".into(),
        tool_name: "Write".into(),
        input_summary: "bar.rs".into(),
        output_summary: "12 lines changed".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: Some(TuiDiffBlock {
            path: "bar.rs".into(),
            hunks: vec![],
            is_binary: false,
            is_too_large: false,
            is_new_file: false,
        }),
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("12 lines changed"),
        "Write 工具应显示 output_summary：{}",
        text
    );
    assert!(
        !text.contains("\u{5df2}\u{6298}\u{53e0}"),
        "不应再显示 diff 折叠提示：{}",
        text
    );
    assert!(
        !text.contains("\u{1f4dd}"),
        "不应再显示 diff 标记：{}",
        text
    );
}

#[test]
fn test_tool_card_web_uses_spec_indicator() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-web".into(),
        tool_name: "WebFetch".into(),
        input_summary: "https://example.com".into(),
        output_summary: "ok".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{25cf} WebFetch"),
        "Web 工具应使用原始工具名而非映射别名：{}",
        text
    );
}

#[test]
fn test_tool_card_bash_uses_spec_indicator_and_display_name() {
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-bash".into(),
        tool_name: "Bash".into(),
        input_summary: "cargo test".into(),
        output_summary: "ok".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{25cf} Shell"),
        "Bash 工具应映射为 Shell 并使用统一成功标识：{}",
        text
    );
}

#[test]
fn test_tool_card_diff_removed() {
    // diff 渲染已完全移除，Edit/Write 工具不再展示 diff 行。
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-3".into(),
        tool_name: "Edit".into(),
        input_summary: "foo.rs".into(),
        output_summary: "ok".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: Some(TuiDiffBlock {
            path: "foo.rs".into(),
            hunks: vec![TuiHunk {
                old_range: "-1,3".into(),
                new_range: "+1,4".into(),
                lines: vec![TuiHunkLine {
                    kind: TuiHunkLineKind::Add,
                    text: "new line".into(),
                    old_no: None,
                    new_no: Some(4),
                }],
            }],
            is_binary: false,
            is_too_large: false,
            is_new_file: false,
        }),
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert!(!lines.is_empty());
    let has_diff = lines
        .iter()
        .any(|l| l.spans.iter().any(|s| s.content.contains("+++")));
    assert!(!has_diff, "diff 已移除，不应包含 +++ diff header");
    let text = collect_text(&lines);
    assert!(
        text.contains("ok"),
        "Edit 工具应显示 output_summary：{}",
        text
    );
}

#[test]
fn test_tool_card_write_no_diff() {
    // Write 工具不再渲染 diff（diff 已移除），assert 不应出现 diff 行。
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-4".into(),
        tool_name: "Write".into(),
        input_summary: "bar.rs".into(),
        output_summary: "ok".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: Some(TuiDiffBlock {
            path: "bar.rs".into(),
            hunks: vec![],
            is_binary: false,
            is_too_large: false,
            is_new_file: false,
        }),
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let has_diff = lines
        .iter()
        .any(|l| l.spans.iter().any(|s| s.content.contains("+++")));
    assert!(!has_diff, "diff 已移除，不应包含 diff header");
}

#[test]
fn test_tool_card_bash_collapsed_by_default() {
    // Bash 默认折叠（COLLAPSED_BY_DEFAULT），完成后仅显示首行输出摘要
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-bash-collapsed".into(),
        tool_name: "Bash".into(),
        input_summary: "ls -la".into(),
        output_summary: "total 8\ndrwxr-xr-x  3 user staff  96 Jul  6 10:00 .\ndrwxr-xr-x  5 user staff 160 Jul  6 09:00 ..".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("\u{2026} 2 more lines"),
        "Bash 默认折叠，应压缩多行输出：{}",
        text
    );
}

#[test]
fn test_tool_card_search_extra_tools_auto_expand() {
    // SearchExtraTools 结果自动展开（AUTO_EXPAND），完成后展示完整输出（最多 4 行）
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-set-autox".into(),
        tool_name: "SearchExtraTools".into(),
        input_summary: "mcp__weixin".into(),
        output_summary: "tool_1\ntool_2\ntool_3".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        text.contains("tool_1"),
        "SearchExtraTools 应自动展开显示完整结果：{}",
        text
    );
}

#[test]
fn test_system_note_info() {
    let vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "session started".into(),
        level: TuiNoteLevel::Info,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert_eq!(lines.len(), 1);
}

#[test]
fn test_system_note_error() {
    let vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "fatal error".into(),
        level: TuiNoteLevel::Error,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert_eq!(lines.len(), 1);
}

#[test]
fn test_subagent_group_always_shows_content() {
    // SubAgent 无折叠态——collapsed 字段被忽略，始终展开
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-1".into(),
        agent_name: "file-searcher".into(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "find foo".into(),
            content_hash: 0,
            reminder: None,
        })]),
        collapsed: true,
        is_running: false,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("Agent(sa-1)"),
        "SubAgent 组不再渲染 \u{276f} 头行：{}",
        text
    );
    assert!(text.contains("find foo"), "子内容应始终可见：{}", text);
}

#[test]
fn test_subagent_group_expanded() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-2".into(),
        agent_name: "tester".into(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "test".into(),
            content_hash: 0,
            reminder: None,
        })]),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert!(!lines.is_empty());
}

#[test]
fn test_subagent_group_expanded_skips_assistant_bubble_and_trims_result() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-visual".into(),
        agent_name: "visual".into(),
        view_models: im::Vector::from(vec![
            TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
                text: "hidden assistant".into(),
                reasoning: None,
                content_hash: 0,
            }),
            TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: "visible user".into(),
                content_hash: 0,
                reminder: None,
            }),
        ]),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: false,
            is_error: false,
            total_steps: 1,
            final_result: Some("x".repeat(100)),
            recent_messages: Vec::new(),
        }),
    });
    let lines = segments_to_lines(&with_status_probe(probe, || render_v2_vm(&vm, 80)));
    let text = collect_text(&lines);
    assert!(
        !text.contains("hidden assistant"),
        "嵌套 TuiAssistantBubble 不应渲染：{}",
        text
    );
    assert!(
        text.contains("visible user"),
        "非 TuiAssistantBubble 嵌套消息应渲染：{}",
        text
    );
    assert_eq!(
        text.matches('x').count(),
        80,
        "最终结果应截断到 80 字符：{}",
        text
    );
}

#[test]
fn test_subagent_group_with_running_probe_shows_status_icon() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: true,
            is_error: false,
            total_steps: 5,
            final_result: None,
            recent_messages: Vec::new(),
        }),
    });
    let lines = segments_to_lines(&with_status_probe(probe, || render_v2_vm(&vm, 80)));
    let text = collect_text(&lines);
    assert!(
        !text.contains("\u{276f}"),
        "SubAgent 组不再渲染 \u{276f} 箭头头行：{}",
        text
    );
    assert!(
        !text.contains("\u{b7} \u{23f3}"),
        "运行状态指示器已随头行删除：{}",
        text
    );
}

#[test]
fn test_subagent_group_with_done_probe_shows_final_result() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: false,
            is_error: false,
            total_steps: 3,
            final_result: Some("completed task successfully".into()),
            recent_messages: Vec::new(),
        }),
    });
    let lines = segments_to_lines(&with_status_probe(probe, || render_v2_vm(&vm, 80)));
    let text = collect_text(&lines);
    assert!(
        !text.contains("\u{276f}"),
        "SubAgent 组不再渲染 \u{276f} 箭头头行：{}",
        text
    );
    assert!(
        text.contains("\u{23bf} completed task"),
        "应显示结果预览：{}",
        text
    );
}

#[test]
fn test_subagent_group_with_error_probe_shows_failed() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: false,
            is_error: true,
            total_steps: 2,
            final_result: Some("Error: tool failed".into()),
            recent_messages: Vec::new(),
        }),
    });
    let lines = segments_to_lines(&with_status_probe(probe, || render_v2_vm(&vm, 80)));
    let text = collect_text(&lines);
    assert!(
        !text.contains("\u{276f}"),
        "SubAgent 组不再渲染 \u{276f} 箭头头行：{}",
        text
    );
    assert!(
        !text.contains("\u{b7} \u{274c}"),
        "错误指示器已随头行删除：{}",
        text
    );
    assert!(text.contains("\u{23bf} Error"), "应显示错误结果：{}", text);
}

#[test]
fn test_subagent_group_without_probe_shows_success_icon_for_committed_placeholder() {
    // 不设置 probe → 已提交的 DTO placeholder
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("\u{276f}"),
        "SubAgent 组不再渲染 \u{276f} 箭头头行：{}",
        text
    );
}

#[test]
fn test_subagent_group_streaming_dto_shows_running() {
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::new(),
        collapsed: false,
        is_running: true,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    assert!(
        !text.contains("\u{276f}"),
        "SubAgent 组不再渲染 \u{276f} 箭头头行：{}",
        text
    );
}

#[test]
fn test_subagent_group_falls_back_to_probe_recent_messages() {
    // DTO.view_models 为空 placeholder，但 probe 提供 recent_messages
    // → 渲染应回退到 probe 的子内容（Phase 2.6 桥接核心路径）
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::new(), // 空占位符
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: true,
            is_error: false,
            total_steps: 1,
            final_result: None,
            recent_messages: vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: "child content from probe".into(),
                content_hash: 0,
                reminder: None,
            })],
        }),
    });
    let lines = segments_to_lines(&with_status_probe(probe, || render_v2_vm(&vm, 80)));
    let text = collect_text(&lines);
    assert!(
        text.contains("child content from probe"),
        "应从 probe.recent_messages 渲染子内容：{}",
        text
    );
}

#[test]
fn test_subagent_group_dto_view_models_takes_priority_over_probe() {
    // 当 DTO.view_models 非空时，应优先使用 DTO（ACP 层填充的真实子内容）
    // 而非 probe.recent_messages（v1 fallback）
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "fork".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "dto child".into(),
            content_hash: 0,
            reminder: None,
        })]),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let probe = std::rc::Rc::new(StaticProbe {
        info: Some(SubAgentRenderInfo {
            is_running: false,
            is_error: false,
            total_steps: 0,
            final_result: None,
            recent_messages: vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: "probe child (should not appear)".into(),
                content_hash: 0,
                reminder: None,
            })],
        }),
    });
    let lines = segments_to_lines(&with_status_probe(probe, || render_v2_vm(&vm, 80)));
    let text = collect_text(&lines);
    assert!(
        text.contains("dto child"),
        "应优先 DTO.view_models：{}",
        text
    );
    assert!(
        !text.contains("should not appear"),
        "probe.recent_messages 在 DTO 非空时应被忽略：{}",
        text
    );
}

#[test]
fn test_collapsed_group() {
    let vm = TuiRenderUnit::TuiCollapsedGroup(TuiCollapsedGroup {
        title: "3 searches".into(),
        count: 3,
        view_models: vec![],
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert_eq!(lines.len(), 1);
    let text = &lines[0].spans;
    assert!(text.iter().any(|s| s.content.contains("3 searches")));
}

#[test]
fn test_divider_with_label() {
    let vm = TuiRenderUnit::TuiDivider(TuiDivider {
        label: Some("Round 2".into()),
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert_eq!(lines.len(), 1);
}

#[test]
fn test_divider_no_label() {
    let vm = TuiRenderUnit::TuiDivider(TuiDivider {
        label: None,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert_eq!(lines.len(), 1);
}

#[test]
fn test_tool_card_write_running_collapsed() {
    // Write 运行中应折叠（FORCE_EXPAND_ON_COMPLETE + is_running → collapsed=true）
    RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-write-running".into(),
        tool_name: "Write".into(),
        input_summary: "path: foo.rs".into(),
        output_summary: "writing...".into(),
        is_error: false,
        is_running: true,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    // 折叠态只显示 1 行 output_summary
    let output_lines: Vec<_> = lines
        .iter()
        .filter(|l| {
            let t: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            t.contains("writing...")
        })
        .collect();
    assert_eq!(
        output_lines.len(),
        1,
        "Write 运行中折叠态应仅显示 1 行输出摘要：{}",
        text
    );
}

#[test]
fn test_tool_card_edit_running_collapsed() {
    // Edit 运行中应折叠（FORCE_EXPAND_ON_COMPLETE + is_running → collapsed=true）
    RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    let vm = TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: "tc-edit-running".into(),
        tool_name: "Edit".into(),
        input_summary: "path: foo.rs".into(),
        output_summary: "applying edit...".into(),
        is_error: false,
        is_running: true,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    let output_lines: Vec<_> = lines
        .iter()
        .filter(|l| {
            let t: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            t.contains("applying edit...")
        })
        .collect();
    assert_eq!(
        output_lines.len(),
        1,
        "Edit 运行中折叠态应仅显示 1 行输出摘要：{}",
        text
    );
}

#[test]
fn test_subagent_group_collapsed_summary_replaces_hard_truncation() {
    // 超过 5 个 TuiToolCard 时，前 N-5 个应显示为 "▶ N collapsed tools" 摘要
    let tool_cards: Vec<TuiRenderUnit> = (0..8)
        .map(|i| {
            TuiRenderUnit::TuiToolCard(TuiToolCard {
                tool_id: format!("tc-{}", i),
                tool_name: "Read".into(),
                input_summary: format!("file_{}.rs", i),
                output_summary: format!("{} lines", i),
                is_error: false,
                is_running: false,
                running_duration_ms: None,
                diff: None,
                tool_calls_count: 0,
                content_hash: 0,
            })
        })
        .collect();
    let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
        agent_id: "sa-collapse".into(),
        agent_name: "Agent".into(),
        view_models: im::Vector::from(tool_cards),
        collapsed: false,
        is_running: false,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    let text = collect_text(&lines);
    // 8 个 TuiToolCard，collapse_count = 3
    assert!(
        text.contains("\u{25b6} 3 collapsed tools"),
        "应显示折叠摘要行：{}",
        text
    );
    // 前 3 个 TuiToolCard 不应出现
    assert!(
        !text.contains("file_0.rs"),
        "被折叠的 TuiToolCard 不应渲染：{}",
        text
    );
    // 最后 5 个 TuiToolCard 应正常渲染
    assert!(
        text.contains("file_5.rs"),
        "最后 5 个 TuiToolCard 应正常渲染：{}",
        text
    );
}

#[test]
fn test_system_note_prefix_classification() {
    let vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "\u{273b} \u{5143}\u{4fe1}\u{606f}\u{884c}\n\u{23bf} \u{7ed3}\u{679c}\u{5f15}\u{7528}\u{884c}\n  \u{23bf} \u{9519}\u{8bef}\u{6458}\u{8981}\u{884c}\n\u{6b63}\u{5e38}\u{884c}\u{542b} \u{274c} \u{5173}\u{952e}\u{8bcd}\n\u{542b} warning \u{5173}\u{952e}\u{8bcd}\u{7684}\u{884c}\n\u{5176}\u{4f59}\u{666e}\u{901a}\u{884c}"
            .into(),
        level: TuiNoteLevel::Info,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert_eq!(lines.len(), 6, "6 行输入应产生 6 行输出");

    let text: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();

    // 第 1 行：✻ 前缀，内容无原始前缀
    assert!(
        text[0].starts_with("\u{273b}"),
        "\u{273b} 行应以 \u{273b} 前缀开头：{}",
        text[0]
    );
    // 不应再有旧的 · 前缀
    assert!(
        text.iter().all(|t| !t.contains("\u{b7} ")),
        "不应再使用旧的 \u{b7} 前缀：{:?}",
        text
    );
    // 第 4 行含 ❌ 应 error 色
    let error_color_line = &lines[3];
    let has_error = error_color_line.spans.iter().any(|s| {
        s.content.contains("\u{274c}")
            && s.style.fg == Some(THEME_ATOM.state().read().semantic.status.error)
    });
    assert!(has_error, "含 \u{274c} 的行应 error 色");
}

#[test]
fn test_system_note_prefix_no_double_space() {
    // L18：✻ / ⎿ / 缩进  ⎿ 前缀行渲染后内容前不应残留双空格
    let vm = TuiRenderUnit::TuiSystemNote(TuiSystemNote {
        text: "\u{273b} meta\n\u{23bf} result\n  \u{23bf} err".into(),
        level: TuiNoteLevel::Info,
        content_hash: 0,
    });
    let lines = segments_to_lines(&render_v2_vm(&vm, 80));
    assert_eq!(lines.len(), 3, "3 行输入应产生 3 行输出");

    // 拼接每行 span 内容，检查 prefix 与内容之间是否残留双空格
    let joined: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();

    // ✻ 前缀 span 是 "✻ "（含一个空格），内容首字符不应再是空格
    assert!(
        !joined[0].contains("\u{273b}  "),
        "\u{273b} 前缀后不应有双空格：{}",
        joined[0]
    );
    // ⎿ 前缀 span 是 "⎿ "，内容首字符不应再是空格
    assert!(
        !joined[1].contains("\u{23bf}  "),
        "\u{23bf} 前缀后不应有双空格：{}",
        joined[1]
    );
    // 缩进  ⎿ 前缀 span 是 "  ⎿ "，内容首字符不应再是空格
    assert!(
        !joined[2].contains("\u{23bf}  "),
        "缩进 \u{23bf} 前缀后不应有双空格：{}",
        joined[2]
    );
}

#[test]
fn test_tool_card_running_indicator_constant() {
    // L20：运行中 ToolCard 头部首 span 为白色 ●（而非空格）
    let mk = || {
        TuiRenderUnit::TuiToolCard(TuiToolCard {
            tool_id: "tc-run".into(),
            tool_name: "Bash".into(),
            input_summary: "echo hi".into(),
            output_summary: String::new(),
            is_error: false,
            is_running: true,
            running_duration_ms: None,
            diff: None,
            tool_calls_count: 0,
            content_hash: 0,
        })
    };

    // 连续调用两次（模拟批次重置），结果应一致
    for i in 0..2 {
        let lines = segments_to_lines(&render_v2_vm(&mk(), 80));
        assert!(!lines.is_empty(), "迭代 {} 应有输出", i);
        // render_tool_card 经 with_message_spacing 在头部插入空行，● 在 lines[1]
        assert!(
            lines.len() >= 2,
            "迭代 {} 应至少 2 行（空行 + 卡片首行）",
            i
        );
        let first_span = &lines[1].spans[0];
        assert_eq!(
            first_span.content, "\u{25cf}",
            "迭代 {} 运行中卡片首 span 应为 \u{25cf}",
            i
        );
        assert_eq!(
            first_span.style.fg,
            Some(ratatui::style::Color::White),
            "迭代 {} 运行中卡片首 span 应为白色",
            i
        );
    }
}

/// SubAgent 内 tool card：running 时保留 ⎿ 行，done 后过滤。
#[test]
fn test_subagent_tool_card_output_lines_by_state() {
    // running 状态：应保留 ⎿ 行
    let running_tool = TuiToolCard {
        tool_id: "tc-running".into(),
        tool_name: "Bash".into(),
        input_summary: "cargo test".into(),
        output_summary: String::new(),
        is_error: false,
        is_running: true,
        running_duration_ms: Some(5000),
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    };
    let running_group = TuiSubAgentGroup {
        agent_id: "test-running".into(),
        agent_name: "explore".into(),
        view_models: im::vector![TuiRenderUnit::TuiToolCard(running_tool)],
        collapsed: false,
        is_running: true,
        content_hash: 0,
    };
    let running_text = collect_text(&segments_to_lines(&render_v2_vm(
        &TuiRenderUnit::TuiSubAgentGroup(running_group),
        80,
    )));
    assert!(
        running_text.contains("\u{23bf} Running"),
        "running 时应有 \u{23bf} Running 行：{}",
        running_text
    );

    // done 状态：应过滤 ⎿ 行
    let done_tool = TuiToolCard {
        tool_id: "tc-done".into(),
        tool_name: "Grep".into(),
        input_summary: "pattern: TODO".into(),
        output_summary: "src/main.rs:42".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        diff: None,
        tool_calls_count: 0,
        content_hash: 0,
    };
    let done_group = TuiSubAgentGroup {
        agent_id: "test-done".into(),
        agent_name: "explore".into(),
        view_models: im::vector![TuiRenderUnit::TuiToolCard(done_tool)],
        collapsed: false,
        is_running: false,
        content_hash: 0,
    };
    let done_text = collect_text(&segments_to_lines(&render_v2_vm(
        &TuiRenderUnit::TuiSubAgentGroup(done_group),
        80,
    )));
    assert!(
        !done_text.contains('\u{23bf}'),
        "done 后不应有 \u{23bf} 行：{}",
        done_text
    );
    assert!(
        done_text.contains("\u{25cf} Grep (pattern: TODO)"),
        "done 后头行应正常显示：{}",
        done_text
    );
}
