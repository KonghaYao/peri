//! V2 ViewModel → ratatui Line 转换器。
//!
//! 纯函数 `render_v2_vm(vm, width, diff_visible) -> Vec<Line<'static>>`，
//! 处理全部 7 种 `peri_acp_types::view_model::ViewModel` 变体。
//! 零副作用，不持有缓存——markdown 每帧重新解析。

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use peri_acp_types::view_model::{
    CollapsedGroupData, DiffBlock, DividerData, HunkLineKind, NoteLevel, ReasoningBlock,
    SubAgentGroupData, ViewModel,
};

use crate::ui::theme;

// ── 公开入口 ──────────────────────────────────────────────────────────────

/// 将单个 V2 ViewModel 转换为 ratatui Line 列表。
///
/// * `width` — 终端可用宽度（当前未使用，为后续宽度感知预留）。
/// * `diff_visible` — 用户是否通过 `Ctrl+O` 展开了 diff 视图。
pub fn render_v2_vm(vm: &ViewModel, _width: usize, diff_visible: bool) -> Vec<Line<'static>> {
    match vm {
        ViewModel::UserBubble(data) => render_user_bubble(&data.text),
        ViewModel::AssistantBubble(data) => render_assistant_bubble(data),
        ViewModel::ToolCard(data) => render_tool_card(data, diff_visible),
        ViewModel::SystemNote(data) => render_system_note(data),
        ViewModel::SubAgentGroup(data) => render_subagent_group(data, _width, diff_visible),
        ViewModel::CollapsedGroup(data) => render_collapsed_group(data),
        ViewModel::Divider(data) => render_divider(data),
    }
}

// ── 各变体渲染 ────────────────────────────────────────────────────────────

fn render_user_bubble(text: &str) -> Vec<Line<'static>> {
    let user_bg = theme::USER_BG;
    let parsed = crate::ui::markdown::parse_markdown_default(text);
    let mut lines = Vec::with_capacity(parsed.lines.len() + 1);
    for (i, line) in parsed.lines.iter().enumerate() {
        if i == 0 {
            let mut spans = vec![Span::styled(
                "❯ ",
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::BOLD)
                    .bg(user_bg),
            )];
            for span in &line.spans {
                spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
            }
            lines.push(Line::from(spans));
        } else {
            let mut spans = vec![Span::styled("  ", Style::default().bg(user_bg))];
            for span in &line.spans {
                spans.push(span.clone().patch_style(Style::default().bg(user_bg)));
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn render_assistant_bubble(
    data: &peri_acp_types::view_model::AssistantBubbleData,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Reasoning block（如果存在）
    if let Some(ref reasoning) = data.reasoning {
        lines.extend(render_reasoning_block(reasoning));
    }

    // Text body（markdown 解析）
    if !data.text.is_empty() {
        let parsed = crate::ui::markdown::parse_markdown_default(&data.text);
        for line in &parsed.lines {
            lines.push(Line::from(line.spans.clone()));
        }
    }

    lines
}

fn render_reasoning_block(reasoning: &ReasoningBlock) -> Vec<Line<'static>> {
    let char_count = reasoning.text.chars().count();
    let mut lines = vec![Line::from(vec![Span::styled(
        format!("Thought for {} chars", char_count),
        Style::default().fg(theme::DIM),
    )])];

    // 尾部预览（最后 3 行）
    if !reasoning.collapsed {
        let tail_lines: Vec<&str> = reasoning.text.lines().rev().take(3).collect();
        for tail in tail_lines.into_iter().rev() {
            if !tail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" ⎿ ", Style::default().fg(theme::DIM)),
                    Span::styled(tail.to_string(), Style::default().fg(theme::DIM)),
                ]));
            }
        }
    }

    lines
}

fn render_tool_card(
    data: &peri_acp_types::view_model::ToolCardData,
    diff_visible: bool,
) -> Vec<Line<'static>> {
    let tool_color = if data.is_error {
        theme::ERROR
    } else {
        theme::SAGE
    };

    let indicator = if data.is_error { "✗" } else { "●" };

    let mut header_spans = vec![
        Span::styled(indicator.to_string(), Style::default().fg(tool_color)),
        Span::raw(" "),
        Span::styled(
            data.tool_name.clone(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if !data.input_summary.is_empty() {
        let summary = truncate_str(&data.input_summary, 400);
        header_spans.push(Span::styled(
            format!("({})", summary),
            Style::default().fg(theme::DIM),
        ));
    }

    let mut lines = vec![Line::from(header_spans)];

    // 输出摘要
    if !data.output_summary.is_empty() {
        let result_color = if data.is_error {
            theme::ERROR
        } else {
            theme::MUTED
        };
        let border_color = if data.is_error {
            theme::ERROR
        } else {
            theme::DIM
        };
        for out_line in data.output_summary.lines() {
            if !out_line.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("  ⎿ ".to_string(), Style::default().fg(border_color)),
                    Span::styled(out_line.to_string(), Style::default().fg(result_color)),
                ]));
            }
        }
    }

    // Diff 块
    if diff_visible {
        if let Some(ref diff) = data.diff {
            lines.extend(render_diff_block(diff));
        }
    }

    lines
}

fn render_diff_block(diff: &DiffBlock) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // File path header
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled(
            format!("--- a/{}", diff.path),
            Style::default().fg(theme::MUTED),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(theme::DIM)),
        Span::styled(
            format!("+++ b/{}", diff.path),
            Style::default().fg(theme::MUTED),
        ),
    ]));

    for hunk in &diff.hunks {
        // Hunk header
        lines.push(Line::from(vec![Span::styled(
            format!("  @@ -{} +{} @@", hunk.old_range, hunk.new_range),
            Style::default().fg(theme::THINKING),
        )]));

        for hunk_line in &hunk.lines {
            let (prefix, color) = match hunk_line.kind {
                HunkLineKind::Add => ("+", Color::Green),
                HunkLineKind::Del => ("-", Color::Red),
                HunkLineKind::Context => (" ", theme::MUTED),
            };
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(theme::DIM)),
                Span::styled(prefix.to_string(), Style::default().fg(color)),
                Span::styled(hunk_line.text.clone(), Style::default().fg(color)),
            ]));
        }
    }

    lines
}

fn render_system_note(data: &peri_acp_types::view_model::SystemNoteData) -> Vec<Line<'static>> {
    let color = match data.level {
        NoteLevel::Info => theme::MUTED,
        NoteLevel::Warning => theme::WARNING,
        NoteLevel::Error => theme::ERROR,
    };
    vec![Line::from(Span::styled(
        format!("· {}", &data.text),
        Style::default().fg(color),
    ))]
}

fn render_subagent_group(
    data: &SubAgentGroupData,
    width: usize,
    diff_visible: bool,
) -> Vec<Line<'static>> {
    let agent_color = theme::SAGE;
    let arrow_color = theme::LOADING;

    let mut lines = vec![Line::from(vec![
        Span::styled("❯ ", Style::default().fg(arrow_color)),
        Span::styled(
            "Agent".to_string(),
            Style::default()
                .fg(agent_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({})", data.agent_name),
            Style::default().fg(theme::MUTED),
        ),
    ])];

    if data.collapsed {
        let count = data.view_models.len();
        lines.push(Line::from(vec![Span::styled(
            format!("  {} items", count),
            Style::default().fg(theme::MUTED),
        )]));
    } else {
        for inner_vm in &data.view_models {
            let inner_lines = render_v2_vm(inner_vm, width, diff_visible);
            if inner_lines.is_empty() {
                continue;
            }
            for line in inner_lines {
                let mut new_spans = vec![Span::raw("  ")];
                new_spans.extend(line.spans);
                lines.push(Line::from(new_spans));
            }
        }
    }

    lines
}

fn render_collapsed_group(data: &CollapsedGroupData) -> Vec<Line<'static>> {
    vec![Line::from(vec![
        Span::styled("● ", Style::default().fg(theme::SAGE)),
        Span::styled(
            format!("{} ({} items)", data.title, data.count),
            Style::default().fg(theme::MUTED),
        ),
    ])]
}

fn render_divider(data: &DividerData) -> Vec<Line<'static>> {
    if let Some(ref label) = data.label {
        vec![Line::from(vec![
            Span::styled("── ", Style::default().fg(theme::DIM)),
            Span::styled(label.clone(), Style::default().fg(theme::MUTED)),
            Span::styled(" ──", Style::default().fg(theme::DIM)),
        ])]
    } else {
        vec![Line::from(vec![Span::styled(
            "───────────────",
            Style::default().fg(theme::DIM),
        )])]
    }
}

// ── 工具函数 ──────────────────────────────────────────────────────────────

/// 字符级截断（保证 CJK 安全）。
fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}…", truncated)
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::view_model::{
        AssistantBubbleData, Hunk, HunkLine, ToolCardData, UserBubbleData,
    };

    #[test]
    fn test_user_bubble_basic() {
        let vm = ViewModel::UserBubble(UserBubbleData {
            text: "hello world".into(),
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert!(
            !lines.is_empty(),
            "UserBubble should produce at least one line"
        );
    }

    #[test]
    fn test_assistant_bubble_text() {
        let vm = ViewModel::AssistantBubble(AssistantBubbleData {
            text: "**bold** text".into(),
            reasoning: None,
            tool_card_ids: vec![],
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_assistant_bubble_with_reasoning() {
        let vm = ViewModel::AssistantBubble(AssistantBubbleData {
            text: String::new(),
            reasoning: Some(ReasoningBlock {
                text: "thinking deeply...\nline 2\nline 3\nline 4".into(),
                collapsed: false,
            }),
            tool_card_ids: vec![],
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert!(!lines.is_empty());
        // Should have "Thought for N chars" line
        let first = &lines[0].spans;
        assert!(first.iter().any(|s| s.content.contains("Thought for")));
    }

    #[test]
    fn test_tool_card_success() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-1".into(),
            tool_name: "Read".into(),
            input_summary: "path: foo.rs".into(),
            output_summary: "3 lines".into(),
            is_error: false,
            diff: None,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert!(!lines.is_empty());
        let first = &lines[0].spans;
        assert!(first.iter().any(|s| s.content.contains("Read")));
    }

    #[test]
    fn test_tool_card_error() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-2".into(),
            tool_name: "Bash".into(),
            input_summary: "rm -rf /".into(),
            output_summary: "permission denied".into(),
            is_error: true,
            diff: None,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let first = &lines[0].spans;
        assert!(first.iter().any(|s| s.content.contains("✗")));
    }

    #[test]
    fn test_tool_card_diff() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-3".into(),
            tool_name: "Edit".into(),
            input_summary: "foo.rs".into(),
            output_summary: "ok".into(),
            is_error: false,
            diff: Some(DiffBlock {
                path: "foo.rs".into(),
                hunks: vec![Hunk {
                    old_range: "-1,3".into(),
                    new_range: "+1,4".into(),
                    lines: vec![HunkLine {
                        kind: HunkLineKind::Add,
                        text: "new line".into(),
                        old_no: None,
                        new_no: Some(4),
                    }],
                }],
            }),
        });
        // diff_visible = true
        let lines = render_v2_vm(&vm, 80, true);
        assert!(!lines.is_empty());
        // Should contain diff header
        let has_diff = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("+++")));
        assert!(
            has_diff,
            "should contain diff header when diff_visible=true"
        );
    }

    #[test]
    fn test_tool_card_diff_hidden() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-4".into(),
            tool_name: "Write".into(),
            input_summary: "bar.rs".into(),
            output_summary: "ok".into(),
            is_error: false,
            diff: Some(DiffBlock {
                path: "bar.rs".into(),
                hunks: vec![],
            }),
        });
        let lines = render_v2_vm(&vm, 80, false);
        let has_diff = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("+++")));
        assert!(!has_diff, "should NOT contain diff when diff_visible=false");
    }

    #[test]
    fn test_system_note_info() {
        let vm = ViewModel::SystemNote(peri_acp_types::view_model::SystemNoteData {
            text: "session started".into(),
            level: NoteLevel::Info,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_system_note_error() {
        let vm = ViewModel::SystemNote(peri_acp_types::view_model::SystemNoteData {
            text: "fatal error".into(),
            level: NoteLevel::Error,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_subagent_group_collapsed() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "sa-1".into(),
            agent_name: "file-searcher".into(),
            view_models: vec![ViewModel::UserBubble(UserBubbleData {
                text: "find foo".into(),
            })],
            collapsed: true,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_subagent_group_expanded() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "sa-2".into(),
            agent_name: "tester".into(),
            view_models: vec![ViewModel::UserBubble(UserBubbleData {
                text: "test".into(),
            })],
            collapsed: false,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_collapsed_group() {
        let vm = ViewModel::CollapsedGroup(CollapsedGroupData {
            title: "3 searches".into(),
            count: 3,
            view_models: vec![],
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
        let text = &lines[0].spans;
        assert!(text.iter().any(|s| s.content.contains("3 searches")));
    }

    #[test]
    fn test_divider_with_label() {
        let vm = ViewModel::Divider(DividerData {
            label: Some("Round 2".into()),
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_divider_no_label() {
        let vm = ViewModel::Divider(DividerData { label: None });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
    }
}
