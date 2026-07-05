//! V2 ViewModel → ratatui Line 转换器。
//!
//! 纯函数 `render_v2_vm(vm, width, diff_visible) -> Vec<Line<'static>>`，
//! 处理全部 7 种 `peri_acp_types::view_model::ViewModel` 变体。
//! 零副作用，不持有缓存——markdown 每帧重新解析。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::kit::tool_display;

use peri_acp_types::view_model::{
    AskUserBlockData, CollapsedGroupData, DiffBlock, DividerData, HunkLineKind, NoteLevel,
    ReasoningBlock, SubAgentGroupData, ViewModel,
};

use crate::kit::theme;

// ── SubAgent 运行时状态探针（thread-local） ─────────────────────────────────

/// V2 SubAgentGroup 渲染所需的运行时状态（用于显示状态 emoji + total_steps）。
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

thread_local! {
    /// 全局渲染调用计数器，用于跨递归边界的 yield 决策。
    /// 每次 render_v2_vm 入口递增 1；render_bridge::append_entries
    /// 每 N 次调用检查后 yield。在 append_entries 结束时重置为 0。
    pub(crate) static RENDER_CALL_COUNT: AtomicUsize = const { AtomicUsize::new(0) };
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
    RENDER_CALL_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
    match vm {
        ViewModel::UserBubble(data) => {
            render_user_bubble(&data.text, width, data.is_system_reminder)
        }
        ViewModel::AssistantBubble(data) => render_assistant_bubble(data, width),
        ViewModel::ToolCard(data) => render_tool_card(data, diff_visible),
        ViewModel::SystemNote(data) => render_system_note(data),
        ViewModel::SubAgentGroup(data) => render_subagent_group(data, width, diff_visible),
        ViewModel::CollapsedGroup(data) => render_collapsed_group(data),
        ViewModel::Divider(data) => render_divider(data),
        ViewModel::AskUserBlock(data) => render_ask_user_block(data),
    }
}

// ── 各变体渲染 ────────────────────────────────────────────────────────────

fn render_user_bubble(text: &str, width: usize, is_system_reminder: bool) -> Vec<Line<'static>> {
    // is_system_reminder: 仅渲染 "📋 Context compacted"（dim + ITALIC），无 ❯ 前缀，无底色
    if is_system_reminder {
        return vec![Line::from(Span::styled(
            "📋 Context compacted",
            Style::default()
                .fg(crate::kit::theme::semantic().text.dim)
                .add_modifier(Modifier::ITALIC),
        ))];
    }

    let semantic = theme::semantic();
    let component = theme::component();
    let user_bg = component.message.user_bg;
    let parsed = crate::kit::markdown::parse_markdown(text, width);
    let mut lines = Vec::with_capacity(parsed.lines.len() + 1);
    for (i, line) in parsed.lines.iter().enumerate() {
        if i == 0 {
            let mut spans = vec![Span::styled(
                "❯ ",
                Style::default()
                    .fg(semantic.border.active)
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
        for line in parsed.lines.iter() {
            lines.push(line.clone());
        }
    }

    lines
}

fn render_reasoning_block(reasoning: &ReasoningBlock) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    let component = theme::component();
    let char_count = reasoning.text.chars().count();
    let mut lines = vec![Line::from(vec![Span::styled(
        format!("🧠 已思考 {} 字符", char_count),
        Style::default().fg(component.message.reasoning),
    )])];

    // 尾部预览（最后 3 行）
    if !reasoning.collapsed {
        let tail_lines: Vec<&str> = reasoning.text.lines().rev().take(3).collect();
        for tail in tail_lines.into_iter().rev() {
            if !tail.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled(" ⎿ ", Style::default().fg(semantic.text.dim)),
                    Span::styled(tail.to_string(), Style::default().fg(semantic.text.dim)),
                ]));
            }
        }
    }

    lines
}

// ── 折叠/展开规则（TUI-PAGE.md §2.4.2） ────────────────────────────────

const COLLAPSED_BY_DEFAULT: &[&str] = &["Read", "Glob", "Grep", "AskUserQuestion"];
const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool"];
const FORCE_EXPAND_ON_COMPLETE: &[&str] = &["Write", "Edit"];

/// 工具调用卡片渲染（v2 ViewModel 渲染器）。
fn render_tool_card(
    data: &peri_acp_types::view_model::ToolCardData,
    diff_visible: bool,
) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    let display = tool_display(&data.tool_name, data.is_error, data.is_running);
    let display_name = tool_display::format_tool_name(&data.tool_name).to_string();

    let mut header_spans = vec![
        Span::styled(display.indicator, Style::default().fg(display.color)),
        Span::raw(" "),
        Span::styled(
            display_name,
            Style::default()
                .fg(display.color)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let summary = compact_summary(&data.input_summary, 140);
    if !summary.is_empty() {
        header_spans.push(Span::styled(
            format!(" — {}", summary),
            Style::default().fg(semantic.text.muted),
        ));
    }

    if data.is_running && !data.is_error {
        header_spans.push(Span::styled(
            " · ●",
            Style::default().fg(semantic.status.success),
        ));
    }

    let mut lines = vec![Line::from(header_spans)];

    // 折叠/展开判断（纯 UI 决策，对应 TUI-PAGE.md §2.4.2）
    let collapsed = if data.is_error {
        false // 错误不折叠
    } else if AUTO_EXPAND.contains(&data.tool_name.as_str()) {
        false // AgentResult/ExecuteExtraTool 自动展开
    } else if FORCE_EXPAND_ON_COMPLETE.contains(&data.tool_name.as_str()) && !data.is_running {
        false // Write/Edit 完成后强制展开
    } else {
        COLLAPSED_BY_DEFAULT.contains(&data.tool_name.as_str())
    };

    if collapsed {
        return lines;
    }

    // 输出摘要
    if !data.output_summary.is_empty() {
        let result_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let border_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.dim
        };
        for out_line in compact_output_lines(&data.output_summary, 4, 180) {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ".to_string(), Style::default().fg(border_color)),
                Span::styled(out_line, Style::default().fg(result_color)),
            ]));
        }
    }

    // Diff 块
    if let Some(ref diff) = data.diff {
        if diff_visible {
            lines.extend(render_diff_block(diff));
        } else {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                Span::styled(
                    format!("📝 diff: {} 已折叠 · Enter::open", diff.path),
                    Style::default().fg(semantic.text.dim),
                ),
            ]));
        }
    }

    lines
}

struct ToolDisplay {
    indicator: &'static str,
    color: Color,
}

fn tool_display(tool_name: &str, is_error: bool, is_running: bool) -> ToolDisplay {
    let semantic = theme::semantic();
    let component = theme::component();
    if is_error {
        return ToolDisplay {
            indicator: "✗",
            color: semantic.status.error,
        };
    }

    if is_running {
        // 800ms 闪烁：((RENDER_CALL_COUNT/16) % 2) == 0 显示 ●，否则空格
        // RENDER_CALL_COUNT 约每 50ms 递增一次，16 次 ≈ 800ms
        let visible =
            (RENDER_CALL_COUNT.with(|c| c.load(Ordering::Relaxed)) / 16).is_multiple_of(2);
        let indicator = if visible { "●" } else { " " };
        return ToolDisplay {
            indicator,
            color: semantic.status.success, // §2.4.2: Running 用 success 绿色
        };
    }

    let lower = tool_name.to_ascii_lowercase();
    let (indicator, color) = if lower.contains("bash") {
        ("$", semantic.status.warning)
    } else if lower.contains("edit") || lower.contains("write") {
        ("✎", semantic.border.active)
    } else if lower.contains("read") || lower.contains("glob") || lower.contains("grep") {
        ("⌕", semantic.status.success)
    } else if lower.contains("ask") || lower.contains("question") {
        ("?", semantic.status.warning)
    } else if lower.contains("todo") {
        ("☑", semantic.status.warning)
    } else if lower.contains("folder") {
        ("▣", semantic.status.success)
    } else if lower.contains("artifact") {
        ("◈", semantic.border.active)
    } else if lower.contains("cron") {
        ("◷", semantic.status.running)
    } else if lower.contains("agent") {
        ("◆", component.message.ai_prefix)
    } else if lower.contains("web") {
        ("◎", semantic.status.running)
    } else {
        ("●", component.message.tool_indicator)
    };

    ToolDisplay { indicator, color }
}

fn compact_summary(text: &str, max_chars: usize) -> String {
    let joined = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    truncate_str(&joined, max_chars)
}

fn compact_output_lines(text: &str, max_lines: usize, max_chars: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(max_lines)
        .map(|line| truncate_str(line, max_chars))
        .collect();

    let total = text.lines().filter(|line| !line.trim().is_empty()).count();
    if total > max_lines {
        lines.push(format!("… {} more lines", total - max_lines));
    }

    lines
}

fn render_diff_block(diff: &DiffBlock) -> Vec<Line<'static>> {
    let semantic = theme::semantic();

    // Early return: binary file
    if diff.is_binary {
        return vec![Line::from(Span::styled(
            format!("  Binary {} - cannot display diff", diff.path),
            Style::default().fg(semantic.text.dim),
        ))];
    }

    // Early return: diff too large
    if diff.is_too_large {
        return vec![Line::from(Span::styled(
            format!("  Diff too large for {} - changes not displayed", diff.path),
            Style::default().fg(semantic.text.dim),
        ))];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();

    // File path header
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(semantic.text.dim)),
        Span::styled(
            format!("--- a/{}", diff.path),
            Style::default().fg(semantic.text.muted),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default().fg(semantic.text.dim)),
        Span::styled(
            format!("+++ b/{}", diff.path),
            Style::default().fg(semantic.text.muted),
        ),
    ]));

    // New file cap: show at most 6 content lines
    let hunk_line_limit: usize = if diff.is_new_file { 6 } else { usize::MAX };
    let mut total_hunk_lines = 0usize;

    for hunk in &diff.hunks {
        // Hunk header
        lines.push(Line::from(vec![Span::styled(
            format!("  @@ -{} +{} @@", hunk.old_range, hunk.new_range),
            Style::default().fg(semantic.diff.hunk),
        )]));

        for hunk_line in &hunk.lines {
            if total_hunk_lines >= hunk_line_limit {
                break;
            }
            let (prefix, color, bg_color) = match hunk_line.kind {
                HunkLineKind::Add => ("+", semantic.diff.add, Some(semantic.diff.add_bg)),
                HunkLineKind::Del => ("-", semantic.diff.remove, Some(semantic.diff.remove_bg)),
                HunkLineKind::Context => (" ", semantic.text.muted, None),
            };
            let mut line_style = Style::default().fg(color);
            if let Some(bg) = bg_color {
                line_style = line_style.bg(bg);
            }
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default().fg(semantic.text.dim)),
                Span::styled(prefix.to_string(), line_style),
                Span::styled(hunk_line.text.clone(), line_style),
            ]));
            total_hunk_lines += 1;
        }

        if total_hunk_lines >= hunk_line_limit {
            let skipped = diff
                .hunks
                .iter()
                .map(|h| h.lines.len())
                .sum::<usize>()
                .saturating_sub(hunk_line_limit);
            if skipped > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  ... {} more lines not shown", skipped),
                    Style::default().fg(semantic.text.dim),
                )));
            }
            break;
        }
    }

    lines
}

fn render_system_note(data: &peri_acp_types::view_model::SystemNoteData) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    let mut lines: Vec<Line<'static>> = Vec::new();

    for line_text in data.text.lines() {
        if line_text.starts_with('✻') {
            // ✻ 开头行 — dim 色，无额外前缀
            lines.push(Line::from(Span::styled(
                line_text.to_string(),
                Style::default().fg(semantic.text.dim),
            )));
        } else if line_text.starts_with('⎿') {
            // ⎿ 开头行 — muted 色，无额外前缀
            lines.push(Line::from(Span::styled(
                line_text.to_string(),
                Style::default().fg(semantic.text.muted),
            )));
        } else if line_text.starts_with("  ⎿") {
            // 已缩进的 ⎿ — error 色（错误摘要行）
            lines.push(Line::from(Span::styled(
                line_text.to_string(),
                Style::default().fg(semantic.status.error),
            )));
        } else {
            // 其余行 — · 前缀 + 自动检测错误/警告
            let color = if line_text.contains("❌")
                || line_text.contains("失败")
                || line_text.to_lowercase().contains("error")
            {
                semantic.status.error
            } else if line_text.contains('⚠') || line_text.contains("已中断") {
                semantic.status.warning
            } else {
                semantic.text.muted
            };
            let prefix = Span::styled("· ", Style::default().fg(semantic.text.dim));
            let content = Span::styled(line_text.to_string(), Style::default().fg(color));
            lines.push(Line::from(vec![prefix, content]));
        }
    }

    lines
}

fn render_ask_user_block(data: &AskUserBlockData) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    let mut lines: Vec<Line<'static>> = Vec::new();

    let title_color = if data.is_error {
        semantic.status.error
    } else {
        semantic.status.success
    };
    lines.push(Line::from(Span::styled(
        "● User answered Peri's questions:",
        Style::default().fg(title_color),
    )));

    for item in &data.items {
        let prefix = Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim));
        let item_color = if data.is_error {
            semantic.status.error
        } else {
            semantic.text.muted
        };
        let content = Span::styled(
            format!("{} → {}", item.header, item.answer),
            Style::default().fg(item_color),
        );
        lines.push(Line::from(vec![prefix, content]));
    }

    lines
}

fn render_subagent_group(
    data: &SubAgentGroupData,
    width: usize,
    diff_visible: bool,
) -> Vec<Line<'static>> {
    let semantic = theme::semantic();

    // 查询运行时状态（v2 DTO 缺失字段由 status probe 注入）
    let status = lookup_subagent_status(&data.agent_id);

    // Agent 标签颜色：仅 error 态用红色。后台运行 warning 色依赖 is_background
    // 数据通道（后续迭代），当前所有非 error 态统一 success 绿色
    let agent_color = match status {
        Some(ref s) if s.is_error => semantic.status.error,
        _ => semantic.status.success,
    };

    let mut header_spans = vec![
        Span::styled("❯ ", Style::default().fg(semantic.loading)),
        Span::styled(
            "Agent".to_string(),
            Style::default()
                .fg(agent_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({})", data.agent_name),
            Style::default().fg(semantic.text.muted),
        ),
    ];

    // 运行时状态指示器
    if let Some(ref s) = status {
        if s.is_running {
            header_spans.push(Span::styled(
                " · ⏳",
                Style::default().fg(semantic.status.running),
            ));
            if s.total_steps > 0 {
                header_spans.push(Span::styled(
                    format!(" {} 步", s.total_steps),
                    Style::default().fg(semantic.text.muted),
                ));
            }
        } else if s.is_error {
            header_spans.push(Span::styled(
                " · ❌",
                Style::default().fg(semantic.status.error),
            ));
        } else {
            header_spans.push(Span::styled(
                " · ✅",
                Style::default().fg(semantic.status.success),
            ));
        }
    } else if data.is_running {
        header_spans.push(Span::styled(
            " · ⏳",
            Style::default().fg(semantic.status.running),
        ));
    } else if data.view_models.is_empty() {
        header_spans.push(Span::styled(
            " · ✅",
            Style::default().fg(semantic.status.success),
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
                format!("  📦 {} 项", count),
                Style::default().fg(semantic.text.muted),
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
    if let Some(ref s) = status
        && !s.is_running
        && let Some(ref result) = s.final_result
    {
        let preview: String = result
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        if !preview.is_empty() {
            let color = if s.is_error {
                semantic.status.error
            } else {
                semantic.text.muted
            };
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                Span::styled(preview, Style::default().fg(color)),
            ]));
        }
    }

    lines
}

fn render_collapsed_group(data: &CollapsedGroupData) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    vec![Line::from(vec![
        Span::styled("● ", Style::default().fg(semantic.status.success)),
        Span::styled(
            format!("{}（{} 项）", data.title, data.count),
            Style::default().fg(semantic.text.muted),
        ),
    ])]
}

fn render_divider(data: &DividerData) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    if let Some(ref label) = data.label {
        vec![Line::from(vec![
            Span::styled("── ", Style::default().fg(semantic.text.dim)),
            Span::styled(label.clone(), Style::default().fg(semantic.text.muted)),
            Span::styled(" ──", Style::default().fg(semantic.text.dim)),
        ])]
    } else {
        vec![Line::from(vec![Span::styled(
            "───────────────",
            Style::default().fg(semantic.text.dim),
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
            content_hash: 0,
            is_system_reminder: false,
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
            content_hash: 0,
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert!(!lines.is_empty());
        // Should have "Thought for N chars" line
        let first = &lines[0].spans;
        assert!(first.iter().any(|s| s.content.contains("🧠")));
    }

    #[test]
    fn test_tool_card_success() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-1".into(),
            tool_name: "Read".into(),
            input_summary: "path: foo.rs".into(),
            output_summary: "3 lines".into(),
            is_error: false,
            is_running: false,
            diff: None,
            content_hash: 0,
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
            is_running: false,
            diff: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let first = &lines[0].spans;
        assert!(first.iter().any(|s| s.content.contains("✗")));
    }

    #[test]
    fn test_tool_card_running_shows_status() {
        // 重置渲染计数器，确保 running 指示器可见（第 0 帧显示 ●）
        RENDER_CALL_COUNT.with(|c| c.store(0, Ordering::Relaxed));

        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-running".into(),
            tool_name: "Edit".into(),
            input_summary: "path: foo.rs\nold_string: hello".into(),
            output_summary: String::new(),
            is_error: false,
            is_running: true,
            diff: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(text.contains("●"), "运行中工具应显示状态 ●：{}", text);
        assert!(text.contains("· ●"), "运行中工具应显示运行中标记：{}", text);
        assert!(text.contains("path: foo.rs · old_string: hello"));
    }

    #[test]
    fn test_tool_card_output_is_compacted() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-output".into(),
            tool_name: "Bash".into(),
            input_summary: "cargo test".into(),
            output_summary: "line 1\nline 2\nline 3\nline 4\nline 5".into(),
            is_error: false,
            is_running: false,
            diff: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(text.contains("… 1 more lines"), "长输出应被压缩：{}", text);
    }

    #[test]
    fn test_tool_card_diff_hidden_shows_hint() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-diff-hint".into(),
            tool_name: "Write".into(),
            input_summary: "bar.rs".into(),
            output_summary: "ok".into(),
            is_error: false,
            is_running: false,
            diff: Some(DiffBlock {
                path: "bar.rs".into(),
                hunks: vec![],
                is_binary: false,
                is_too_large: false,
                is_new_file: false,
            }),
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(text.contains("已折叠"), "隐藏 diff 应提示可展开：{}", text);
    }

    #[test]
    fn test_tool_card_web_uses_distinct_indicator() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-web".into(),
            tool_name: "WebFetch".into(),
            input_summary: "https://example.com".into(),
            output_summary: "ok".into(),
            is_error: false,
            is_running: false,
            diff: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(text.contains("◎"), "Web 工具应有独立标识：{}", text);
    }

    #[test]
    fn test_tool_card_bash_uses_distinct_indicator() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-bash".into(),
            tool_name: "Bash".into(),
            input_summary: "cargo test".into(),
            output_summary: "ok".into(),
            is_error: false,
            is_running: false,
            diff: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(text.contains("$"), "Bash 工具应有独立标识：{}", text);
    }

    #[test]
    fn test_tool_card_diff() {
        let vm = ViewModel::ToolCard(ToolCardData {
            tool_id: "tc-3".into(),
            tool_name: "Edit".into(),
            input_summary: "foo.rs".into(),
            output_summary: "ok".into(),
            is_error: false,
            is_running: false,
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
                is_binary: false,
                is_too_large: false,
                is_new_file: false,
            }),
            content_hash: 0,
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
            is_running: false,
            diff: Some(DiffBlock {
                path: "bar.rs".into(),
                hunks: vec![],
                is_binary: false,
                is_too_large: false,
                is_new_file: false,
            }),
            content_hash: 0,
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_system_note_error() {
        let vm = ViewModel::SystemNote(peri_acp_types::view_model::SystemNoteData {
            text: "fatal error".into(),
            level: NoteLevel::Error,
            content_hash: 0,
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
                content_hash: 0,
                is_system_reminder: false,
            })],
            collapsed: true,
            is_running: false,
            content_hash: 0,
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
                content_hash: 0,
                is_system_reminder: false,
            })],
            collapsed: false,
            is_running: false,
            content_hash: 0,
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
    fn test_subagent_group_with_running_probe_shows_status_icon() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
        let text = collect_text(&lines);
        assert!(text.contains("⏳"), "应显示运行中状态：{}", text);
        assert!(text.contains("5 步"), "应显示步数：{}", text);
    }

    #[test]
    fn test_subagent_group_with_done_probe_shows_final_result() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
        let text = collect_text(&lines);
        assert!(text.contains("✅"), "应显示完成状态：{}", text);
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80, false));
        let text = collect_text(&lines);
        assert!(text.contains("❌"), "应显示失败状态：{}", text);
        assert!(text.contains("→ Error"), "应显示错误结果：{}", text);
    }

    #[test]
    fn test_subagent_group_without_probe_shows_success_icon_for_committed_placeholder() {
        // 不设置 probe → 已提交的 DTO placeholder 显示完成状态，避免历史消息看起来仍在运行。
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
            collapsed: false,
            is_running: false,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(text.contains("✅"), "无 probe 时应显示完成状态：{}", text);
    }

    #[test]
    fn test_subagent_group_streaming_dto_shows_running() {
        let vm = ViewModel::SubAgentGroup(SubAgentGroupData {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: Vec::new(),
            collapsed: false,
            is_running: true,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        let text = collect_text(&lines);
        assert!(text.contains("⏳"), "流式 DTO 应显示运行中状态：{}", text);
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
            is_running: false,
            content_hash: 0,
        });
        let probe = std::rc::Rc::new(StaticProbe {
            info: Some(SubAgentRenderInfo {
                is_running: true,
                is_error: false,
                total_steps: 1,
                final_result: None,
                recent_messages: vec![ViewModel::UserBubble(UserBubbleData {
                    text: "child content from probe".into(),
                    content_hash: 0,
                    is_system_reminder: false,
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
                content_hash: 0,
                is_system_reminder: false,
            })],
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
                recent_messages: vec![ViewModel::UserBubble(UserBubbleData {
                    text: "probe child (should not appear)".into(),
                    content_hash: 0,
                    is_system_reminder: false,
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
            content_hash: 0,
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_divider_no_label() {
        let vm = ViewModel::Divider(DividerData {
            label: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80, false);
        assert_eq!(lines.len(), 1);
    }
}
