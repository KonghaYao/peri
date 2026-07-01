//! V2 ViewModel → ratatui Line 转换器。
//!
//! 纯函数 `render_v2_vm(vm, width, diff_visible) -> Vec<Line<'static>>`，
//! 处理全部 7 种 `peri_acp_types::view_model::ViewModel` 变体。
//! 零副作用，不持有缓存——markdown 每帧重新解析。

use std::cell::RefCell;
use std::rc::Rc;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use peri_acp_types::view_model::{
    CollapsedGroupData, DiffBlock, DividerData, HunkLineKind, NoteLevel, ReasoningBlock,
    SubAgentGroupData, ViewModel,
};

use crate::kit::theme;

// ── SubAgent 运行时状态探针（thread-local） ─────────────────────────────────

/// V2 SubAgentGroup 渲染所需的运行时状态（用于显示 running/done/failed + total_steps）。
///
/// 由 app 层通过 [`with_status_probe`] 注入；render_subagent_group 通过
/// agent_id 查询。对应 v2 DTO `SubAgentGroupData` 缺失的运行时字段。
#[derive(Clone, Debug, Default)]
pub struct SubAgentRenderInfo {
    pub is_running: bool,
    pub is_error: bool,
    pub total_steps: usize,
    pub final_result: Option<String>,
    /// 子 Agent 的最近消息（v2 ViewModel 形式）。
    ///
    /// 当 v2 DTO `SubAgentGroupData.view_models` 为空（ACP 层 view_mapper
    /// 生成的 placeholder）时，渲染层从此字段取子内容。app 层通过
    /// 通过 `subagent_status` 状态 probe 把 SubAgent 运行时状态转换为 v2 VMs
    /// 后填充此字段。
    pub recent_messages: Vec<ViewModel>,
}

/// V2 SubAgentGroup 状态查询接口。app 层实现并通过 [`with_status_probe`] 设置。
///
/// 实现者通常是 `SubAgentStatusMap` 的快照或借用包装。
pub trait SubAgentStatusProbe {
    fn lookup_by_agent_id(&self, agent_id: &str) -> Option<SubAgentRenderInfo>;
}

thread_local! {
    /// 当前线程的 status probe。draw_now 在调用 terminal.draw 前设置，
    /// render_subagent_group 通过 lookup_subagent_status 查询。
    static STATUS_PROBE: RefCell<Option<Rc<dyn SubAgentStatusProbe>>> = const { RefCell::new(None) };
}

/// 在 closure 内设置 status probe，closure 结束后自动恢复（支持嵌套）。
///
/// 典型用法：`draw_now` 中 `with_status_probe(probe, || self.terminal.draw(...))`。
pub fn with_status_probe<R>(probe: Rc<dyn SubAgentStatusProbe>, f: impl FnOnce() -> R) -> R {
    let prev = STATUS_PROBE.with(|cell| cell.replace(Some(probe)));
    let result = f();
    STATUS_PROBE.with(|cell| {
        let _ = cell.replace(prev);
    });
    result
}

/// render_subagent_group 内部使用：按 agent_id 查询运行时状态。
fn lookup_subagent_status(agent_id: &str) -> Option<SubAgentRenderInfo> {
    STATUS_PROBE.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|probe| probe.lookup_by_agent_id(agent_id))
    })
}

// ── 公开入口 ──────────────────────────────────────────────────────────────

/// 将单个 V2 ViewModel 转换为 ratatui Line 列表。
///
/// * `width` — 终端可用宽度，用于 markdown 解析时折行。
/// * `diff_visible` — 用户是否通过 `Ctrl+O` 展开了 diff 视图。
pub fn render_v2_vm(vm: &ViewModel, width: usize, diff_visible: bool) -> Vec<Line<'static>> {
    match vm {
        ViewModel::UserBubble(data) => render_user_bubble(&data.text, width),
        ViewModel::AssistantBubble(data) => render_assistant_bubble(data, width),
        ViewModel::ToolCard(data) => render_tool_card(data, diff_visible),
        ViewModel::SystemNote(data) => render_system_note(data),
        ViewModel::SubAgentGroup(data) => render_subagent_group(data, width, diff_visible),
        ViewModel::CollapsedGroup(data) => render_collapsed_group(data),
        ViewModel::Divider(data) => render_divider(data),
    }
}

// ── 各变体渲染 ────────────────────────────────────────────────────────────

fn render_user_bubble(text: &str, width: usize) -> Vec<Line<'static>> {
    let user_bg = theme::USER_BG;
    let parsed = crate::kit::markdown::parse_markdown(text, width);
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
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Reasoning block（如果存在）
    if let Some(ref reasoning) = data.reasoning {
        lines.extend(render_reasoning_block(reasoning));
    }

    // Text body（markdown 解析）
    if !data.text.is_empty() {
        let parsed = crate::kit::markdown::parse_markdown(&data.text, width);
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

    // 查询运行时状态（v2 DTO 缺失字段由 status probe 注入）
    let status = lookup_subagent_status(&data.agent_id);

    let mut header_spans = vec![
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
    ];

    // 运行时状态指示器
    if let Some(ref s) = status {
        if s.is_running {
            header_spans.push(Span::styled(
                " · running",
                Style::default().fg(theme::LOADING),
            ));
            if s.total_steps > 0 {
                header_spans.push(Span::styled(
                    format!(" · {} steps", s.total_steps),
                    Style::default().fg(theme::MUTED),
                ));
            }
        } else if s.is_error {
            header_spans.push(Span::styled(" · failed", Style::default().fg(theme::ERROR)));
        } else {
            header_spans.push(Span::styled(" · done", Style::default().fg(theme::SAGE)));
        }
    } else if data.view_models.is_empty() {
        // DTO placeholder（ACP 层 view_mapper 生成的空 SubAgentGroup），
        // 没有 status probe 或 probe 未命中 → 显示通用 running 提示
        header_spans.push(Span::styled(
            " · running...",
            Style::default().fg(theme::MUTED),
        ));
    }

    let mut lines = vec![Line::from(header_spans)];

    // 子内容来源优先级：
    // 1. v2 DTO `view_models`（ACP 层填充，当前永久为空 placeholder）
    // 2. status probe 的 `recent_messages`（app 层填充）
    let children: Vec<ViewModel> = if !data.view_models.is_empty() {
        data.view_models.clone()
    } else if let Some(ref s) = status {
        s.recent_messages.clone()
    } else {
        Vec::new()
    };

    if data.collapsed {
        let count = children.len();
        if count > 0 {
            lines.push(Line::from(vec![Span::styled(
                format!("  {} items", count),
                Style::default().fg(theme::MUTED),
            )]));
        }
    } else {
        for inner_vm in &children {
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

    // 显示 final_result 摘要（如果完成且有结果）
    if let Some(ref s) = status {
        if !s.is_running {
            if let Some(ref result) = s.final_result {
                let preview: String = result
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(120)
                    .collect();
                if !preview.is_empty() {
                    let color = if s.is_error {
                        theme::ERROR
                    } else {
                        theme::MUTED
                    };
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("→ {}", preview), Style::default().fg(color)),
                    ]));
                }
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

    /// 测试辅助：一个最小 SubAgentStatusProbe，返回固定 SubAgentRenderInfo。
    struct StaticProbe {
        info: Option<SubAgentRenderInfo>,
    }
    impl SubAgentStatusProbe for StaticProbe {
        fn lookup_by_agent_id(&self, _agent_id: &str) -> Option<SubAgentRenderInfo> {
            self.info.clone()
        }
    }

    fn collect_text(lines: &[Line]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.clone())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn test_subagent_group_with_running_probe_shows_running() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
            collapsed: false,
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
        let text = collect_text(&lines);
        assert!(text.contains("running"), "应显示 running：{}", text);
        assert!(text.contains("5 steps"), "应显示步数：{}", text);
    }

    #[test]
    fn test_subagent_group_with_done_probe_shows_final_result() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
            collapsed: false,
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
        let text = collect_text(&lines);
        assert!(text.contains("done"), "应显示 done：{}", text);
        assert!(
            text.contains("→ completed task"),
            "应显示结果预览：{}",
            text
        );
    }

    #[test]
    fn test_subagent_group_with_error_probe_shows_failed() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
            collapsed: false,
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
        let text = collect_text(&lines);
        assert!(text.contains("failed"), "应显示 failed：{}", text);
        assert!(text.contains("→ Error"), "应显示错误结果：{}", text);
    }

    #[test]
    fn test_subagent_group_without_probe_shows_running_hint() {
        // 不设置 probe → DTO placeholder 显示 "running..."（无 probe 命中）
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
            collapsed: false,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(
            text.contains("running"),
            "无 probe 时应显示 running 提示：{}",
            text
        );
    }

    #[test]
    fn test_subagent_group_falls_back_to_probe_recent_messages() {
        // DTO.view_models 为空 placeholder，但 probe 提供 recent_messages
        // → 渲染应回退到 probe 的子内容（Phase 2.6 桥接核心路径）
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(), // 空占位符
            collapsed: false,
        });
        let probe = std::rc::Rc::new(StaticProbe {
            info: Some(SubAgentRenderInfo {
                is_running: true,
                is_error: false,
                total_steps: 1,
                final_result: None,
                recent_messages: vec![ViewModel::UserBubble(UserBubbleData {
                    text: "child content from probe".into(),
                })],
            }),
        });
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
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
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: vec![ViewModel::UserBubble(UserBubbleData {
                text: "dto child".into(),
            })],
            collapsed: false,
        });
        let probe = std::rc::Rc::new(StaticProbe {
            info: Some(SubAgentRenderInfo {
                is_running: false,
                is_error: false,
                total_steps: 0,
                final_result: None,
                recent_messages: vec![ViewModel::UserBubble(UserBubbleData {
                    text: "probe child (should not appear)".into(),
                })],
            }),
        });
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
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
