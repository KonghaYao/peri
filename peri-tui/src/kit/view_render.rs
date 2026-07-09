//! V2 TuiRenderUnit → ratatui Line 转换器。
//!
//! 纯函数 `render_v2_vm(vm, width) -> Vec<Line<'static>>`，
//! 处理全部 7 种 `crate::kit::tui_render_unit::TuiRenderUnit` 变体。
//! 零副作用，不持有缓存——markdown 每帧重新解析。

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::kit::tool_display;

#[allow(unused_imports)]
use crate::kit::tui_render_unit::{
    TuiAskUserBlock, TuiCollapsedGroup, TuiDiffBlock, TuiDivider, TuiHunkLineKind, TuiNoteLevel,
    TuiReasoningBlock, TuiRenderUnit, TuiSubAgentGroup,
};

use crate::kit::theme;

// ── SubAgent 运行时状态探针（thread-local） ─────────────────────────────────

/// V2 TuiSubAgentGroup 渲染所需的运行时状态（用于显示状态 emoji + total_steps）。
///
/// 由 app 层通过 [`with_status_probe`] 注入；render_subagent_group 通过
/// agent_id 查询。对应 v2 DTO `TuiSubAgentGroup` 缺失的运行时字段。
#[derive(Clone, Debug, Default)]
pub struct SubAgentRenderInfo {
    pub is_running: bool,
    pub is_error: bool,
    pub total_steps: usize,
    pub final_result: Option<String>,
    /// 子 Agent 的最近消息（v2 TuiRenderUnit 形式）。
    ///
    /// 当 v2 DTO `TuiSubAgentGroup.view_models` 为空（ACP 层 view_mapper
    /// 生成的 placeholder）时，渲染层从此字段取子内容。app 层通过
    /// 通过 `subagent_status` 状态 probe 把 SubAgent 运行时状态转换为 v2 VMs
    /// 后填充此字段。
    pub recent_messages: Vec<TuiRenderUnit>,
}

/// V2 TuiSubAgentGroup 状态查询接口。app 层实现并通过 [`with_status_probe`] 设置。
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

/// 将单个 V2 TuiRenderUnit 转换为 ratatui Line 列表。
///
/// * `width` — 终端可用宽度，用于 markdown 解析时折行。
pub fn render_v2_vm(vm: &TuiRenderUnit, width: usize) -> Vec<Line<'static>> {
    RENDER_CALL_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
    match vm {
        TuiRenderUnit::TuiUserBubble(data) => {
            render_user_bubble(&data.text, width, data.is_system_reminder)
        }
        TuiRenderUnit::TuiAssistantBubble(data) => render_assistant_bubble(data, width),
        TuiRenderUnit::TuiToolCard(data) => render_tool_card(data),
        TuiRenderUnit::TuiSystemNote(data) => render_system_note(data),
        TuiRenderUnit::TuiSubAgentGroup(data) => render_subagent_group(data, width),
        TuiRenderUnit::TuiCollapsedGroup(data) => render_collapsed_group(data),
        TuiRenderUnit::TuiDivider(data) => render_divider(data),
        TuiRenderUnit::TuiAskUserBlock(data) => render_ask_user_block(data),
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
    lines.push(Line::from(""));
    for (i, line) in parsed.lines.iter().enumerate() {
        if i == 0 {
            let mut spans = vec![Span::styled(
                "❯ ",
                Style::default()
                    .fg(semantic.accent)
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
    data: &crate::kit::tui_render_unit::TuiAssistantBubble,
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

fn render_reasoning_block(reasoning: &TuiReasoningBlock) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    let char_count = reasoning.text.chars().count();
    let mut lines = vec![Line::from(vec![Span::styled(
        format!("Thought for {} chars", char_count),
        Style::default().fg(semantic.text.dim),
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

const COLLAPSED_BY_DEFAULT: &[&str] = &["Bash", "Read", "Glob", "Grep", "AskUserQuestion"];
const AUTO_EXPAND: &[&str] = &["AgentResult", "ExecuteExtraTool", "SearchExtraTools"];
const FORCE_EXPAND_ON_COMPLETE: &[&str] = &["Write", "Edit"];

/// 工具调用卡片渲染（v2 TuiRenderUnit 渲染器）。
fn render_tool_card(data: &crate::kit::tui_render_unit::TuiToolCard) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    let display = tool_display(&data.tool_name, data.is_error, data.is_running);
    let display_name = tool_display::format_tool_name(&data.tool_name).to_string();

    let mut header_spans = vec![
        Span::styled(display.indicator, Style::default().fg(display.color)),
        Span::raw(" "),
        Span::styled(
            display_name,
            Style::default()
                .fg(semantic.text.primary)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let summary = compact_summary(&data.input_summary, 400);
    if !summary.is_empty() {
        header_spans.push(Span::styled(
            format!(" ({})", summary),
            Style::default().fg(semantic.text.dim),
        ));
    }

    let mut lines = vec![Line::from(header_spans)];

    if data.tool_name == "Bash" && data.is_running && !data.is_error {
        let duration = data.running_duration_ms.unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                format!("Running ({})", format_running_duration(duration)),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }

    // 折叠/展开判断（纯 UI 决策，对应 TUI-TOOLCALL.md §1.3）
    let collapsed = if data.is_error {
        false // 错误不折叠
    } else if AUTO_EXPAND.contains(&data.tool_name.as_str()) {
        false // AgentResult/ExecuteExtraTool 自动展开
    } else if FORCE_EXPAND_ON_COMPLETE.contains(&data.tool_name.as_str()) {
        data.is_running // Write/Edit 运行中折叠，完成后展开
    } else {
        COLLAPSED_BY_DEFAULT.contains(&data.tool_name.as_str())
    };

    if collapsed {
        if !data.output_summary.is_empty() {
            let color = if data.is_error {
                semantic.status.error
            } else {
                semantic.text.muted
            };
            for out_line in compact_output_lines(&data.output_summary, 1, 400) {
                lines.push(Line::from(vec![
                    Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                    Span::styled(out_line, Style::default().fg(color)),
                ]));
            }
        }
        return with_message_spacing(lines);
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
        let max_lines = if data.tool_name == "TodoWrite" {
            usize::MAX
        } else {
            4
        };
        for out_line in compact_output_lines(&data.output_summary, max_lines, 400) {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ".to_string(), Style::default().fg(border_color)),
                Span::styled(out_line, Style::default().fg(result_color)),
            ]));
        }
    }

    // Diff 变更统计（Write/Edit）
    if let Some(ref diff) = data.diff {
        if let Some(summary) = diff_change_summary(diff) {
            lines.push(Line::from(vec![
                Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                Span::styled(summary, Style::default().fg(semantic.text.muted)),
            ]));
        }
    }

    with_message_spacing(lines)
}

fn format_running_duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}min", secs / 60)
    }
}

fn diff_change_summary(diff: &TuiDiffBlock) -> Option<String> {
    let adds = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Add))
        .count();
    let dels = diff
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, TuiHunkLineKind::Del))
        .count();
    if adds == 0 && dels == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if adds > 0 {
        parts.push(format!("+{}", adds));
    }
    if dels > 0 {
        parts.push(format!("-{}", dels));
    }
    Some(parts.join(" · "))
}

struct ToolDisplay {
    indicator: &'static str,
    color: Color,
}

fn tool_display(_tool_name: &str, is_error: bool, is_running: bool) -> ToolDisplay {
    let semantic = theme::semantic();
    if is_error {
        return ToolDisplay {
            indicator: "●",
            color: semantic.status.error,
        };
    }

    if is_running {
        // 运行中：常量白色 ●。原 RENDER_CALL_COUNT 闪烁逻辑失效（计数器每批次
        // 在 append_entries 末尾 reset 为 0，且 render 层禁止跨帧写 atom 状态）。
        // 运行态视觉信号由 Bash 卡片的 "Running (duration)" 行独立提供。
        return ToolDisplay {
            indicator: "●",
            color: Color::White,
        };
    }

    ToolDisplay {
        indicator: "●",
        color: semantic.status.success,
    }
}

fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
    while lines
        .last()
        .is_some_and(|line| line.spans.iter().all(|span| span.content.is_empty()))
    {
        lines.pop();
    }
}

fn with_message_spacing(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    trim_trailing_blank_lines(&mut lines);
    let mut spaced = Vec::with_capacity(lines.len() + 1);
    spaced.push(Line::from(""));
    spaced.extend(lines);
    // 只加头部空行，尾部空行由下一条消息的头部空行提供——避免相邻消息间出现双空行
    spaced
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

fn render_system_note(data: &crate::kit::tui_render_unit::TuiSystemNote) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for line_text in data.text.lines() {
        let (prefix_str, color) = if line_text.starts_with('\u{273B}') {
            // ✻ 元信息前缀 → dim 色
            ("✻ ", semantic.text.dim)
        } else if line_text.starts_with("⎿") {
            // 行首 ⎿（无缩进）→ muted 色
            ("⎿ ", semantic.text.muted)
        } else if line_text.starts_with("  ⎿") {
            // 缩进 ⎿ → error 色
            ("  ⎿ ", semantic.status.error)
        } else if line_text.contains('\u{274C}')
            || line_text.contains("失败")
            || line_text.contains("error")
        {
            // 含 ❌/失败/error 关键词 → error 色，无前缀
            ("", semantic.status.error)
        } else if line_text.contains("warning") || line_text.contains("warn") {
            // 含 warning/warn 关键词 → warning 色，无前缀
            ("", semantic.status.warning)
        } else {
            // 其余行 → muted 色，无前缀
            ("", semantic.text.muted)
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        // 跳过已消费的前缀字符
        let content_text = if prefix_str.contains('\u{273B}') {
            spans.push(Span::styled(
                "✻ ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix('\u{273B}')
                .unwrap_or(line_text)
                .trim_start()
        } else if prefix_str.contains("⎿") && prefix_str.starts_with("  ") {
            spans.push(Span::styled(
                "  ⎿ ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix("  ⎿")
                .unwrap_or(line_text)
                .trim_start()
        } else if prefix_str.contains("⎿") {
            spans.push(Span::styled(
                "⎿ ".to_string(),
                Style::default().fg(semantic.text.dim),
            ));
            line_text
                .strip_prefix("⎿")
                .unwrap_or(line_text)
                .trim_start()
        } else {
            line_text
        };
        if !content_text.is_empty() {
            spans.push(Span::styled(
                content_text.to_string(),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn render_ask_user_block(data: &TuiAskUserBlock) -> Vec<Line<'static>> {
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

fn render_subagent_group(data: &TuiSubAgentGroup, width: usize) -> Vec<Line<'static>> {
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
            format!("Agent({})", data.agent_id),
            Style::default()
                .fg(agent_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let task_preview = truncate_str(&data.agent_name, 50);
    if !task_preview.is_empty() {
        header_spans.push(Span::styled(
            format!(" {}", task_preview),
            Style::default().fg(semantic.text.muted),
        ));
    }

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
    let children: Vec<TuiRenderUnit> = if !data.view_models.is_empty() {
        data.view_models.iter().cloned().collect()
    } else if let Some(ref s) = status {
        s.recent_messages.clone()
    } else {
        Vec::new()
    };

    // 折叠摘要：TuiToolCard 超过 5 个时，前 N-5 个渲染为单行 "▶ N collapsed tools"，
    // 最后 5 个正常渲染。非 TuiToolCard 子消息始终正常渲染。
    let tool_count = children
        .iter()
        .filter(|vm| matches!(vm, TuiRenderUnit::TuiToolCard(_)))
        .count();
    let collapse_count = tool_count.saturating_sub(5);
    let mut tool_idx = 0;

    if collapse_count > 0 {
        lines.push(Line::from(vec![
            Span::styled("  ▶ ", Style::default().fg(semantic.text.dim)),
            Span::styled(
                format!("{} collapsed tools", collapse_count),
                Style::default().fg(semantic.text.muted),
            ),
        ]));
    }

    for inner_vm in &children {
        if matches!(inner_vm, TuiRenderUnit::TuiAssistantBubble(_)) {
            continue;
        }
        if matches!(inner_vm, TuiRenderUnit::TuiToolCard(_)) {
            tool_idx += 1;
            // 跳过被折叠的前 N-5 个 TuiToolCard
            if tool_idx <= collapse_count {
                continue;
            }
        }
        let inner_lines = render_v2_vm(inner_vm, width);
        // 运行中的 SubAgent：TuiToolCard 去掉输出行，单行显示
        let is_running = status.as_ref().map_or(data.is_running, |s| s.is_running);
        let inner_lines: Vec<_> = if is_running && matches!(inner_vm, TuiRenderUnit::TuiToolCard(_))
        {
            inner_lines
                .into_iter()
                .filter(|l| {
                    let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                    !text.contains("⎿")
                })
                .collect()
        } else {
            inner_lines
        };
        if inner_lines.is_empty() {
            continue;
        }
        // SubAgent 展开区内移除嵌套消息的 leading/trailing 空行
        // （render_v2_vm 对 TuiToolCard 等会调用 with_message_spacing 包裹空行）
        let start = inner_lines
            .iter()
            .position(|l| !l.spans.is_empty())
            .unwrap_or(0);
        let end = inner_lines
            .iter()
            .rposition(|l| !l.spans.is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        let trimmed = &inner_lines[start..end];
        for line in trimmed {
            let mut new_spans = vec![Span::raw("  ")];
            new_spans.extend(line.spans.iter().cloned());
            lines.push(Line::from(new_spans));
        }
    }

    // 显示 final_result 摘要（如果完成且有结果，最多前 3 行）
    if let Some(ref s) = status {
        if !s.is_running {
            if let Some(ref result) = s.final_result {
                let color = if s.is_error {
                    semantic.status.error
                } else {
                    semantic.text.muted
                };
                let preview_lines: Vec<&str> = result
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .collect();
                for line_text in preview_lines {
                    let truncated: String = line_text.chars().take(80).collect();
                    if !truncated.is_empty() {
                        lines.push(Line::from(vec![
                            Span::styled("  ⎿ ", Style::default().fg(semantic.text.dim)),
                            Span::styled(truncated, Style::default().fg(color)),
                        ]));
                    }
                }
            }
        }
    }

    with_message_spacing(lines)
}

fn render_collapsed_group(data: &TuiCollapsedGroup) -> Vec<Line<'static>> {
    let semantic = theme::semantic();
    vec![Line::from(vec![
        Span::styled("● ", Style::default().fg(semantic.status.success)),
        Span::styled(
            format!("{}（{} 项）", data.title, data.count),
            Style::default().fg(semantic.text.muted),
        ),
    ])]
}

fn render_divider(data: &TuiDivider) -> Vec<Line<'static>> {
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
    use crate::kit::tui_render_unit::{
        TuiAssistantBubble, TuiHunk, TuiHunkLine, TuiToolCard, TuiUserBubble,
    };

    #[test]
    fn test_user_bubble_basic() {
        let vm = TuiRenderUnit::TuiUserBubble(TuiUserBubble {
            text: "hello world".into(),
            content_hash: 0,
            is_system_reminder: false,
        });
        let lines = render_v2_vm(&vm, 80);
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
            is_system_reminder: false,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert_eq!(
            lines.len(),
            2,
            "用户消息应 1 行头部空行 + 1 行内容：{}",
            text
        );
        assert!(
            text.contains("❯ hello world"),
            "首行应使用 ❯ 前缀：{}",
            text
        );
        assert!(
            lines
                .first()
                .is_some_and(|line| collect_text(std::slice::from_ref(line)).is_empty())
        );
    }

    #[test]
    fn test_assistant_bubble_text() {
        let vm = TuiRenderUnit::TuiAssistantBubble(TuiAssistantBubble {
            text: "**bold** text".into(),
            reasoning: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
        let lines = render_v2_vm(&vm, 80);
        assert!(!lines.is_empty());
        // Should have "Thought for N chars" line
        let first = &lines[0].spans;
        assert!(first.iter().any(|s| s.content.contains("Thought for")));
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("● Read (path: foo.rs)"),
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let first = &lines[1].spans;
        assert!(first.iter().any(|s| s.content.contains("●")));
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("● Read (foo.rs)"),
            "错误工具应显示失败标识（红色 ●）：{}",
            text
        );
        assert!(
            text.contains("⎿ permission denied"),
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(text.contains("●"), "运行中工具应显示状态 ●：{}", text);
        // 运行中状态仅通过前导白色 ● 表示（常量，不闪烁），不再追加尾部 · ●
        assert!(
            !text.contains("· ●"),
            "运行中工具不应显示尾部标记：{}",
            text
        );
        assert!(text.contains("Edit (path: foo.rs · old_string: hello"));
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("● Shell (cargo test)"),
            "Bash 应显示为 Shell：{}",
            text
        );
        assert!(
            text.contains("⎿ Running (1min)"),
            "运行中 Bash 应显示耗时行：{}",
            text
        );
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
            text.contains("… 2 more lines"),
            "完成态仍应压缩输出：{}",
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(text.contains("… 4 more lines"), "长输出应被压缩：{}", text);
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("12 lines changed"),
            "Write 工具应显示 output_summary：{}",
            text
        );
        assert!(
            !text.contains("已折叠"),
            "不应再显示 diff 折叠提示：{}",
            text
        );
        assert!(!text.contains("📝"), "不应再显示 diff 标记：{}", text);
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("● WebFetch"),
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("● Shell"),
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("… 2 more lines"),
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(
            text.contains("tool_1"),
            "SearchExtraTools 应自动展开显示完整结果：{}",
            text
        );
    }

    #[test]
    fn test_system_note_info() {
        let vm = TuiRenderUnit::TuiSystemNote(crate::kit::tui_render_unit::TuiSystemNote {
            text: "session started".into(),
            level: TuiNoteLevel::Info,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_system_note_error() {
        let vm = TuiRenderUnit::TuiSystemNote(crate::kit::tui_render_unit::TuiSystemNote {
            text: "fatal error".into(),
            level: TuiNoteLevel::Error,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
                is_system_reminder: false,
            })]),
            collapsed: true,
            is_running: false,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(text.contains("Agent(sa-1) file-searcher"));
        assert!(
            text.contains("find foo"),
            "SubAgent 不再折叠，内容始终可见：{}",
            text
        );
    }

    #[test]
    fn test_subagent_group_expanded() {
        let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
            agent_id: "sa-2".into(),
            agent_name: "tester".into(),
            view_models: im::Vector::from(vec![TuiRenderUnit::TuiUserBubble(TuiUserBubble {
                text: "test".into(),
                content_hash: 0,
                is_system_reminder: false,
            })]),
            collapsed: false,
            is_running: false,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
                    is_system_reminder: false,
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
        let text = collect_text(&lines);
        assert!(text.contains("⏳"), "应显示运行中状态：{}", text);
        assert!(text.contains("5 步"), "应显示步数：{}", text);
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
        let text = collect_text(&lines);
        assert!(text.contains("✅"), "应显示完成状态：{}", text);
        assert!(
            text.contains("⎿ completed task"),
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
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
        let text = collect_text(&lines);
        assert!(text.contains("❌"), "应显示失败状态：{}", text);
        assert!(text.contains("⎿ Error"), "应显示错误结果：{}", text);
    }

    #[test]
    fn test_subagent_group_without_probe_shows_success_icon_for_committed_placeholder() {
        // 不设置 probe → 已提交的 DTO placeholder 显示完成状态，避免历史消息看起来仍在运行。
        let vm = TuiRenderUnit::TuiSubAgentGroup(TuiSubAgentGroup {
            agent_id: "fork".into(),
            agent_name: "Agent".into(),
            view_models: im::Vector::new(),
            collapsed: false,
            is_running: false,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(text.contains("✅"), "无 probe 时应显示完成状态：{}", text);
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
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        assert!(text.contains("⏳"), "流式 DTO 应显示运行中状态：{}", text);
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
                    is_system_reminder: false,
                })],
            }),
        });
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
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
                is_system_reminder: false,
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
                    is_system_reminder: false,
                })],
            }),
        });
        let lines = with_status_probe(probe, || render_v2_vm(&vm, 80));
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
        let lines = render_v2_vm(&vm, 80);
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
        let lines = render_v2_vm(&vm, 80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_divider_no_label() {
        let vm = TuiRenderUnit::TuiDivider(TuiDivider {
            label: None,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
        let lines = render_v2_vm(&vm, 80);
        let text = collect_text(&lines);
        // 8 个 TuiToolCard，collapse_count = 3
        assert!(
            text.contains("▶ 3 collapsed tools"),
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
        let vm = TuiRenderUnit::TuiSystemNote(crate::kit::tui_render_unit::TuiSystemNote {
            text: "✻ 元信息行\n⎿ 结果引用行\n  ⎿ 错误摘要行\n正常行含 ❌ 关键词\n含 warning 关键词的行\n其余普通行"
                .into(),
            level: TuiNoteLevel::Info,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
        assert!(text[0].starts_with("✻"), "✻ 行应以 ✻ 前缀开头：{}", text[0]);
        // 不应再有旧的 · 前缀
        assert!(
            text.iter().all(|t| !t.contains("· ")),
            "不应再使用旧的 · 前缀：{:?}",
            text
        );
        // 第 4 行含 ❌ 应 error 色
        let error_color_line = &lines[3];
        let has_error = error_color_line.spans.iter().any(|s| {
            s.content.contains("❌") && s.style.fg == Some(theme::semantic().status.error)
        });
        assert!(has_error, "含 ❌ 的行应 error 色");
    }

    #[test]
    fn test_system_note_prefix_no_double_space() {
        // L18：✻ / ⎿ / 缩进  ⎿ 前缀行渲染后内容前不应残留双空格
        let vm = TuiRenderUnit::TuiSystemNote(crate::kit::tui_render_unit::TuiSystemNote {
            text: "✻ meta\n⎿ result\n  ⎿ err".into(),
            level: TuiNoteLevel::Info,
            content_hash: 0,
        });
        let lines = render_v2_vm(&vm, 80);
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
            !joined[0].contains("✻  "),
            "✻ 前缀后不应有双空格：{}",
            joined[0]
        );
        // ⎿ 前缀 span 是 "⎿ "，内容首字符不应再是空格
        assert!(
            !joined[1].contains("⎿  "),
            "⎿ 前缀后不应有双空格：{}",
            joined[1]
        );
        // 缩进  ⎿ 前缀 span 是 "  ⎿ "，内容首字符不应再是空格
        assert!(
            !joined[2].contains("⎿  "),
            "缩进 ⎿ 前缀后不应有双空格：{}",
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
                content_hash: 0,
            })
        };

        // 连续调用两次（模拟批次重置），结果应一致
        for i in 0..2 {
            let lines = render_v2_vm(&mk(), 80);
            assert!(!lines.is_empty(), "迭代 {} 应有输出", i);
            // render_tool_card 经 with_message_spacing 在头部插入空行，● 在 lines[1]
            assert!(
                lines.len() >= 2,
                "迭代 {} 应至少 2 行（空行 + 卡片首行）",
                i
            );
            let first_span = &lines[1].spans[0];
            assert_eq!(
                first_span.content, "●",
                "迭代 {} 运行中卡片首 span 应为 ●",
                i
            );
            assert_eq!(
                first_span.style.fg,
                Some(Color::White),
                "迭代 {} 运行中卡片首 span 应为白色",
                i
            );
        }
    }
}
